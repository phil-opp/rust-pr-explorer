//! Precise call resolution with rust-analyzer, used as a library.
//!
//! The syntactic analysis in [`crate::analyze`] cannot know the type of a
//! method call's receiver, so it guesses targets from method names. Here
//! rust-analyzer's type inference answers "what does this function call?"
//! for every function, and its answers replace the guesses.
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
use ra_ap_ide::{AnalysisHost, CallHierarchyConfig, FilePosition, TextSize};
use ra_ap_ide_db::ra_fixture::RaFixtureConfig;
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_paths::AbsPathBuf;
use ra_ap_project_model::{CargoConfig, CargoFeatures, RustLibSource};
use ra_ap_vfs::{Vfs, VfsPath};
use std::collections::{BTreeSet, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Stats {
    /// Functions rust-analyzer answered for.
    pub functions: usize,
    /// Guessed method calls that were confirmed, corrected or dropped.
    pub resolved_guesses: usize,
    /// Calls found only by rust-analyzer.
    pub new_calls: usize,
}

/// Replaces guessed method-call targets with rust-analyzer's results.
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
    // Where each analyzed function's name is, to map results back to items.
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
        let config = CallHierarchyConfig {
            exclude_tests: true,
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
            let offset = to_offset(&text, item.name_pos.unwrap());
            let position = FilePosition {
                file_id,
                offset: TextSize::from(offset as u32),
            };
            let result = catch_unwind(AssertUnwindSafe(|| ra.outgoing_calls(&config, position)));
            let Ok(Ok(Some(calls))) = result else {
                continue;
            };
            done.insert(id.clone());
            stats.functions += 1;

            // Call sites (by position) and the local item each one calls, if any.
            let mut sites: HashMap<LineCol, (String, Option<String>)> = HashMap::new();
            for call in calls {
                let target = call.target;
                let target_id = vfs
                    .file_path(target.file_id)
                    .as_path()
                    .and_then(|p| Path::new(p.as_str()).strip_prefix(&root).ok())
                    .map(|p| p.display().to_string())
                    .and_then(|target_rel| {
                        let start = target.focus_range.unwrap_or(target.full_range).start();
                        let pos = to_linecol(texts.get(&root, &target_rel)?, start.into());
                        by_name_pos.get(&(target_rel, pos)).cloned()
                    });
                for range in call.ranges {
                    if range.file_id == file_id {
                        let pos = to_linecol(&text, range.range.start().into());
                        sites.insert(pos, (target.name.as_str().to_string(), target_id.clone()));
                    }
                }
            }
            apply(analysis, &id, &sites, &mut stats);
        }
    }
    Ok(stats)
}

/// Merges rust-analyzer's call sites into one item's calls and references.
fn apply(
    analysis: &mut Analysis,
    id: &str,
    sites: &HashMap<LineCol, (String, Option<String>)>,
    stats: &mut Stats,
) {
    let item = analysis.items.get_mut(id).unwrap();
    let mut calls: BTreeSet<Call> = item
        .info
        .calls
        .iter()
        .filter(|c| !c.inferred)
        .cloned()
        .collect();
    for (_, target) in sites.values() {
        if let Some(target) = target
            && target != id
            && calls.insert(Call {
                to: target.clone(),
                inferred: false,
            })
        {
            stats.new_calls += 1;
        }
    }
    // Keep a guess only where rust-analyzer said nothing about that call site.
    for guess in &item.guessed_calls {
        let key = format!(".{}", guess.method);
        match sites.get(&guess.site) {
            Some((_, resolved)) => {
                stats.resolved_guesses += 1;
                if item.info.body_refs.get(&key).is_some_and(|r| r.inferred) {
                    match resolved {
                        Some(target) => {
                            item.info.body_refs.insert(
                                key,
                                Ref {
                                    id: Some(target.clone()),
                                    path: target.clone(),
                                    inferred: false,
                                },
                            );
                        }
                        None => {
                            item.info.body_refs.remove(&key);
                        }
                    }
                }
            }
            None => {
                if !calls.iter().any(|c| c.to == guess.target) {
                    calls.insert(Call {
                        to: guess.target.clone(),
                        inferred: true,
                    });
                }
            }
        }
    }
    // Method names the syntactic pass could not guess at all.
    for (name, target) in sites.values() {
        if let Some(target) = target {
            item.info
                .body_refs
                .entry(format!(".{name}"))
                .or_insert_with(|| Ref {
                    id: Some(target.clone()),
                    path: target.clone(),
                    inferred: false,
                });
        }
    }
    item.info.calls = calls;
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
