//! The exact public API, from rust-analyzer's name resolution.
//!
//! The syntactic pass follows `pub use` by Rust's rules as far as it can
//! see them. Here rust-analyzer's resolved module scopes give every name
//! that code outside the crate can use, after macro expansion, `cfg` and
//! glob imports, and they are filtered the way rustdoc documents them:
//!
//! - `#[doc(hidden)]` on an item hides it everywhere; on a module, it hides
//!   the paths through that module; on a `pub use`, only the name it adds.
//! - An item is listed where rustdoc documents it: at its definition path
//!   if that is public, otherwise at a re-export, which rustdoc inlines
//!   there unless it is marked `#[doc(no_inline)]`. A `#[doc(inline)]`
//!   re-export comes before the other re-exports.
//! - Items of other crates that a crate re-exports are part of its API and
//!   become items of their own (`Analysis::items` with `reexport_of`).

use crate::analyze::{Analysis, ItemData, LineCol};
use model::{ItemInfo, Kind, Ref};
use ra_ap_hir as hir;
use ra_ap_hir_def::attrs::AttrFlags;
use ra_ap_hir_def::item_scope::{ImportOrExternCrate, ItemInNs};
use ra_ap_hir_def::visibility::Visibility;
use ra_ap_hir_def::{AttrDefId, Lookup, ModuleDefId, ModuleId};
use ra_ap_ide::TryToNav;
use ra_ap_ide_db::RootDatabase;
use ra_ap_syntax::ast::{self, HasAttrs};
use ra_ap_syntax::{AstNode, Edition};
use ra_ap_vfs::Vfs;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

/// Paths longer than this only arise from re-export cycles.
const MAX_SEGMENTS: usize = 12;
/// How many different paths lead into one module at most.
const MAX_PATHS_PER_MODULE: usize = 32;

/// How rustdoc treats a re-export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocInline {
    Default,
    /// `#[doc(inline)]`
    Always,
    /// `#[doc(no_inline)]`
    Never,
}

/// One path under which code outside the crate can name an item.
#[derive(Clone, Debug)]
struct PathInfo {
    path: String,
    /// The definition path: no `use` along the way.
    def: bool,
    /// `#[doc(hidden)]` on the item, a module along the path or a `use`.
    hidden: bool,
    /// The attribute of the last `use` along the path.
    inline: DocInline,
}

/// The paths of one item, ordered as the analysis lists them: where
/// rustdoc documents the item first, then the other re-exports, shortest
/// first. Hidden paths are dropped.
fn ordered_paths(paths: &[PathInfo]) -> Vec<String> {
    let visible: Vec<&PathInfo> = paths.iter().filter(|p| !p.hidden).collect();
    let def_visible = visible.iter().any(|p| p.def);
    let rank = |p: &PathInfo| match (p.def, p.inline) {
        (true, _) => 0,
        (false, DocInline::Always) => 1,
        (false, DocInline::Default) if !def_visible => 1,
        _ => 2,
    };
    let mut ranked: Vec<(usize, usize, &str)> = visible
        .iter()
        .map(|p| (rank(p), p.path.split("::").count(), p.path.as_str()))
        .collect();
    ranked.sort();
    let mut seen = HashSet::new();
    ranked
        .into_iter()
        .filter(|(.., p)| seen.insert(*p))
        .map(|(.., p)| p.to_string())
        .collect()
}

/// A name in a public module scope that refers to another crate's item.
struct Foreign {
    module: ModuleId,
    path: PathInfo,
    import: Option<ImportOrExternCrate>,
}

/// What [`apply`] needs to map rust-analyzer's definitions to items.
pub struct Ctx<'a> {
    pub db: &'a RootDatabase,
    pub vfs: &'a Vfs,
    pub root: &'a Path,
    pub by_name_pos: &'a HashMap<(String, LineCol), String>,
}

impl Ctx<'_> {
    /// The checkout-relative path of a file.
    fn rel(&self, file: ra_ap_vfs::FileId) -> Option<String> {
        self.vfs
            .file_path(file)
            .as_path()
            .and_then(|p| Path::new(p.as_str()).strip_prefix(self.root).ok())
            .map(|p| p.display().to_string())
    }

    fn file_text(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(rel)).ok()
    }
}

