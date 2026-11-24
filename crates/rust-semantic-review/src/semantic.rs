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
use anyhow::Result;
use model::{Call, External, Kind, Ref};
use ra_ap_ide::{
    AnalysisHost, CallHierarchyConfig, FilePosition, FileRange, GotoDefinitionConfig, HoverConfig,
    HoverDocFormat, NavigationTarget, SubstTyLen, SymbolKind, TextRange, TextSize,
};
use ra_ap_ide_db::ra_fixture::RaFixtureConfig;
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_paths::{AbsPathBuf, Utf8PathBuf};
use ra_ap_project_model::{
    CargoConfig, CargoFeatures, RustLibSource, Sysroot, TargetDirectoryConfig,
};
use ra_ap_vfs::{Vfs, VfsPath};
use std::collections::BTreeMap;
use std::collections::{BTreeSet, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

#[derive(Debug, Default)]
pub struct Stats {
    /// Functions rust-analyzer answered for.
    pub functions: usize,
    /// Call sites whose target rust-analyzer determined.
    pub resolved_sites: usize,
    /// Call sites it could not resolve either.
    pub unresolved_sites: usize,
    /// Items outside the analyzed crates it described.
    pub externals: usize,
    /// Workspaces that failed to load, with the error: their items keep
    /// the syntactic results.
    pub failed: Vec<String>,
    /// Threads that resolved the calls.
    pub threads: usize,
    /// Seconds spent loading the workspaces, resolving the calls, and
    /// determining the public API and describing external items.
    pub load_secs: f64,
    pub calls_secs: f64,
    pub rest_secs: f64,
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
    target_dir: Option<&Path>,
    analysis: &mut Analysis,
) -> Result<Stats> {
    let root = std::fs::canonicalize(root)?;
    let threads = threads();
    let mut stats = Stats {
        threads,
        ..Stats::default()
    };
    let mut texts = Texts::default();
    // Where each analyzed item's name is, to map results back to items.
    let mut by_name_pos: HashMap<(String, LineCol), String> = HashMap::new();
    for (id, item) in &analysis.items {
        if let Some(pos) = item.name_pos {
            by_name_pos.insert((item.info.file.clone(), pos), id.clone());
        }
    }
    let mut done: BTreeSet<String> = BTreeSet::new();
    // Crates whose public API rust-analyzer determined.
    let mut api_done: BTreeSet<String> = BTreeSet::new();
    for dir in manifest_dirs {
        let dir = std::fs::canonicalize(dir)?;
        let started = Instant::now();
        let (host, vfs) = match load(&dir, trust, target_dir, threads) {
            Ok(loaded) => loaded,
            Err(e) => {
                let rel = dir
                    .strip_prefix(&root)
                    .unwrap_or(&dir)
                    .display()
                    .to_string();
                let rel = if rel.is_empty() { ".".to_string() } else { rel };
                stats.failed.push(format!("{rel}: {e:#}"));
                continue;
            }
        };
        let loaded = Instant::now();
        let ids: Vec<String> = analysis
            .items
            .iter()
            .filter(|(id, item)| {
                item.info.kind.is_callable() && item.name_pos.is_some() && !done.contains(*id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let ctx = Resolver {
            vfs: &vfs,
            root: &root,
            by_name_pos: &by_name_pos,
            analysis,
        };
        let resolved = ctx.resolve_all(&host, &ids, threads);
        for (id, r) in ids.into_iter().zip(resolved) {
            let Some(r) = r else {
                continue;
            };
            done.insert(id.clone());
            stats.functions += 1;
            stats.resolved_sites += r.resolved_sites;
            stats.unresolved_sites += r.unresolved as usize;
            let item = analysis.items.get_mut(&id).unwrap();
            item.info.calls = r.calls.into_iter().map(|to| Call { to }).collect();
            item.info.unresolved_calls = r.unresolved;
            item.info.source_refs.extend(r.site_refs);
            // A function called is no longer only "used".
            let calls = item.info.calls.clone();
            item.info
                .uses
                .retain(|u| !calls.iter().any(|c| c.to == u.to));
        }
        let called = Instant::now();
        crate::public_api::apply(
            &crate::public_api::Ctx {
                db: host.raw_database(),
                vfs: &vfs,
                root: &root,
                by_name_pos: &by_name_pos,
            },
            analysis,
            &mut api_done,
        );
        let goto_config = GotoDefinitionConfig {
            ra_fixture: RaFixtureConfig::default(),
        };
        describe_externals(
            &host.analysis(),
            &vfs,
            &root,
            &mut texts,
            analysis,
            &goto_config,
        );
        stats.load_secs += (loaded - started).as_secs_f64();
        stats.calls_secs += (called - loaded).as_secs_f64();
        stats.rest_secs += called.elapsed().as_secs_f64();
    }
    // rust-analyzer sees the code behind `#[cfg(test)]` too; it is never
    // part of the public API.
    for item in analysis.items.values_mut().filter(|i| i.info.test_only) {
        item.info.public = false;
        item.info.public_paths.clear();
    }
    // Re-exported items of other crates show their docs.
    for item in analysis.items.values_mut() {
        if let Some(ext) = item
            .info
            .reexport_of
            .as_ref()
            .and_then(|p| analysis.externals.get(p))
        {
            item.info.docs = ext.docs.clone();
        }
    }
    stats.externals = analysis.externals.len();
    Ok(stats)
}

/// How many threads resolve the calls: `RUST_SEMANTIC_REVIEW_THREADS`, or
/// one per CPU core (at most 8).
///
/// The threads share one rust-analyzer database, so more threads use
/// barely more memory: what rust-analyzer computes for one function it
/// keeps for all. The cap is a guess: it was only measured up to 4 cores,
/// and the threads wait for each other more often the more there are.
fn threads() -> usize {
    std::env::var("RUST_SEMANTIC_REVIEW_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get().min(8)))
}

/// What rust-analyzer says about one function's calls.
struct Resolved {
    /// The analyzed functions it calls.
    calls: BTreeSet<String>,
    /// Method call sites with their targets, by `line:column`.
    site_refs: BTreeMap<String, Ref>,
    resolved_sites: usize,
    unresolved: u32,
}

/// Resolves functions' calls in one loaded workspace.
struct Resolver<'a> {
    vfs: &'a Vfs,
    root: &'a Path,
    by_name_pos: &'a HashMap<(String, LineCol), String>,
    analysis: &'a Analysis,
}

impl Resolver<'_> {
    /// The calls of each of `ids`, in that order (`None` where
    /// rust-analyzer had no answer). `threads` threads take the functions
    /// one at a time, each with its own snapshot of the database.
    fn resolve_all(
        &self,
        host: &AnalysisHost,
        ids: &[String],
        threads: usize,
    ) -> Vec<Option<Resolved>> {
        let next = AtomicUsize::new(0);
        // The database snapshots are made here: the host can't be shared
        // between threads.
        let work = |ra: ra_ap_ide::Analysis| {
            let mut texts = Texts::default();
            let mut out = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(id) = ids.get(i) else {
                    return out;
                };
                out.push((i, self.resolve(&ra, &mut texts, id)));
            }
        };
        let mut results: Vec<Option<Resolved>> = Vec::new();
        results.resize_with(ids.len(), || None);
        let parts = if threads <= 1 {
            vec![work(host.analysis())]
        } else {
            std::thread::scope(|s| {
                let handles: Vec<_> = (0..threads)
                    .map(|_| {
                        let ra = host.analysis();
                        s.spawn(|| work(ra))
                    })
                    .collect();
                // A panic outside `catch_unwind` (not one of
                // rust-analyzer's) is a bug of this tool: pass it on.
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e)))
                    .collect()
            })
        };
        for (i, r) in parts.into_iter().flatten() {
            results[i] = r;
        }
        results
    }

    /// Asks rust-analyzer what the function `id` calls. Every call site
    /// found by the syntactic pass is checked, including method calls and
    /// calls inside macro invocations.
    fn resolve(&self, ra: &ra_ap_ide::Analysis, texts: &mut Texts, id: &str) -> Option<Resolved> {
        let (vfs, root, by_name_pos) = (self.vfs, self.root, self.by_name_pos);
        let item = &self.analysis.items[id];
        let call_config = CallHierarchyConfig {
            exclude_tests: true,
            ra_fixture: RaFixtureConfig::default(),
        };
        let goto_config = GotoDefinitionConfig {
            ra_fixture: RaFixtureConfig::default(),
        };
        let rel = &item.info.file;
        let file_id = file_id(vfs, &root.join(rel))?;
        let text = texts.get(root, rel)?.to_string();
        let position = |pos: LineCol| FilePosition {
            file_id,
            offset: TextSize::from(to_offset(&text, pos) as u32),
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            ra.outgoing_calls(&call_config, position(item.name_pos.unwrap()))
        }));
        let Ok(Ok(Some(calls))) = result else {
            return None;
        };

        // Where each call goes, by the range of its call site.
        let mut ranges: Vec<(LineCol, LineCol, Target)> = Vec::new();
        let mut targets: BTreeSet<String> = BTreeSet::new();
        for call in calls {
            let target = target_of(vfs, root, texts, by_name_pos, &call.target);
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
        let mut site_refs: BTreeMap<String, Ref> = BTreeMap::new();
        let (mut resolved_sites, mut unresolved) = (0, 0);
        for site in &item.call_sites {
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
                    .map(|nav| target_of(vfs, root, texts, by_name_pos, nav))
                    .collect();
                // Several definitions (e.g. ambiguous macro code): unknown.
                (found.len() == 1).then(|| found[0].clone())
            });
            match target {
                Some(Target::Item(t)) => {
                    resolved_sites += 1;
                    let callable = self
                        .analysis
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
                Some(Target::Other) => resolved_sites += 1,
                None => unresolved += 1,
            }
        }
        targets.remove(id);
        Some(Resolved {
            calls: targets,
            site_refs,
            resolved_sites,
            unresolved,
        })
    }
}

