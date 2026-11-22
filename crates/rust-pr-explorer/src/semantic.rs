//! Precise call resolution with rust-analyzer, used as a library.
//!
//! The syntactic analysis in [`crate::analyze`] cannot know the type of a
//! method call's receiver, nor what a macro does with its arguments, so it
//! leaves those call sites unresolved. Here rust-analyzer's type inference
//! answers "what does this function call?" for every function.
//!
//! By default no code from the analyzed project is executed: build scripts
//! are not run and procedural macros are not expanded. Only `cargo metadata`
//! is invoked, which may download (but never builds) dependency sources.
//! With [`Trust::Trusted`] (for sandboxed environments), build scripts run
//! and proc macros are expanded, so rust-analyzer also sees generated code.

use crate::analyze::{Analysis, LineCol};
use crate::source::Trust;
use anyhow::{Context, Result};
use model::{Call, Ref};
use ra_ap_ide::{
    AnalysisHost, CallHierarchyConfig, FilePosition, GotoDefinitionConfig, NavigationTarget,
    TextSize,
};
use ra_ap_ide_db::ra_fixture::RaFixtureConfig;
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_paths::AbsPathBuf;
use ra_ap_project_model::{CargoConfig, CargoFeatures, RustLibSource};
use ra_ap_vfs::{Vfs, VfsPath};
use std::collections::BTreeMap;
use std::collections::{BTreeSet, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Stats {
    /// Functions rust-analyzer answered for.
    pub functions: usize,
    /// Call sites whose target rust-analyzer determined.
    pub resolved_sites: usize,
    /// Call sites it could not resolve either.
    pub unresolved_sites: usize,
}

/// What a call site refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// A function or method of the analyzed crates.
    Item(String),
    /// Something else: a function of another crate, a constructor, a
    /// closure, … Known, but not an edge of the call graph.
    Other,
}

/// Replaces each function's calls with what rust-analyzer's type inference
/// says. Every call site found by the syntactic pass is checked, including
/// method calls and calls inside macro invocations; sites it can't resolve
/// are counted as unresolved instead of guessed.
///
/// `manifest_dirs` are the directories whose `Cargo.toml` should be
/// loaded; each is loaded as a cargo workspace.
pub fn refine(
    root: &Path,
    manifest_dirs: &[PathBuf],
    trust: Trust,
    analysis: &mut Analysis,
) -> Result<Stats> {
    let root = std::fs::canonicalize(root)?;
    let mut stats = Stats::default();
    let mut texts = Texts::default();
    // Where each analyzed item's name is, to map results back to items.
    let mut by_name_pos: HashMap<(String, LineCol), String> = HashMap::new();
    for (id, item) in &analysis.items {
        if let Some(pos) = item.name_pos {
            by_name_pos.insert((item.info.file.clone(), pos), id.clone());
        }
    }
    let mut done: BTreeSet<String> = BTreeSet::new();
    for dir in manifest_dirs {
        let dir = std::fs::canonicalize(dir)?;
        let (host, vfs) =
            load(&dir, trust).with_context(|| format!("loading {}", dir.display()))?;
        let ra = host.analysis();
        let call_config = CallHierarchyConfig {
            exclude_tests: true,
            ra_fixture: RaFixtureConfig::default(),
        };
        let goto_config = GotoDefinitionConfig {
            ra_fixture: RaFixtureConfig::default(),
        };
        let ids: Vec<String> = analysis
            .items
            .iter()
            .filter(|(id, item)| item.name_pos.is_some() && !done.contains(*id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let item = &analysis.items[&id];
            let rel = item.info.file.clone();
            let Some(file_id) = file_id(&vfs, &root.join(&rel)) else {
                continue;
            };
            let Some(text) = texts.get(&root, &rel).map(str::to_string) else {
                continue;
            };
            let position = |pos: LineCol| FilePosition {
                file_id,
                offset: TextSize::from(to_offset(&text, pos) as u32),
            };
            let result = catch_unwind(AssertUnwindSafe(|| {
                ra.outgoing_calls(&call_config, position(item.name_pos.unwrap()))
            }));
            let Ok(Ok(Some(calls))) = result else {
                continue;
            };
            done.insert(id.clone());
            stats.functions += 1;

            // Where each call goes, by the range of its call site.
            let mut ranges: Vec<(LineCol, LineCol, Target)> = Vec::new();
            let mut targets: BTreeSet<String> = BTreeSet::new();
            for call in calls {
                let target = target_of(&vfs, &root, &mut texts, &by_name_pos, &call.target);
                if let Target::Item(t) = &target {
                    targets.insert(t.clone());
                }
                for range in call.ranges {
                    if range.file_id == file_id {
                        let start = to_linecol(&text, range.range.start().into());
                        let end = to_linecol(&text, range.range.end().into());
                        ranges.push((start, end, target.clone()));
                    }
                }
            }
            // Check every syntactic call site; ask "go to definition" for
            // those the call hierarchy doesn't list (e.g. inside macros).
            let sites = item.call_sites.clone();
            let mut site_refs = BTreeMap::new();
            let mut unresolved = 0;
            for site in &sites {
                let listed = ranges
                    .iter()
                    .find(|(start, end, _)| *start <= site.pos && site.pos < *end)
                    .map(|(.., t)| t.clone());
                let target = listed.or_else(|| {
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        ra.goto_definition(position(site.pos), &goto_config)
                    }));
                    let Ok(Ok(Some(info))) = result else {
                        return None;
                    };
                    let found: Vec<Target> = info
                        .info
                        .iter()
                        .map(|nav| target_of(&vfs, &root, &mut texts, &by_name_pos, nav))
                        .collect();
                    // Several definitions (e.g. ambiguous macro code): unknown.
                    (found.len() == 1).then(|| found[0].clone())
                });
                match target {
                    Some(Target::Item(t)) => {
                        stats.resolved_sites += 1;
                        let callable = analysis
                            .items
                            .get(&t)
                            .is_some_and(|i| i.info.kind.is_callable());
                        if callable {
                            if site.method {
                                site_refs.insert(
                                    format!("{}:{}", site.pos.line, site.pos.column),
                                    Ref {
                                        id: Some(t.clone()),
                                        path: t.clone(),
                                    },
                                );
                            }
                            targets.insert(t);
                        }
                    }
                    Some(Target::Other) => stats.resolved_sites += 1,
                    None => {
                        stats.unresolved_sites += 1;
                        unresolved += 1;
                    }
                }
            }
            targets.remove(&id);
            let item = analysis.items.get_mut(&id).unwrap();
            item.info.calls = targets.into_iter().map(|to| Call { to }).collect();
            item.info.unresolved_calls = unresolved;
            item.info.site_refs = site_refs;
            // A function called is no longer only "used".
            let calls = item.info.calls.clone();
            item.info
                .uses
                .retain(|u| !calls.iter().any(|c| c.to == u.to));
        }
    }
    Ok(stats)
}