/// Replaces the public paths of the items of every library crate in the
/// loaded workspace with rust-analyzer's, and adds the items of other
/// crates they re-export. `done` collects the crates handled so far.
pub fn apply(ctx: &Ctx, analysis: &mut Analysis, done: &mut BTreeSet<String>) {
    let db = ctx.db;
    // Library crate roots of the analysis, by root file.
    let roots: HashMap<String, String> = analysis
        .crates
        .iter()
        .filter(|c| !crate::analyze::is_extra_target(c))
        .filter_map(|c| Some((analysis.items.get(c)?.info.file.clone(), c.clone())))
        .collect();
    for krate in hir::Crate::all(db) {
        let root_module = krate.root_module(db);
        let Some(file) = root_module.as_source_file_id(db) else {
            continue;
        };
        let Some(rel) = ctx.rel(file.file_id(db)) else {
            continue;
        };
        let Some(crate_id) = roots.get(&rel) else {
            continue;
        };
        if !done.insert(crate_id.clone()) {
            continue;
        }
        // rust-analyzer's type layer needs the database attached to the
        // thread. If it fails, the crate keeps the syntactic results.
        let saved = analysis.items.clone();
        let result = catch_unwind(AssertUnwindSafe(|| {
            hir::attach_db(db, || apply_crate(ctx, analysis, krate, crate_id))
        }));
        if result.is_err() {
            analysis.items = saved;
            analysis.warnings.push(format!(
                "rust-analyzer failed to determine the public API of {crate_id}; it follows the syntax only"
            ));
        }
    }
}