/// Describes the items outside the analyzed crates that the code names
/// (the standard library and dependencies): rust-analyzer's hover at one
/// place that names each of them gives its declaration and docs. Only
/// what the analyzed code refers to is looked up, not whole crates.
fn describe_externals(
    ra: &ra_ap_ide::Analysis,
    vfs: &Vfs,
    root: &Path,
    texts: &mut Texts,
    analysis: &mut Analysis,
    goto_config: &GotoDefinitionConfig,
) {
    let config = HoverConfig {
        links_in_hover: true,
        memory_layout: None,
        documentation: true,
        keywords: false,
        format: HoverDocFormat::Markdown,
        max_trait_assoc_items_count: Some(12),
        max_fields_count: Some(12),
        max_enum_variants_count: Some(12),
        max_subst_ty_len: SubstTyLen::Hide,
        show_drop_glue: false,
        ra_fixture: RaFixtureConfig::default(),
    };
    // One place in the code per external path.
    let mut sites: BTreeMap<String, (String, LineCol)> = BTreeMap::new();
    for item in analysis.items.values() {
        for (path, pos) in &item.ext_sites {
            sites
                .entry(path.clone())
                .or_insert_with(|| (item.info.file.clone(), *pos));
        }
    }
    let allowed = allowed_sources(root);
    for (path, (rel, pos)) in sites {
        if analysis.externals.contains_key(&path) {
            continue;
        }
        let Some(file_id) = file_id(vfs, &root.join(&rel)) else {
            continue;
        };
        let Some(text) = texts.get(root, &rel) else {
            continue;
        };
        let offset = TextSize::from(to_offset(text, pos) as u32);
        let position = FilePosition { file_id, offset };
        let result = catch_unwind(AssertUnwindSafe(|| {
            let nav = ra.goto_definition(position, goto_config).ok()??;
            let [nav] = nav.info.as_slice() else {
                return None;
            };
            let hover = ra
                .hover(
                    &config,
                    FileRange {
                        file_id,
                        range: TextRange::empty(offset),
                    },
                )
                .ok()??;
            let url = ra
                .external_docs(position, None, None)
                .ok()
                .and_then(|l| l.web_url);
            Some((nav.clone(), hover.info.markup.as_str().to_string(), url))
        }));
        let Ok(Some((nav, markup, url))) = result else {
            continue;
        };
        // Only describe items whose source is in the checkout, the
        // toolchain or Cargo's download directory: a path dependency
        // could point at any file on this machine.
        let source = vfs
            .file_path(nav.file_id)
            .as_path()
            .map(|p| PathBuf::from(p.as_str()));
        if !source.is_some_and(|p| allowed.iter().any(|a| p.starts_with(a))) {
            continue;
        }
        if let Some(ext) = external_from(&path, &nav, &markup, url) {
            analysis.externals.insert(path, ext);
        }
    }
}