/// The analyzed item a navigation target points at, by its name position.
fn target_of(
    vfs: &Vfs,
    root: &Path,
    texts: &mut Texts,
    by_name_pos: &HashMap<(String, LineCol), String>,
    nav: &NavigationTarget,
) -> Target {
    let Some(rel) = vfs
        .file_path(nav.file_id)
        .as_path()
        .and_then(|p| Path::new(p.as_str()).strip_prefix(root).ok())
        .map(|p| p.display().to_string())
    else {
        return Target::Other;
    };
    let start = nav.focus_range.unwrap_or(nav.full_range).start();
    let Some(text) = texts.get(root, &rel) else {
        return Target::Other;
    };
    let pos = to_linecol(text, start.into());
    match by_name_pos.get(&(rel, pos)) {
        Some(id) => Target::Item(id.clone()),
        None => Target::Other,
    }
}

fn load(dir: &Path, trust: Trust) -> Result<(AnalysisHost, Vfs)> {
    let cargo = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        // The syntactic analysis sees code behind every `cfg(feature)`;
        // enable all features so that rust-analyzer does too.
        features: CargoFeatures::All,
        ..Default::default()
    };
    let trusted = trust == Trust::Trusted;
    let load = LoadCargoConfig {
        // Build scripts and proc macros are the project's own code: run
        // them only when the environment is trusted (sandboxed).
        load_out_dirs_from_check: trusted,
        with_proc_macro_server: if trusted {
            ProcMacroServerChoice::Sysroot
        } else {
            ProcMacroServerChoice::None
        },
        prefill_caches: false,
        num_worker_threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        proc_macro_processes: usize::from(trusted),
    };
    let (db, vfs, _) = load_workspace_at(dir, &cargo, &load, &|_| {})?;
    Ok((AnalysisHost::with_database(db), vfs))
}

fn file_id(vfs: &Vfs, path: &Path) -> Option<ra_ap_vfs::FileId> {
    let abs = AbsPathBuf::try_from(path.to_str()?).ok()?;
    vfs.file_id(&VfsPath::from(abs)).map(|(id, _)| id)
}

/// File contents by path relative to the root, read on demand.
#[derive(Default)]
struct Texts(HashMap<String, Option<String>>);

impl Texts {
    fn get(&mut self, root: &Path, rel: &str) -> Option<&str> {
        self.0
            .entry(rel.to_string())
            .or_insert_with(|| std::fs::read_to_string(root.join(rel)).ok())
            .as_deref()
    }
}

fn to_offset(text: &str, pos: LineCol) -> usize {
    let line_start: usize = text
        .split_inclusive('\n')
        .take(pos.line.saturating_sub(1))
        .map(str::len)
        .sum();
    let line = &text[line_start..];
    line_start
        + line
            .char_indices()
            .nth(pos.column)
            .map_or(line.len(), |(i, _)| i)
}

fn to_linecol(text: &str, offset: u32) -> LineCol {
    let offset = (offset as usize).min(text.len());
    let before = &text[..offset];
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    LineCol {
        line: before.matches('\n').count() + 1,
        column: before[line_start..].chars().count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_round_trip() {
        let text = "fn a() {}\n// é\nfn bé() { x.y() }\n";
        for offset in [0, 3, 10, 18, 25, 28] {
            let lc = to_linecol(text, offset);
            assert_eq!(to_offset(text, lc), offset as usize, "{lc:?}");
        }
        assert_eq!(to_linecol(text, 18), LineCol { line: 3, column: 2 });
    }
}