fn apply_crate(ctx: &Ctx, analysis: &mut Analysis, krate: hir::Crate, crate_id: &str) {
    let db = ctx.db;
    let module_id = |m: hir::Module| -> String {
        let mut id = crate_id.to_string();
        for name in m.path_segments(db) {
            id.push_str("::");
            id.push_str(name.as_str());
        }
        id
    };
    let modules: HashSet<String> = analysis
        .items
        .values()
        .filter(|i| i.info.kind == Kind::Mod)
        .map(|i| i.info.id.clone())
        .collect();
    let item_id = |def: hir::ModuleDef| -> Option<String> {
        if let hir::ModuleDef::Module(m) = def {
            let id = module_id(m);
            return modules.contains(&id).then_some(id);
        }
        let sema = hir::Semantics::new(db);
        let nav = def.try_to_nav(&sema)?.call_site;
        let rel = ctx.rel(nav.file_id)?;
        let text = ctx.file_text(&rel)?;
        let start = nav.focus_range.unwrap_or(nav.full_range).start();
        let pos = crate::semantic::to_linecol(&text, start.into());
        ctx.by_name_pos.get(&(rel, pos)).cloned()
    };

    // Every definition rust-analyzer sees in the crate: those without a
    // public path are private. Items it doesn't see (code behind an
    // inactive `cfg`) keep the syntactic results.
    let mut seen: HashSet<String> = HashSet::new();
    for m in krate.modules(db) {
        seen.extend(item_id(hir::ModuleDef::Module(m)));
        for def in m.declarations(db) {
            seen.extend(item_id(def));
        }
    }

    // Walk the public scopes from the crate root.
    let mut local: HashMap<ModuleDefId, Vec<PathInfo>> = HashMap::new();
    let mut local_macros: HashMap<hir::Macro, Vec<PathInfo>> = HashMap::new();
    let mut foreign: BTreeMap<String, (hir::ModuleDef, Vec<Foreign>)> = BTreeMap::new();
    let root = ModuleId::from(krate.root_module(db));
    let mut work = vec![(
        root,
        PathInfo {
            path: crate_id.to_string(),
            def: true,
            hidden: false,
            inline: DocInline::Default,
        },
    )];
    let mut entered: HashMap<ModuleId, usize> = HashMap::new();
    while let Some((module, at)) = work.pop() {
        let count = entered.entry(module).or_default();
        if *count >= MAX_PATHS_PER_MODULE || at.path.split("::").count() >= MAX_SEGMENTS {
            continue;
        }
        *count += 1;
        let def_map = module.def_map(db);
        for (name, per_ns) in def_map[module].scope.entries() {
            let per_ns = per_ns.filter_visibility(|v| v == Visibility::Public);
            let mut items_here: Vec<(ItemInNs, Option<ImportOrExternCrate>)> =
                per_ns.iter_items().collect();
            // A unit or tuple struct is in both name spaces: once is enough.
            items_here.dedup_by_key(|(item, _)| item.as_module_def_id());
            for (item, import) in items_here {
                let def: hir::ModuleDef = match item {
                    ItemInNs::Types(id) | ItemInNs::Values(id) => id.into(),
                    ItemInNs::Macros(id) => hir::ModuleDef::Macro(id.into()),
                };
                if matches!(def, hir::ModuleDef::BuiltinType(_)) {
                    continue;
                }
                let item_hidden = attr_owner(&def).is_some_and(|o| is_hidden(db, o));
                let (entry_hidden, inline) = match import {
                    Some(ImportOrExternCrate::ExternCrate(e)) => (
                        is_hidden(db, AttrDefId::ExternCrateId(e)),
                        DocInline::Default,
                    ),
                    Some(imp) => match imp.use_() {
                        Some(u) => (is_hidden(db, AttrDefId::UseId(u)), use_inline(db, u)),
                        None => (false, DocInline::Default),
                    },
                    None => (false, at.inline),
                };
                let path = PathInfo {
                    path: format!("{}::{}", at.path, name.as_str()),
                    def: at.def && import.is_none(),
                    hidden: at.hidden || item_hidden || entry_hidden,
                    inline,
                };
                let Some(target_crate) = def_crate(db, def) else {
                    continue;
                };
                if target_crate != krate {
                    let Some(canonical) = canonical_path(db, def) else {
                        continue;
                    };
                    foreign
                        .entry(canonical)
                        .or_insert_with(|| (def, Vec::new()))
                        .1
                        .push(Foreign {
                            module,
                            path,
                            import,
                        });
                    continue;
                }
                if let hir::ModuleDef::Module(m) = def {
                    work.push((m.into(), path.clone()));
                }
                match def {
                    hir::ModuleDef::Macro(m) => local_macros.entry(m).or_default().push(path),
                    _ => {
                        if let Ok(id) = ModuleDefId::try_from(def) {
                            local.entry(id).or_default().push(path);
                        }
                    }
                }
            }
        }
    }

    // Public paths of the crate's own items.
    let mut paths: HashMap<String, Vec<PathInfo>> = HashMap::new();
    let defs = local
        .into_iter()
        .map(|(id, p)| (hir::ModuleDef::from(id), p))
        .chain(
            local_macros
                .into_iter()
                .map(|(m, p)| (hir::ModuleDef::Macro(m), p)),
        );
    for (def, p) in defs {
        if let Some(id) = item_id(def) {
            seen.insert(id.clone());
            paths.entry(id).or_default().extend(p);
        }
    }
    let in_crate = |id: &str| id == crate_id || id.starts_with(&format!("{crate_id}::"));
    let module_level = |i: &ItemInfo| i.parent.is_none() && i.kind != Kind::Impl;
    let ids: Vec<String> = analysis
        .items
        .keys()
        .filter(|id| in_crate(id))
        .cloned()
        .collect();
    for id in &ids {
        if !seen.contains(id) {
            continue;
        }
        let item = analysis.items.get_mut(id).unwrap();
        if !module_level(&item.info) {
            continue;
        }
        if *id == crate_id {
            item.info.public = true;
            item.info.public_paths = vec![crate_id.to_string()];
            continue;
        }
        let list = ordered_paths(paths.get(id).map_or(&[], Vec::as_slice));
        item.info.public = !list.is_empty();
        item.info.public_paths = list;
    }
    // Members and impls are public with the item they belong to.
    let mut memo: HashMap<String, bool> = HashMap::new();
    for id in &ids {
        let info = &analysis.items[id].info;
        if module_level(info) {
            continue;
        }
        let public = member_public(id, &analysis.items, &mut memo);
        analysis.items.get_mut(id).unwrap().info.public = public;
    }

    // Other crates' items, re-exported.
    for (canonical, (def, uses)) in foreign {
        let all: Vec<PathInfo> = uses.iter().map(|u| u.path.clone()).collect();
        let list = ordered_paths(&all);
        let Some(first) = list.first() else {
            continue;
        };
        let Some(at) = uses.iter().find(|u| &u.path.path == first) else {
            continue;
        };
        let module = module_id(at.module.into());
        if !analysis.items.contains_key(&module) {
            continue;
        }
        let Some(site) = use_site(ctx, at) else {
            continue;
        };
        let name = first.rsplit("::").next().unwrap_or(first).to_string();
        let own_name = def.name(db).map(|n| n.as_str().to_string());
        let sig = if own_name.as_deref() == Some(name.as_str()) {
            format!("pub use {canonical};")
        } else {
            format!("pub use {canonical} as {name};")
        };
        let target = match def {
            hir::ModuleDef::Module(_) => None,
            _ => item_id(def).filter(|id| analysis.items.contains_key(id)),
        };
        let mut ext_sites = BTreeMap::new();
        if target.is_none()
            && let Some(pos) = site.name_pos
        {
            ext_sites.insert(canonical.clone(), pos);
        }
        let id = format!("{crate_id}::<re-export {canonical}>");
        let info = ItemInfo {
            id: id.clone(),
            kind: kind_of(def),
            public_paths: list,
            name,
            module: module.clone(),
            vis_scope: None,
            vis_label: "pub".into(),
            vis_parent: Some(module),
            public: true,
            sig: sig.clone(),
            sig_refs: BTreeMap::from([(
                canonical.clone(),
                Ref {
                    id: target,
                    path: canonical.clone(),
                },
            )]),
            sig_links: vec!["pub use ".len() as u32],
            file: site.file,
            line: site.line,
            reexport_of: Some(canonical.clone()),
            ..Default::default()
        };
        analysis.items.insert(
            id,
            ItemData {
                info,
                sig_norm: format!("{sig}|{canonical}"),
                body_norm: String::new(),
                source: site.source,
                name_pos: None,
                doc_hidden: false,
                call_sites: Vec::new(),
                ext_sites,
            },
        );
    }
}