/// Directories whose files may be described: the checkout, the toolchain
/// (sysroot) and Cargo's registry and git checkouts.
fn allowed_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")));
    if let Some(home) = cargo_home {
        for sub in ["registry", "git"] {
            if let Ok(p) = std::fs::canonicalize(home.join(sub)) {
                out.push(p);
            }
        }
    }
    if let Ok(o) = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        && let Ok(p) = std::fs::canonicalize(String::from_utf8_lossy(&o.stdout).trim())
    {
        out.push(p);
    }
    out
}

/// Builds the description of `path` from rust-analyzer's answers, if they
/// are about the item the analysis means by `path`.
fn external_from(
    path: &str,
    nav: &NavigationTarget,
    markup: &str,
    url: Option<String>,
) -> Option<External> {
    let kind = match nav.kind? {
        SymbolKind::Module | SymbolKind::CrateRoot => Kind::Mod,
        SymbolKind::Struct => Kind::Struct,
        SymbolKind::Enum => Kind::Enum,
        SymbolKind::Union => Kind::Union,
        SymbolKind::Trait => Kind::Trait,
        SymbolKind::TypeAlias => Kind::TypeAlias,
        SymbolKind::Function => Kind::Fn,
        SymbolKind::Method => Kind::Method,
        SymbolKind::Const => Kind::Const,
        SymbolKind::Static => Kind::Static,
        SymbolKind::Macro => Kind::Macro,
        SymbolKind::Variant => Kind::Variant,
        _ => return None,
    };
    let (module, sig, docs) = parse_hover(markup)?;
    let name = nav.name.to_string();
    // The same item: same name, and from the same crate (the standard
    // library re-exports `core` and `alloc` as `std`).
    let (path_crate, path_name) = (path.split("::").next()?, path.rsplit("::").next()?);
    let module_crate = module.split("::").next()?;
    let std = |c: &str| matches!(c, "std" | "core" | "alloc");
    let same_crate = path_crate == module_crate || (std(path_crate) && std(module_crate));
    if name != path_name || !same_crate {
        return None;
    }
    // Functions are laid out like the analyzed ones; an associated
    // function's hover starts with its impl header.
    let sig = match (kind, sig.split_once('\n')) {
        (Kind::Fn | Kind::Method, Some((header, f))) if header.starts_with("impl") => {
            format!("{header}\n{}", crate::analyze::wrap_fn_sig(f))
        }
        (Kind::Fn | Kind::Method, _) => crate::analyze::wrap_fn_sig(&sig),
        _ => sig,
    };
    Some(External {
        kind,
        module,
        name,
        sig,
        docs,
        url,
    })
}