/// Whether an associated item or impl is part of the public API: its own
/// visibility is `pub` (or implied, in traits and trait impls), it isn't
/// `#[doc(hidden)]`, and the item it belongs to is public.
fn member_public(
    id: &str,
    items: &BTreeMap<String, ItemData>,
    memo: &mut HashMap<String, bool>,
) -> bool {
    if let Some(&r) = memo.get(id) {
        return r;
    }
    memo.insert(id.to_string(), false);
    let r = match items.get(id) {
        None => true,
        Some(item) if item.info.parent.is_none() && item.info.kind != Kind::Impl => {
            item.info.public
        }
        Some(item) => {
            item.info.vis_scope.is_none()
                && !item.doc_hidden
                && !item.info.test_only
                && match &item.info.vis_parent {
                    None => true,
                    Some(p) => member_public(p, items, memo),
                }
        }
    };
    memo.insert(id.to_string(), r);
    r
}

/// Where a re-export is written.
struct UseSite {
    file: String,
    line: usize,
    /// The `use` item's text.
    source: String,
    /// The last segment of the re-exported path, for named re-exports.
    name_pos: Option<LineCol>,
}

fn use_site(ctx: &Ctx, at: &Foreign) -> Option<UseSite> {
    let db = ctx.db;
    let (file_id, node, tree) = match at.import? {
        ImportOrExternCrate::Import(imp) => {
            let loc = imp.use_.lookup(db);
            let tree = ra_ap_hir_def::src::use_tree_to_ast(db, loc.id, imp.idx);
            let node = loc.id.to_node(db);
            (loc.id.file_id, node.syntax().clone(), Some(tree))
        }
        ImportOrExternCrate::Glob(g) => {
            let loc = g.use_.lookup(db);
            (loc.id.file_id, loc.id.to_node(db).syntax().clone(), None)
        }
        ImportOrExternCrate::ExternCrate(e) => {
            let loc = e.lookup(db);
            (loc.id.file_id, loc.id.to_node(db).syntax().clone(), None)
        }
    };
    // Re-exports generated by macros have no place in the source.
    let file = file_id.file_id()?.file_id(db);
    let rel = ctx.rel(file)?;
    let text = ctx.file_text(&rel)?;
    let range = node.text_range();
    let start = crate::semantic::to_linecol(&text, range.start().into());
    let source = text
        .get(usize::from(range.start())..usize::from(range.end()))?
        .to_string();
    let name_pos = tree
        .and_then(|t| t.path())
        .and_then(|p| p.segment())
        .and_then(|s| s.name_ref())
        .map(|n| crate::semantic::to_linecol(&text, n.syntax().text_range().start().into()));
    Some(UseSite {
        file: rel,
        line: start.line,
        source,
        name_pos,
    })
}

fn def_crate(db: &RootDatabase, def: hir::ModuleDef) -> Option<hir::Crate> {
    Some(match def {
        hir::ModuleDef::Module(m) => m.krate(db),
        hir::ModuleDef::BuiltinType(_) => return None,
        other => other.module(db)?.krate(db),
    })
}

/// The crate's name and the path of the item's definition in it, e.g.
/// `serde_core::ser::Serialize`.
fn canonical_path(db: &RootDatabase, def: hir::ModuleDef) -> Option<String> {
    let krate = def_crate(db, def)?;
    let name = krate.display_name(db)?.to_string();
    match def {
        hir::ModuleDef::Module(m) if m.is_crate_root(db) => Some(name),
        _ => Some(format!(
            "{name}::{}",
            def.canonical_path(db, Edition::CURRENT)?
        )),
    }
}

fn kind_of(def: hir::ModuleDef) -> Kind {
    match def {
        hir::ModuleDef::Module(_) => Kind::Mod,
        hir::ModuleDef::Function(_) => Kind::Fn,
        hir::ModuleDef::Adt(hir::Adt::Struct(_)) => Kind::Struct,
        hir::ModuleDef::Adt(hir::Adt::Enum(_)) => Kind::Enum,
        hir::ModuleDef::Adt(hir::Adt::Union(_)) => Kind::Union,
        hir::ModuleDef::EnumVariant(_) => Kind::Variant,
        hir::ModuleDef::Const(_) => Kind::Const,
        hir::ModuleDef::Static(_) => Kind::Static,
        hir::ModuleDef::Trait(_) => Kind::Trait,
        hir::ModuleDef::TypeAlias(_) => Kind::TypeAlias,
        hir::ModuleDef::Macro(_) => Kind::Macro,
        hir::ModuleDef::BuiltinType(_) => Kind::TypeAlias,
    }
}

fn attr_owner(def: &hir::ModuleDef) -> Option<AttrDefId> {
    let id = match *def {
        hir::ModuleDef::Macro(m) => return Some(AttrDefId::MacroId(m.into())),
        other => ModuleDefId::try_from(other).ok()?,
    };
    Some(match id {
        ModuleDefId::ModuleId(it) => AttrDefId::ModuleId(it),
        ModuleDefId::FunctionId(it) => it.into(),
        ModuleDefId::AdtId(it) => it.into(),
        ModuleDefId::EnumVariantId(it) => it.into(),
        ModuleDefId::ConstId(it) => it.into(),
        ModuleDefId::StaticId(it) => it.into(),
        ModuleDefId::TraitId(it) => it.into(),
        ModuleDefId::TypeAliasId(it) => it.into(),
        ModuleDefId::MacroId(it) => it.into(),
        ModuleDefId::BuiltinType(_) => return None,
    })
}

fn is_hidden(db: &RootDatabase, owner: AttrDefId) -> bool {
    AttrFlags::query(db, owner).contains(AttrFlags::IS_DOC_HIDDEN)
}

/// `#[doc(inline)]` or `#[doc(no_inline)]` on a `use` item.
fn use_inline(db: &RootDatabase, use_: ra_ap_hir_def::UseId) -> DocInline {
    let node: ast::Use = use_.lookup(db).id.to_node(db);
    let mut inline = DocInline::Default;
    for attr in node.attrs() {
        let text: String = attr
            .syntax()
            .text()
            .to_string()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let Some(args) = text
            .strip_prefix("#[doc(")
            .and_then(|t| t.strip_suffix(")]"))
        else {
            continue;
        };
        for arg in args.split(',') {
            match arg {
                "inline" => inline = DocInline::Always,
                "no_inline" => inline = DocInline::Never,
                _ => {}
            }
        }
    }
    inline
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: &str, def: bool, hidden: bool, inline: DocInline) -> PathInfo {
        PathInfo {
            path: path.into(),
            def,
            hidden,
            inline,
        }
    }

    #[test]
    fn paths_are_ordered_like_rustdoc() {
        // Public definition path first, even when longer.
        assert_eq!(
            ordered_paths(&[
                p("t::X", false, false, DocInline::Default),
                p("t::a::X", true, false, DocInline::Default)
            ]),
            ["t::a::X", "t::X"]
        );
        // `#[doc(inline)]` re-exports before other ones.
        assert_eq!(
            ordered_paths(&[
                p("t::X", false, false, DocInline::Default),
                p("t::a::b::X", false, false, DocInline::Always),
                p("t::c::X", true, false, DocInline::Default),
            ]),
            ["t::c::X", "t::a::b::X", "t::X"]
        );
        // Without a public definition path, rustdoc inlines re-exports,
        // except `#[doc(no_inline)]` ones.
        assert_eq!(
            ordered_paths(&[
                p("t::X", false, false, DocInline::Never),
                p("t::a::X", false, false, DocInline::Default),
            ]),
            ["t::a::X", "t::X"]
        );
        // Hidden paths are not public.
        assert_eq!(
            ordered_paths(&[
                p("t::X", false, true, DocInline::Default),
                p("t::a::X", true, true, DocInline::Default)
            ]),
            Vec::<String>::new()
        );
    }
}