/// Splits rust-analyzer's hover text: the module path, the declaration
/// and the docs.
fn parse_hover(markup: &str) -> Option<(String, String, String)> {
    const OPEN: &str = "```rust\n";
    const CLOSE: &str = "\n```";
    let block = |s: &str| -> Option<(String, usize)> {
        let rest = s.strip_prefix(OPEN)?;
        let end = rest.find(CLOSE)?;
        Some((rest[..end].to_string(), OPEN.len() + end + CLOSE.len()))
    };
    let markup = markup.trim_start();
    let (first, used) = block(markup)?;
    let rest = &markup[used..];
    let (module, sig, rest) = match rest.strip_prefix("\n\n").and_then(block) {
        Some((sig, used2)) => (first, sig, &rest[2 + used2..]),
        None => return None,
    };
    // A horizontal rule separates the docs.
    let docs = ["\n---\n", "\n___\n"]
        .iter()
        .filter_map(|rule| rest.find(rule).map(|i| i + rule.len()))
        .min()
        .map_or(String::new(), |i| rest[i..].trim().to_string());
    Some((module, sig, docs))
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

fn load(
    dir: &Path,
    trust: Trust,
    target_dir: Option<&Path>,
    threads: usize,
) -> Result<(AnalysisHost, Vfs)> {
    let target_dir_config = match target_dir.map(|d| Utf8PathBuf::from_path_buf(d.to_path_buf())) {
        Some(Ok(d)) => TargetDirectoryConfig::Directory(d),
        Some(Err(d)) => anyhow::bail!("target directory is not UTF-8: {}", d.display()),
        None => TargetDirectoryConfig::None,
    };
    let cargo = CargoConfig {
        sysroot: Some(RustLibSource::Discover),
        // The syntactic analysis sees code behind every `cfg(feature)`;
        // enable all features so that rust-analyzer does too.
        features: CargoFeatures::All,
        // `cfg(test)` for the analyzed crates, as the rust-analyzer
        // language server sets it. rust-analyzer expands `#[test]` to
        // `#[cfg(test)]`: without it, test functions and `#[cfg(test)]`
        // modules have no body to infer types in. Code behind
        // `cfg(not(test))` is then inactive and keeps the syntactic results.
        set_test: true,
        // Where `cargo check` builds the build scripts and proc macros.
        target_dir_config,
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
        // Without this, the first functions' calls make one thread compute
        // each crate's name resolution while the other threads wait for it.
        // Prefilling computes it for independent crates in parallel (and
        // also builds symbol and import indexes, which the tool doesn't
        // use). With one thread there is nothing to gain.
        prefill_caches: threads > 1,
        num_worker_threads: threads,
        proc_macro_processes: usize::from(trusted),
    };
    discover_sysroot(dir, &cargo);
    let (db, vfs, _) = load_workspace_at(dir, &cargo, &load, &|_| {})?;
    Ok((AnalysisHost::with_database(db), vfs))
}

/// Finds the toolchain's sysroot for `dir`, one load at a time.
///
/// When the toolchain has no `rust-src` component, rust-analyzer installs
/// it with `rustup component add rust-src` while it discovers the sysroot.
/// A second load in the same process at the same time (the tests run
/// several in parallel) would find the component missing or half-installed
/// and silently load without the standard library: then nothing from `std`
/// resolves, nor any method called on a value of a `std` type. Discovering
/// the sysroot here under a lock lets the install finish first; the load's
/// own discovery afterwards finds the component in place.
fn discover_sysroot(dir: &Path, cargo: &CargoConfig) {
    static LOCK: Mutex<()> = Mutex::new(());
    let Some(dir) = dir.to_str().and_then(|d| AbsPathBuf::try_from(d).ok()) else {
        return;
    };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Sysroot::discover(&dir, &cargo.extra_env);
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

pub(crate) fn to_linecol(text: &str, offset: u32) -> LineCol {
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
    fn hover_text_is_split() {
        let markup = "\n```rust\ncore::result\n```\n\n```rust\npub enum Result<T, E> {\n    Ok(T),\n}\n```\n\n---\n\n`Result` is a type.";
        assert_eq!(
            parse_hover(markup),
            Some((
                "core::result".into(),
                "pub enum Result<T, E> {\n    Ok(T),\n}".into(),
                "`Result` is a type.".into()
            ))
        );
        // Without a module path it is not an item.
        assert_eq!(parse_hover("```rust\nlet x: u8\n```"), None);
    }

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
