//! Turns one source tree into a flat map of items with resolved names,
//! rustdoc-style signatures and call/use edges.

use crate::source::CrateSrc;
use model::{Call, ItemInfo, Kind, Ref, Use, UseCtx};
use proc_macro2::TokenStream;
use quote::ToTokens;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use syn::spanned::Spanned;
use syn::visit_mut::{self, VisitMut};
use syn::{Attribute, Expr, Item, Visibility, parse_quote};

/// A position in a source file: 1-based line, 0-based column in chars
/// (the convention of `proc_macro2`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineCol {
    pub line: usize,
    pub column: usize,
}

impl From<proc_macro2::LineColumn> for LineCol {
    fn from(lc: proc_macro2::LineColumn) -> Self {
        LineCol {
            line: lc.line,
            column: lc.column,
        }
    }
}

/// A place in a function body that looks like a call.
#[derive(Clone, Debug)]
pub struct CallSite {
    /// Position of the called name (the method name, or the last segment
    /// of a path).
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    pub pos: LineCol,
    pub method: bool,
    /// Inside a macro invocation, where the syntax alone doesn't prove that
    /// the code runs (or runs as a call).
    pub in_macro: bool,
    /// The path's target is known (maybe outside the analyzed crates).
    pub known: bool,
    /// For path calls outside macros: the function the path names, by
    /// Rust's name resolution rules.
    pub target: Option<String>,
}

/// A name space of Rust paths: a name can refer to a type and to a value
/// at the same time (e.g. `mod foo` and `fn foo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ns {
    Type,
    Value,
}

/// One item of one side (base or head) of the pull request.
#[derive(Clone, Debug)]
pub struct ItemData {
    pub info: ItemInfo,
    /// Signature text plus the targets of all names in it.
    pub sig_norm: String,
    /// Canonical body text plus the targets of all names in it.
    pub body_norm: String,
    /// The item's source text.
    pub source: String,
    /// Where the item's name is, for functions and methods.
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    pub name_pos: Option<LineCol>,
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    pub call_sites: Vec<CallSite>,
}

pub struct Analysis {
    pub items: BTreeMap<String, ItemData>,
    pub crates: Vec<String>,
    pub warnings: Vec<String>,
}

struct SrcFile {
    rel: String,
    lines: Vec<String>,
}

#[derive(Default)]
struct ModData {
    parent: Option<String>,
    defs: HashMap<String, Vec<String>>,
    uses: Vec<UseEntry>,
}

struct UseEntry {
    /// `None` for glob imports.
    alias: Option<String>,
    path: Vec<String>,
    public: bool,
}

enum Node {
    Item(Item),
    Mod {
        attrs: Vec<Attribute>,
        vis: Visibility,
    },
    ImplFn(syn::ImplItemFn),
    ImplConst(syn::ImplItemConst),
    ImplType(syn::ImplItemType),
    TraitFn(syn::TraitItemFn),
    TraitConst(syn::TraitItemConst),
    TraitType(syn::TraitItemType),
}

struct Pending {
    id: String,
    name: String,
    module: String,
    file: Rc<SrcFile>,
    node: Node,
    /// Owning type/trait/impl item.
    parent: Option<String>,
    /// Item id that `Self` refers to.
    self_ty: Option<String>,
    impl_ctx: Option<Rc<ImplCtx>>,
    /// Generic parameters of the enclosing impl or trait.
    outer_generics: Vec<String>,
}

struct ImplCtx {
    header: String,
    trait_ref: Option<Ref>,
    /// Local self type, if any.
    self_ty: Option<String>,
    /// Visibility parent of the members.
    vis_parent: Option<String>,
}

/// Output of [`Analyzer::render_node`].
struct Rendered {
    attrs: Vec<Attribute>,
    vis: Visibility,
    sig: String,
    sig_public: Option<String>,
    body_norm: String,
    span: Option<proc_macro2::Span>,
    use_ctx: UseCtx,
}

struct RawImpl {
    module: String,
    file: Rc<SrcFile>,
    item: syn::ItemImpl,
}

#[derive(Clone)]
struct Res {
    id: Option<String>,
    path: String,
    /// Index of the first segment to keep when displaying the path.
    display_start: usize,
}

pub struct Analyzer {
    root: PathBuf,
    modules: HashMap<String, ModData>,
    kinds: HashMap<String, Kind>,
    pending: Vec<Pending>,
    impls: Vec<RawImpl>,
    /// Type or trait id → associated items as (name, id).
    assoc: HashMap<String, Vec<(String, String)>>,
    enum_variants: HashMap<String, Vec<String>>,
    /// Declared visibility scope of module-level definitions (`None` = pub),
    /// to know what a glob import brings in.
    def_vis: HashMap<String, Option<String>>,
    /// Tuple and unit structs, which are also values (constructors).
    value_structs: std::collections::HashSet<String>,
    /// Per crate root: dependency names and edition.
    crate_info: HashMap<String, (BTreeSet<String>, Option<u16>)>,
    children: HashMap<String, Vec<String>>,
    crate_roots: BTreeSet<String>,
    warnings: Vec<String>,
}

/// Names of the standard prelude: name, path, name space, and the first
/// edition that has them.
const PRELUDE: &[(&str, &str, Ns, u16)] = &[
    ("Option", "core::option::Option", Ns::Type, 2015),
    ("Some", "core::option::Option::Some", Ns::Value, 2015),
    ("None", "core::option::Option::None", Ns::Value, 2015),
    ("Result", "core::result::Result", Ns::Type, 2015),
    ("Ok", "core::result::Result::Ok", Ns::Value, 2015),
    ("Err", "core::result::Result::Err", Ns::Value, 2015),
    ("String", "alloc::string::String", Ns::Type, 2015),
    ("ToString", "alloc::string::ToString", Ns::Type, 2015),
    ("Vec", "alloc::vec::Vec", Ns::Type, 2015),
    ("Box", "alloc::boxed::Box", Ns::Type, 2015),
    ("Clone", "core::clone::Clone", Ns::Type, 2015),
    ("Copy", "core::marker::Copy", Ns::Type, 2015),
    ("Send", "core::marker::Send", Ns::Type, 2015),
    ("Sync", "core::marker::Sync", Ns::Type, 2015),
    ("Sized", "core::marker::Sized", Ns::Type, 2015),
    ("Unpin", "core::marker::Unpin", Ns::Type, 2015),
    ("Default", "core::default::Default", Ns::Type, 2015),
    ("Drop", "core::ops::Drop", Ns::Type, 2015),
    ("Fn", "core::ops::Fn", Ns::Type, 2015),
    ("FnMut", "core::ops::FnMut", Ns::Type, 2015),
    ("FnOnce", "core::ops::FnOnce", Ns::Type, 2015),
    ("From", "core::convert::From", Ns::Type, 2015),
    ("Into", "core::convert::Into", Ns::Type, 2015),
    ("AsRef", "core::convert::AsRef", Ns::Type, 2015),
    ("AsMut", "core::convert::AsMut", Ns::Type, 2015),
    ("Iterator", "core::iter::Iterator", Ns::Type, 2015),
    ("IntoIterator", "core::iter::IntoIterator", Ns::Type, 2015),
    (
        "DoubleEndedIterator",
        "core::iter::DoubleEndedIterator",
        Ns::Type,
        2015,
    ),
    (
        "ExactSizeIterator",
        "core::iter::ExactSizeIterator",
        Ns::Type,
        2015,
    ),
    ("Extend", "core::iter::Extend", Ns::Type, 2015),
    ("PartialEq", "core::cmp::PartialEq", Ns::Type, 2015),
    ("Eq", "core::cmp::Eq", Ns::Type, 2015),
    ("PartialOrd", "core::cmp::PartialOrd", Ns::Type, 2015),
    ("Ord", "core::cmp::Ord", Ns::Type, 2015),
    ("ToOwned", "alloc::borrow::ToOwned", Ns::Type, 2015),
    ("TryFrom", "core::convert::TryFrom", Ns::Type, 2021),
    ("TryInto", "core::convert::TryInto", Ns::Type, 2021),
    ("FromIterator", "core::iter::FromIterator", Ns::Type, 2021),
];

/// Crates every crate can name without declaring them.
const BUILTIN_CRATES: &[&str] = &["std", "core", "alloc"];

/// Whether an item of `kind` (with `value_struct` for tuple and unit
/// structs) lives in name space `ns`. `None` accepts any.
fn in_ns(kind: Kind, value_struct: bool, ns: Option<Ns>) -> bool {
    match ns {
        None => true,
        Some(Ns::Type) => matches!(
            kind,
            Kind::Mod | Kind::Struct | Kind::Enum | Kind::Union | Kind::Trait | Kind::TypeAlias
        ),
        Some(Ns::Value) => {
            matches!(kind, Kind::Fn | Kind::Const | Kind::Static)
                || (kind == Kind::Struct && value_struct)
        }
    }
}

impl Analyzer {
    pub fn run(root: &Path, crates: &[CrateSrc]) -> Analysis {
        let mut an = Analyzer {
            root: root.to_path_buf(),
            modules: HashMap::new(),
            kinds: HashMap::new(),
            pending: Vec::new(),
            impls: Vec::new(),
            assoc: HashMap::new(),
            enum_variants: HashMap::new(),
            def_vis: HashMap::new(),
            value_structs: Default::default(),
            crate_info: HashMap::new(),
            children: HashMap::new(),
            crate_roots: BTreeSet::new(),
            warnings: Vec::new(),
        };
        let mut crate_order = Vec::new();
        for krate in crates {
            if !an.crate_roots.insert(krate.name.clone()) {
                continue;
            }
            crate_order.push(krate.name.clone());
            an.crate_info.insert(
                krate.name.clone(),
                (krate.deps.iter().cloned().collect(), krate.edition),
            );
            match an.load_file(&krate.root_file) {
                Ok((file, src)) => {
                    let dir = krate.root_file.parent().unwrap().to_path_buf();
                    an.kinds.insert(krate.name.clone(), Kind::Mod);
                    an.modules.insert(krate.name.clone(), ModData::default());
                    an.pending.push(Pending {
                        id: krate.name.clone(),
                        name: krate.name.clone(),
                        module: krate.name.clone(),
                        file: src.clone(),
                        node: Node::Mod {
                            attrs: file.attrs.clone(),
                            vis: parse_quote!(pub),
                        },
                        parent: None,
                        self_ty: None,
                        impl_ctx: None,
                        outer_generics: Vec::new(),
                    });
                    an.collect_items(&krate.name, &file.items, &src, &dir);
                }
                Err(e) => an.warnings.push(e),
            }
        }
        an.collect_impls();
        let items = an.build_items();
        Analysis {
            items,
            crates: crate_order,
            warnings: an.warnings,
        }
    }

    fn load_file(&self, path: &Path) -> Result<(syn::File, Rc<SrcFile>), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let file =
            syn::parse_file(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        let rel = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string();
        let src = Rc::new(SrcFile {
            rel,
            lines: text.lines().map(str::to_string).collect(),
        });
        Ok((file, src))
    }

    fn define(&mut self, module: &str, name: &str, id: &str, kind: Kind, vis: &Visibility) {
        let scope = self.declared_scope(module, vis);
        self.def_vis.insert(id.to_string(), scope);
        self.modules
            .get_mut(module)
            .unwrap()
            .defs
            .entry(name.to_string())
            .or_default()
            .push(id.to_string());
        self.kinds.insert(id.to_string(), kind);
        self.children
            .entry(module.to_string())
            .or_default()
            .push(id.to_string());
    }

    /// The module a declared visibility makes an item visible in, without
    /// name resolution (paths in `pub(in …)` are absolute or relative).
    fn declared_scope(&self, module: &str, vis: &Visibility) -> Option<String> {
        match vis {
            Visibility::Public(_) => None,
            Visibility::Inherited => Some(module.to_string()),
            Visibility::Restricted(r) => {
                let mut scope = module.to_string();
                for seg in path_segments(&r.path) {
                    scope = match seg.as_str() {
                        "crate" => Self::crate_of(module).to_string(),
                        "self" => scope,
                        "super" => self
                            .modules
                            .get(&scope)
                            .and_then(|m| m.parent.clone())
                            .unwrap_or(scope),
                        name => format!("{scope}::{name}"),
                    };
                }
                Some(scope)
            }
        }
    }

    /// Pass 1: module tree, definitions and imports.
    fn collect_items(&mut self, module: &str, items: &[Item], src: &Rc<SrcFile>, dir: &Path) {
        for item in items {
            if is_test_only(item_attrs(item)) {
                continue;
            }
            let push = |an: &mut Analyzer, name: String, kind: Kind| {
                let id = format!("{module}::{name}");
                let vis = match item {
                    Item::Fn(i) => i.vis.clone(),
                    Item::Struct(i) => i.vis.clone(),
                    Item::Union(i) => i.vis.clone(),
                    Item::Enum(i) => i.vis.clone(),
                    Item::Const(i) => i.vis.clone(),
                    Item::Static(i) => i.vis.clone(),
                    Item::Type(i) => i.vis.clone(),
                    Item::Trait(i) => i.vis.clone(),
                    _ => Visibility::Inherited,
                };
                an.define(module, &name, &id, kind, &vis);
                an.pending.push(Pending {
                    id,
                    name,
                    module: module.to_string(),
                    file: src.clone(),
                    node: Node::Item(item.clone()),
                    parent: None,
                    self_ty: None,
                    impl_ctx: None,
                    outer_generics: Vec::new(),
                });
            };
            match item {
                Item::Fn(f) => push(self, f.sig.ident.to_string(), Kind::Fn),
                Item::Struct(s) => {
                    if !matches!(s.fields, syn::Fields::Named(_)) {
                        self.value_structs.insert(format!("{module}::{}", s.ident));
                    }
                    push(self, s.ident.to_string(), Kind::Struct)
                }
                Item::Union(u) => push(self, u.ident.to_string(), Kind::Union),
                Item::Enum(e) => {
                    let id = format!("{module}::{}", e.ident);
                    self.enum_variants
                        .insert(id, e.variants.iter().map(|v| v.ident.to_string()).collect());
                    push(self, e.ident.to_string(), Kind::Enum)
                }
                Item::Const(c) => push(self, c.ident.to_string(), Kind::Const),
                Item::Static(s) => push(self, s.ident.to_string(), Kind::Static),
                Item::Type(t) => push(self, t.ident.to_string(), Kind::TypeAlias),
                Item::Macro(m) => {
                    if let Some(ident) = &m.ident {
                        push(self, ident.to_string(), Kind::Macro)
                    }
                }
                Item::Trait(t) => {
                    let trait_id = format!("{module}::{}", t.ident);
                    push(self, t.ident.to_string(), Kind::Trait);
                    for member in &t.items {
                        let (name, kind, node) = match member {
                            syn::TraitItem::Fn(f) => (
                                f.sig.ident.to_string(),
                                Kind::TraitMethod,
                                Node::TraitFn(f.clone()),
                            ),
                            syn::TraitItem::Const(c) => (
                                c.ident.to_string(),
                                Kind::AssocConst,
                                Node::TraitConst(c.clone()),
                            ),
                            syn::TraitItem::Type(ty) => (
                                ty.ident.to_string(),
                                Kind::AssocType,
                                Node::TraitType(ty.clone()),
                            ),
                            _ => continue,
                        };
                        let id = format!("{trait_id}::{name}");
                        self.register_assoc(&trait_id, &name, &id, kind);
                        self.pending.push(Pending {
                            id,
                            name,
                            module: module.to_string(),
                            file: src.clone(),
                            node,
                            parent: Some(trait_id.clone()),
                            self_ty: Some(trait_id.clone()),
                            impl_ctx: None,
                            outer_generics: generic_names(&t.generics),
                        });
                    }
                }
                Item::Impl(i) => self.impls.push(RawImpl {
                    module: module.to_string(),
                    file: src.clone(),
                    item: i.clone(),
                }),
                Item::Use(u) => {
                    let public = matches!(u.vis, Visibility::Public(_));
                    let mut entries = Vec::new();
                    flatten_use(&u.tree, Vec::new(), &mut entries);
                    let m = self.modules.get_mut(module).unwrap();
                    for (alias, path) in entries {
                        m.uses.push(UseEntry {
                            alias,
                            path,
                            public,
                        });
                    }
                }
                Item::ExternCrate(e) => {
                    let alias = e
                        .rename
                        .as_ref()
                        .map(|(_, i)| i.to_string())
                        .unwrap_or_else(|| e.ident.to_string());
                    self.modules.get_mut(module).unwrap().uses.push(UseEntry {
                        alias: Some(alias),
                        path: vec![e.ident.to_string()],
                        public: false,
                    });
                }
                Item::Mod(m) => {
                    let name = m.ident.to_string();
                    let id = format!("{module}::{name}");
                    let path_attr = path_attr(&m.attrs);
                    let (child_items, child_src, child_dir, file_attrs) = match &m.content {
                        Some((_, items)) => {
                            let child_dir = match &path_attr {
                                Some(p) => dir.join(p),
                                None => dir.join(&name),
                            };
                            (items.clone(), src.clone(), child_dir, Vec::new())
                        }
                        None => {
                            let candidates = match &path_attr {
                                Some(p) => vec![dir.join(p)],
                                None => {
                                    vec![
                                        dir.join(format!("{name}.rs")),
                                        dir.join(&name).join("mod.rs"),
                                    ]
                                }
                            };
                            let Some(file_path) = candidates.iter().find(|p| p.is_file()) else {
                                self.warnings.push(format!(
                                    "module `{id}`: file not found (tried {})",
                                    candidates
                                        .iter()
                                        .map(|c| c.display().to_string())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                ));
                                continue;
                            };
                            match self.load_file(file_path) {
                                Ok((file, child_src)) => {
                                    let is_mod_rs = file_path.file_name().unwrap() == "mod.rs"
                                        || path_attr.is_some();
                                    let child_dir = if is_mod_rs {
                                        file_path.parent().unwrap().to_path_buf()
                                    } else {
                                        dir.join(&name)
                                    };
                                    (file.items, child_src, child_dir, file.attrs)
                                }
                                Err(e) => {
                                    self.warnings.push(e);
                                    continue;
                                }
                            }
                        }
                    };
                    self.define(module, &name, &id, Kind::Mod, &m.vis);
                    self.modules.insert(
                        id.clone(),
                        ModData {
                            parent: Some(module.to_string()),
                            ..Default::default()
                        },
                    );
                    let mut attrs = m.attrs.clone();
                    attrs.extend(file_attrs);
                    self.pending.push(Pending {
                        id: id.clone(),
                        name,
                        module: module.to_string(),
                        file: child_src.clone(),
                        node: Node::Mod {
                            attrs,
                            vis: m.vis.clone(),
                        },
                        parent: None,
                        self_ty: None,
                        impl_ctx: None,
                        outer_generics: Vec::new(),
                    });
                    self.collect_items(&id, &child_items, &child_src, &child_dir);
                }
                _ => {}
            }
        }
    }

    fn register_assoc(&mut self, owner: &str, name: &str, id: &str, kind: Kind) {
        self.assoc
            .entry(owner.to_string())
            .or_default()
            .push((name.to_string(), id.to_string()));
        self.kinds.insert(id.to_string(), kind);
        self.children
            .entry(owner.to_string())
            .or_default()
            .push(id.to_string());
    }

    /// Pass 2: impl blocks. Needs the full module tree to resolve self types.
    fn collect_impls(&mut self) {
        let impls = std::mem::take(&mut self.impls);
        for raw in impls {
            let item = &raw.item;
            if is_test_only(&item.attrs) {
                continue;
            }
            let self_id = match &*item.self_ty {
                syn::Type::Path(tp) if tp.qself.is_none() => self
                    .resolve(&raw.module, &path_segments(&tp.path), None, Some(Ns::Type))
                    .and_then(|r| r.id)
                    .filter(|id| self.kinds.get(id).is_some_and(|k| k.is_type_like())),
                _ => None,
            };
            let trait_res = item.trait_.as_ref().map(|(path, _)| {
                let segs = path_segments(path);
                self.resolve(&raw.module, &segs, None, Some(Ns::Type))
                    .unwrap_or(Res {
                        id: None,
                        path: segs.join("::"),
                        display_start: segs.len() - 1,
                    })
            });
            // Header, displayed with normalized paths.
            let mut header_item = item.clone();
            header_item.items.clear();
            header_item.attrs.clear();
            let mut col = Collector::new(self, &raw.module, self_id.as_deref(), true);
            col.visit_item_impl_mut(&mut header_item);
            let header = strip_body(&unparse(Item::Impl(header_item)));
            let self_display = {
                let mut ty = (*item.self_ty).clone();
                let mut col = Collector::new(self, &raw.module, self_id.as_deref(), true);
                col.visit_type_mut(&mut ty);
                unparse_type(&ty)
            };

            let (owner, impl_id) = match &trait_res {
                Some(tr) => {
                    let trait_display = {
                        let mut p = item.trait_.as_ref().unwrap().0.clone();
                        let mut col = Collector::new(self, &raw.module, self_id.as_deref(), true);
                        col.visit_path_mut(&mut p);
                        unparse_type(&parse_quote!(#p))
                    };
                    let base = self_id.clone().unwrap_or_else(|| raw.module.clone());
                    let impl_id = match &self_id {
                        Some(_) => format!("{base}::<impl {trait_display}>"),
                        None => format!("{base}::<impl {trait_display} for {self_display}>"),
                    };
                    let name = format!("impl {trait_display} for {self_display}");
                    if self.kinds.contains_key(&impl_id) {
                        // Duplicate impl headers (e.g. different generics); skip.
                        continue;
                    }
                    self.kinds.insert(impl_id.clone(), Kind::Impl);
                    let owner_of_impl = self_id.clone().unwrap_or_else(|| raw.module.clone());
                    self.children
                        .entry(owner_of_impl)
                        .or_default()
                        .push(impl_id.clone());
                    if let Some(tid) = &tr.id {
                        self.children
                            .entry(tid.clone())
                            .or_default()
                            .push(impl_id.clone());
                    }
                    self.pending.push(Pending {
                        id: impl_id.clone(),
                        name,
                        module: raw.module.clone(),
                        file: raw.file.clone(),
                        node: Node::Item(Item::Impl(item.clone())),
                        parent: self_id.clone(),
                        self_ty: self_id.clone(),
                        impl_ctx: Some(Rc::new(ImplCtx {
                            header: header.clone(),
                            trait_ref: Some(Ref {
                                id: tr.id.clone(),
                                path: tr.path.clone(),
                            }),
                            self_ty: self_id.clone(),
                            vis_parent: self_id.clone(),
                        })),
                        outer_generics: generic_names(&item.generics),
                    });
                    (impl_id.clone(), impl_id)
                }
                None => {
                    let owner = match &self_id {
                        Some(id) => id.clone(),
                        None => format!("{}::<impl {self_display}>", raw.module),
                    };
                    (owner.clone(), owner)
                }
            };
            let ctx = Rc::new(ImplCtx {
                header,
                trait_ref: trait_res.as_ref().map(|tr| Ref {
                    id: tr.id.clone(),
                    path: tr.path.clone(),
                }),
                self_ty: self_id.clone(),
                vis_parent: self_id.clone(),
            });
            for member in &item.items {
                let (name, kind, node) = match member {
                    syn::ImplItem::Fn(f) => {
                        if is_test_only(&f.attrs) {
                            continue;
                        }
                        (
                            f.sig.ident.to_string(),
                            Kind::Method,
                            Node::ImplFn(f.clone()),
                        )
                    }
                    syn::ImplItem::Const(c) => (
                        c.ident.to_string(),
                        Kind::AssocConst,
                        Node::ImplConst(c.clone()),
                    ),
                    syn::ImplItem::Type(t) => (
                        t.ident.to_string(),
                        Kind::AssocType,
                        Node::ImplType(t.clone()),
                    ),
                    _ => continue,
                };
                let id = format!("{impl_id}::{name}");
                if self.kinds.contains_key(&id) {
                    continue;
                }
                self.register_assoc(&owner, &name, &id, kind);
                // Trait impl members are also reachable as `Type::name`.
                if trait_res.is_some()
                    && let Some(sid) = &self_id
                {
                    self.assoc
                        .entry(sid.clone())
                        .or_default()
                        .push((name.clone(), id.clone()));
                }
                self.pending.push(Pending {
                    id,
                    name,
                    module: raw.module.clone(),
                    file: raw.file.clone(),
                    node,
                    parent: Some(owner.clone()),
                    self_ty: self_id.clone(),
                    impl_ctx: Some(ctx.clone()),
                    outer_generics: generic_names(&item.generics),
                });
            }
        }
    }

    fn crate_of(id: &str) -> &str {
        id.split("::").next().unwrap()
    }

    fn deps_of(&self, module: &str) -> Option<&BTreeSet<String>> {
        self.crate_info.get(Self::crate_of(module)).map(|(d, _)| d)
    }

    /// A crate name usable in paths of `module`'s crate other than the
    /// analyzed crates: a declared dependency or `std`/`core`/`alloc`.
    fn is_extern_crate(&self, module: &str, name: &str) -> bool {
        BUILTIN_CRATES.contains(&name) || self.deps_of(module).is_some_and(|d| d.contains(name))
    }

    /// Resolves a path as written in `module`. `ns` is the name space of the
    /// last segment, if the context tells (a type or an expression).
    /// Returns `None` whenever the target is not certain.
    fn resolve(
        &self,
        module: &str,
        segs: &[String],
        self_ty: Option<&str>,
        ns: Option<Ns>,
    ) -> Option<Res> {
        self.resolve_depth(module, segs, self_ty, ns, 0)
    }

    fn resolve_depth(
        &self,
        module: &str,
        segs: &[String],
        self_ty: Option<&str>,
        ns: Option<Ns>,
        depth: usize,
    ) -> Option<Res> {
        if depth > 12 || segs.is_empty() {
            return None;
        }
        let first = segs[0].as_str();
        let first_ns = if segs.len() == 1 { ns } else { Some(Ns::Type) };
        let mut idx = 1;
        let mut cur: Res = match first {
            "crate" => Res {
                id: Some(Self::crate_of(module).to_string()),
                path: Self::crate_of(module).to_string(),
                display_start: 1,
            },
            "self" => Res {
                id: Some(module.to_string()),
                path: module.to_string(),
                display_start: 1,
            },
            "super" => {
                let mut m = module.to_string();
                let mut i = 0;
                while segs.get(i).map(String::as_str) == Some("super") {
                    m = self.modules.get(&m)?.parent.clone()?;
                    i += 1;
                }
                idx = i;
                Res {
                    id: Some(m.clone()),
                    path: m,
                    display_start: i,
                }
            }
            "Self" => {
                let id = self_ty?.to_string();
                Res {
                    id: Some(id.clone()),
                    path: id,
                    display_start: 0,
                }
            }
            _ => match self.lookup_name(module, first, first_ns, depth) {
                Some(r) => r,
                None if self.has_opaque_glob(module, depth + 1) => {
                    // A glob import we can't see into might bring this name.
                    return None;
                }
                None if first_ns != Some(Ns::Value) && self.crate_roots.contains(first) => Res {
                    id: Some(first.to_string()),
                    path: first.to_string(),
                    display_start: 1,
                },
                None if segs.len() > 1 && self.is_extern_crate(module, first) => Res {
                    id: None,
                    path: first.to_string(),
                    display_start: 0,
                },
                None => {
                    let edition = self
                        .crate_info
                        .get(Self::crate_of(module))
                        .and_then(|(_, e)| *e);
                    let (_, path, _, _) = PRELUDE.iter().find(|(n, _, pns, since)| {
                        *n == first
                            && first_ns.is_none_or(|ns| ns == *pns)
                            // Unknown edition: only names in every prelude.
                            && edition.unwrap_or(2015) >= *since
                    })?;
                    Res {
                        id: None,
                        path: path.to_string(),
                        display_start: 0,
                    }
                }
            },
        };
        let mut reached_type = None;
        while idx < segs.len() {
            let seg = &segs[idx];
            let last = idx == segs.len() - 1;
            match cur.id.as_deref() {
                Some(id) if self.kinds.get(id) == Some(&Kind::Mod) => {
                    let seg_ns = if last { ns } else { Some(Ns::Type) };
                    let r = self.lookup_name(id, seg, seg_ns, depth + 1)?;
                    cur = Res {
                        display_start: idx,
                        ..r
                    };
                }
                Some(id) if self.kinds.get(id).is_some_and(|k| k.is_type_like()) => {
                    reached_type.get_or_insert(idx - 1);
                    if let Some((_, member)) = self
                        .assoc
                        .get(id)
                        .and_then(|members| members.iter().find(|(n, _)| n == seg))
                    {
                        cur = Res {
                            id: Some(member.clone()),
                            path: member.clone(),
                            display_start: 0,
                        };
                    } else if self
                        .enum_variants
                        .get(id)
                        .is_some_and(|v| v.iter().any(|v| v == seg))
                    {
                        // Variants refer to their enum.
                        cur.path = format!("{id}::{seg}");
                    } else {
                        // An associated item we don't see (from a derive or a
                        // trait impl elsewhere): it is still the type's.
                        cur.path = format!("{id}::{seg}");
                    }
                }
                Some(_) => return None,
                None => {
                    cur.path = format!("{}::{seg}", cur.path);
                }
            }
            idx += 1;
        }
        cur.display_start = match (&cur.id, reached_type) {
            (_, Some(t)) => t,
            (Some(_), None) => segs.len() - 1,
            (None, None) => {
                // External path: keep from the first capitalized segment.
                segs.iter()
                    .position(|s| s.starts_with(|c: char| c.is_uppercase()))
                    .unwrap_or(segs.len() - 1)
            }
        };
        if segs[0] == "Self" {
            cur.display_start = 0;
        }
        Some(cur)
    }

    /// Looks a name up in a module: its definitions, then its imports, then
    /// its glob imports (only what they make visible).
    fn lookup_name(&self, module: &str, name: &str, ns: Option<Ns>, depth: usize) -> Option<Res> {
        if depth > 12 {
            return None;
        }
        let m = self.modules.get(module)?;
        if let Some(ids) = m.defs.get(name)
            && let Some(id) = ids.iter().find(|id| {
                self.kinds
                    .get(*id)
                    .is_some_and(|k| in_ns(*k, self.value_structs.contains(*id), ns))
            })
        {
            return Some(Res {
                path: id.clone(),
                id: Some(id.clone()),
                display_start: 0,
            });
        }
        for u in &m.uses {
            if u.alias.as_deref() == Some(name) {
                if u.path.len() == 1 && u.path[0] == name {
                    // `use foo;` / `extern crate foo;`
                    if self.crate_roots.contains(name) {
                        return Some(Res {
                            id: Some(name.to_string()),
                            path: name.to_string(),
                            display_start: 0,
                        });
                    }
                    if self.is_extern_crate(module, name) {
                        return Some(Res {
                            id: None,
                            path: name.to_string(),
                            display_start: 0,
                        });
                    }
                    return None;
                }
                if let Some(r) = self.resolve_depth(module, &u.path, None, ns, depth + 1) {
                    return Some(r);
                }
            }
        }
        for u in &m.uses {
            if u.alias.is_some() {
                continue;
            }
            let Some(Res {
                id: Some(target), ..
            }) = self.resolve_depth(module, &u.path, None, Some(Ns::Type), depth + 1)
            else {
                continue;
            };
            if target == module {
                continue;
            }
            if self.kinds.get(&target) == Some(&Kind::Enum) {
                if ns != Some(Ns::Type)
                    && self
                        .enum_variants
                        .get(&target)
                        .is_some_and(|v| v.iter().any(|v| v == name))
                {
                    return Some(Res {
                        id: Some(target.clone()),
                        path: format!("{target}::{name}"),
                        display_start: 0,
                    });
                }
                continue;
            }
            if let Some(r) = self.lookup_name(&target, name, ns, depth + 1) {
                // A glob only imports what is visible to the importing module.
                let visible = r.id.as_ref().is_none_or(|id| {
                    self.def_vis.get(id).is_none_or(|scope| {
                        scope
                            .as_ref()
                            .is_none_or(|s| crate::analyze::within(module, s))
                    })
                });
                if visible {
                    return Some(r);
                }
            }
        }
        None
    }

    /// Whether a module has glob imports of something whose contents we
    /// can't see (another crate, or a path we can't resolve).
    fn has_opaque_glob(&self, module: &str, depth: usize) -> bool {
        if depth > 8 {
            return true;
        }
        let Some(m) = self.modules.get(module) else {
            return false;
        };
        m.uses.iter().filter(|u| u.alias.is_none()).any(|u| {
            match self.resolve_depth(module, &u.path, None, Some(Ns::Type), depth + 1) {
                Some(Res {
                    id: Some(target), ..
                }) => {
                    target != module
                        && self.kinds.get(&target) == Some(&Kind::Mod)
                        && self.has_opaque_glob(&target, depth + 1)
                }
                _ => true,
            }
        })
    }

    fn vis_scope(&self, module: &str, vis: &Visibility) -> (Option<String>, String) {
        match vis {
            Visibility::Public(_) => (None, "pub".into()),
            Visibility::Inherited => (Some(module.to_string()), "private".into()),
            Visibility::Restricted(r) => {
                let segs = path_segments(&r.path);
                let label = format!(
                    "pub({}{})",
                    if r.in_token.is_some() { "in " } else { "" },
                    segs.join("::")
                );
                let scope = match self.resolve(module, &segs, None, Some(Ns::Type)) {
                    Some(Res { id: Some(id), .. }) => id,
                    _ => Self::crate_of(module).to_string(),
                };
                (Some(scope), label)
            }
        }
    }

    /// Pass 3: signatures, docs, references and edges.
    fn build_items(&mut self) -> BTreeMap<String, ItemData> {
        let pending = std::mem::take(&mut self.pending);
        let mut items = BTreeMap::new();
        let mut reexports: Vec<(String, String)> = Vec::new();
        for (module, data) in &self.modules {
            for u in &data.uses {
                // `pub use path::Item;` and `pub use path::*;`
                if !u.public {
                    continue;
                }
                let ns = if u.alias.is_some() {
                    None
                } else {
                    Some(Ns::Type)
                };
                if let Some(Res { id: Some(id), .. }) = self.resolve(module, &u.path, None, ns) {
                    if u.alias.is_some() {
                        reexports.push((module.clone(), id));
                    } else if self.kinds.get(&id) == Some(&Kind::Mod) {
                        for child in self.children.get(&id).into_iter().flatten() {
                            if self.def_vis.get(child).is_some_and(|v| v.is_none()) {
                                reexports.push((module.clone(), child.clone()));
                            }
                        }
                    }
                }
            }
        }
        for p in pending {
            if items.contains_key(&p.id) {
                continue;
            }
            if let Some(item) = self.build_item(&p) {
                items.insert(p.id.clone(), item);
            }
        }
        // Public reachability.
        let ids: Vec<String> = items.keys().cloned().collect();
        let mut memo: HashMap<String, bool> = HashMap::new();
        for id in &ids {
            let public = reachable(id, &items, &mut memo);
            items.get_mut(id).unwrap().info.public = public;
        }
        // Re-exports from public modules make their targets public, and
        // with them everything public inside; repeat until nothing changes,
        // since a module can itself become public through a re-export.
        fn mark_public(id: &str, items: &mut BTreeMap<String, ItemData>) {
            let Some(item) = items.get_mut(id) else {
                return;
            };
            if item.info.public {
                return;
            }
            item.info.public = true;
            for child in item.info.children.clone() {
                if items
                    .get(&child)
                    .is_some_and(|c| c.info.vis_scope.is_none())
                {
                    mark_public(&child, items);
                }
            }
        }
        loop {
            let pending: Vec<String> = reexports
                .iter()
                .filter(|(module, target)| {
                    items.get(module).is_some_and(|m| m.info.public)
                        && items.get(target).is_some_and(|t| !t.info.public)
                })
                .map(|(_, target)| target.clone())
                .collect();
            if pending.is_empty() {
                break;
            }
            for target in pending {
                mark_public(&target, &mut items);
            }
        }
        items
    }

    /// Rustdoc-style signature and normalized body of one item. Paths are
    /// resolved (and recorded) by the two collectors along the way.
    fn render_node(
        &self,
        p: &Pending,
        sig_col: &mut Collector,
        body_col: &mut Collector,
    ) -> Option<Rendered> {
        let mut sig_public = None;
        let mut use_ctx = UseCtx::Signature;
        let (attrs, vis, sig, body_norm, span): (Vec<Attribute>, Visibility, String, String, _) =
            match &p.node {
                Node::Mod { attrs, vis } => (
                    attrs.clone(),
                    vis.clone(),
                    if p.id == p.module {
                        format!("crate {}", p.name)
                    } else {
                        format!("{}mod {}", vis_prefix(vis), p.name)
                    },
                    String::new(),
                    None,
                ),
                Node::Item(item) => {
                    let span = item.span();
                    match item {
                        Item::Fn(f) => {
                            let mut f = f.clone();
                            sig_col.visit_signature_mut(&mut f.sig);
                            body_col.visit_block_mut(&mut f.block);
                            let body = norm_block(&f.block);
                            let attrs = std::mem::take(&mut f.attrs);
                            f.attrs = shown_attrs(&attrs);
                            f.block = Box::new(parse_quote!({}));
                            let vis = f.vis.clone();
                            (
                                attrs,
                                vis,
                                strip_body(&unparse(Item::Fn(f))),
                                body,
                                Some(span),
                            )
                        }
                        Item::Struct(s) => {
                            use_ctx = UseCtx::Field;
                            let mut s = s.clone();
                            let attrs = std::mem::take(&mut s.attrs);
                            s.attrs = shown_attrs(&attrs);
                            strip_field_docs(&mut s.fields);
                            sig_col.visit_item_struct_mut(&mut s);
                            let full = unparse(Item::Struct(s.clone()));
                            if hide_private_fields(&mut s.fields) {
                                sig_public =
                                    Some(fix_private_marker(&unparse(Item::Struct(s.clone()))));
                            }
                            let vis = s.vis.clone();
                            (
                                attrs,
                                vis,
                                full.trim().to_string(),
                                String::new(),
                                Some(span),
                            )
                        }
                        Item::Union(u) => {
                            use_ctx = UseCtx::Field;
                            let mut u = u.clone();
                            let attrs = std::mem::take(&mut u.attrs);
                            u.attrs = shown_attrs(&attrs);
                            for f in u.fields.named.iter_mut() {
                                f.attrs.retain(|a| !a.path().is_ident("doc"));
                            }
                            sig_col.visit_item_union_mut(&mut u);
                            let vis = u.vis.clone();
                            (
                                attrs,
                                vis,
                                unparse(Item::Union(u)).trim().to_string(),
                                String::new(),
                                Some(span),
                            )
                        }
                        Item::Enum(e) => {
                            use_ctx = UseCtx::Field;
                            let mut e = e.clone();
                            let attrs = std::mem::take(&mut e.attrs);
                            e.attrs = shown_attrs(&attrs);
                            for v in e.variants.iter_mut() {
                                v.attrs.retain(|a| !a.path().is_ident("doc"));
                                strip_field_docs(&mut v.fields);
                            }
                            sig_col.visit_item_enum_mut(&mut e);
                            let vis = e.vis.clone();
                            (
                                attrs,
                                vis,
                                unparse(Item::Enum(e)).trim().to_string(),
                                String::new(),
                                Some(span),
                            )
                        }
                        Item::Trait(t) => {
                            let mut t = t.clone();
                            let attrs = std::mem::take(&mut t.attrs);
                            t.attrs = shown_attrs(&attrs);
                            for member in t.items.iter_mut() {
                                match member {
                                    syn::TraitItem::Fn(f) => {
                                        f.attrs.retain(|a| !a.path().is_ident("doc"));
                                        if f.default.is_some() {
                                            f.default = Some(parse_quote!({}));
                                            f.semi_token = None;
                                        }
                                    }
                                    syn::TraitItem::Const(c) => {
                                        c.attrs.retain(|a| !a.path().is_ident("doc"))
                                    }
                                    syn::TraitItem::Type(ty) => {
                                        ty.attrs.retain(|a| !a.path().is_ident("doc"))
                                    }
                                    _ => {}
                                }
                            }
                            sig_col.visit_item_trait_mut(&mut t);
                            let vis = t.vis.clone();
                            let text = unparse(Item::Trait(t)).replace(" {}\n", " { ... }\n");
                            (
                                attrs,
                                vis,
                                text.trim().to_string(),
                                String::new(),
                                Some(span),
                            )
                        }
                        Item::Const(c) => {
                            let mut c = c.clone();
                            let attrs = std::mem::take(&mut c.attrs);
                            sig_col.visit_type_mut(&mut c.ty);
                            body_col.visit_expr_mut(&mut c.expr);
                            let body = norm_expr(&c.expr);
                            shorten_expr(&mut c.expr);
                            let vis = c.vis.clone();
                            (
                                attrs,
                                vis,
                                unparse(Item::Const(c)).trim().to_string(),
                                body,
                                Some(span),
                            )
                        }
                        Item::Static(s) => {
                            let mut s = s.clone();
                            let attrs = std::mem::take(&mut s.attrs);
                            sig_col.visit_type_mut(&mut s.ty);
                            body_col.visit_expr_mut(&mut s.expr);
                            let body = norm_expr(&s.expr);
                            shorten_expr(&mut s.expr);
                            let vis = s.vis.clone();
                            (
                                attrs,
                                vis,
                                unparse(Item::Static(s)).trim().to_string(),
                                body,
                                Some(span),
                            )
                        }
                        Item::Type(t) => {
                            let mut t = t.clone();
                            let attrs = std::mem::take(&mut t.attrs);
                            sig_col.visit_item_type_mut(&mut t);
                            let vis = t.vis.clone();
                            (
                                attrs,
                                vis,
                                unparse(Item::Type(t)).trim().to_string(),
                                String::new(),
                                Some(span),
                            )
                        }
                        Item::Macro(m) => {
                            let exported =
                                m.attrs.iter().any(|a| a.path().is_ident("macro_export"));
                            let vis = if exported {
                                parse_quote!(pub)
                            } else {
                                Visibility::Inherited
                            };
                            (
                                m.attrs.clone(),
                                vis,
                                format!("macro_rules! {} {{ ... }}", p.name),
                                m.mac.tokens.to_string(),
                                Some(span),
                            )
                        }
                        Item::Impl(i) => {
                            use_ctx = UseCtx::Impl;
                            let mut i = i.clone();
                            let attrs = std::mem::take(&mut i.attrs);
                            i.items.clear();
                            sig_col.visit_item_impl_mut(&mut i);
                            (
                                attrs,
                                parse_quote!(pub),
                                strip_body(&unparse(Item::Impl(i))),
                                String::new(),
                                Some(span),
                            )
                        }
                        _ => return None,
                    }
                }
                Node::ImplFn(f) => {
                    let span = f.span();
                    let mut sig_ = f.sig.clone();
                    sig_col.visit_signature_mut(&mut sig_);
                    let mut block = f.block.clone();
                    body_col.visit_block_mut(&mut block);
                    let shown = shown_attrs(&f.attrs);
                    let vis = &f.vis;
                    let text = strip_body(&unparse(parse_quote!(#(#shown)* #vis #sig_ {})));
                    (
                        f.attrs.clone(),
                        f.vis.clone(),
                        text,
                        norm_block(&block),
                        Some(span),
                    )
                }
                Node::TraitFn(f) => {
                    let span = f.span();
                    let mut sig_ = f.sig.clone();
                    sig_col.visit_signature_mut(&mut sig_);
                    let mut body = String::new();
                    if let Some(block) = &f.default {
                        let mut block = block.clone();
                        body_col.visit_block_mut(&mut block);
                        body = norm_block(&block);
                    }
                    let mut text = strip_body(&unparse(parse_quote!(#sig_ {})));
                    text.push_str(if f.default.is_some() { " { ... }" } else { ";" });
                    (f.attrs.clone(), parse_quote!(pub), text, body, Some(span))
                }
                Node::ImplConst(c) => {
                    let span = c.span();
                    let mut ty = c.ty.clone();
                    sig_col.visit_type_mut(&mut ty);
                    let mut expr = c.expr.clone();
                    body_col.visit_expr_mut(&mut expr);
                    let (vis, ident) = (&c.vis, &c.ident);
                    let text = unparse(parse_quote!(#vis const #ident: #ty = _;));
                    (
                        c.attrs.clone(),
                        c.vis.clone(),
                        text.trim().to_string(),
                        norm_expr(&expr),
                        Some(span),
                    )
                }
                Node::ImplType(t) => {
                    let span = t.span();
                    let mut ty = t.ty.clone();
                    sig_col.visit_type_mut(&mut ty);
                    let (vis, ident, generics) = (&t.vis, &t.ident, &t.generics);
                    let text = unparse(parse_quote!(#vis type #ident #generics = #ty;));
                    (
                        t.attrs.clone(),
                        t.vis.clone(),
                        text.trim().to_string(),
                        String::new(),
                        Some(span),
                    )
                }
                Node::TraitConst(c) => {
                    let span = c.span();
                    let mut ty = c.ty.clone();
                    sig_col.visit_type_mut(&mut ty);
                    let ident = &c.ident;
                    let text = format!("const {ident}: {};", unparse_type(&ty));
                    let body = c
                        .default
                        .as_ref()
                        .map(|(_, e)| norm_expr(e))
                        .unwrap_or_default();
                    (c.attrs.clone(), parse_quote!(pub), text, body, Some(span))
                }
                Node::TraitType(t) => {
                    let span = t.span();
                    let mut t2 = t.clone();
                    t2.attrs.clear();
                    sig_col.visit_trait_item_type_mut(&mut t2);
                    let text = t2.to_token_stream().to_string();
                    let text = unparse(parse_quote!(trait __X { #text }));
                    let text = text
                        .trim()
                        .trim_start_matches("trait __X {")
                        .trim_end_matches('}')
                        .trim()
                        .to_string();
                    (
                        t.attrs.clone(),
                        parse_quote!(pub),
                        text,
                        String::new(),
                        Some(span),
                    )
                }
            };
        Some(Rendered {
            attrs,
            vis,
            sig,
            sig_public,
            body_norm,
            span,
            use_ctx,
        })
    }

    fn build_item(&self, p: &Pending) -> Option<ItemData> {
        let kind = *self.kinds.get(&p.id)?;
        let mut sig_col = Collector::new(self, &p.module, p.self_ty.as_deref(), true);
        let mut body_col = Collector::new(self, &p.module, p.self_ty.as_deref(), true);
        // Generic parameters and local bindings hide items of the same name.
        let mut generics = p.outer_generics.clone();
        generics.extend(node_generics(&p.node));
        sig_col.shadow(generics.iter().cloned());
        body_col.shadow(generics);
        body_col.shadow(node_bindings(&p.node));
        let Rendered {
            attrs,
            vis,
            sig,
            sig_public,
            body_norm,
            span,
            use_ctx,
        } = self.render_node(p, &mut sig_col, &mut body_col)?;
        let docs = extract_docs(&attrs);
        let mut doc_refs = BTreeMap::new();
        // Reference-style definitions: `[name]: path::to::Item`.
        let mut links: Vec<(String, String)> = Vec::new();
        for line in docs.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix('[')
                && let Some((name, target)) = rest.split_once("]:")
            {
                links.push((name.to_string(), target.trim().to_string()));
            }
        }
        links.extend(intra_doc_links(&docs).into_iter().map(|l| (l.clone(), l)));
        for (key, link) in links {
            let cleaned = link
                .trim_matches('`')
                .trim_end_matches("()")
                .trim_end_matches('!')
                .to_string();
            let cleaned = cleaned
                .split_once('@')
                .map(|(_, r)| r.to_string())
                .unwrap_or(cleaned);
            let segs: Vec<String> = cleaned.split("::").map(str::to_string).collect();
            let scope = if kind == Kind::Mod { &p.id } else { &p.module };
            if link.trim_matches('`').ends_with('!') {
                // Macros live in their own namespace; exported ones anywhere in the crate.
                let suffix = format!("::{cleaned}");
                if let Some(id) = self
                    .kinds
                    .iter()
                    .filter(|(id, k)| **k == Kind::Macro && id.ends_with(&suffix))
                    .map(|(id, _)| id)
                    .min()
                {
                    doc_refs.insert(
                        key,
                        Ref {
                            id: Some(id.clone()),
                            path: id.clone(),
                        },
                    );
                    continue;
                }
            }
            if segs.iter().all(|s| is_ident(s))
                && let Some(r) = self.resolve(scope, &segs, p.self_ty.as_deref(), None)
            {
                doc_refs.insert(
                    key,
                    Ref {
                        id: r.id,
                        path: r.path,
                    },
                );
            }
        }

        let (vis_scope, vis_label, vis_parent) = match (&p.node, &p.impl_ctx) {
            (Node::Mod { .. }, _) if p.id == p.module => (None, "pub".to_string(), None),
            (Node::Mod { .. }, _) => {
                let (s, l) = self.vis_scope(&p.module, &vis);
                (s, l, Some(p.module.clone()))
            }
            (_, Some(ctx)) if ctx.trait_ref.is_some() => (
                None,
                "pub".to_string(),
                Some(ctx.vis_parent.clone().unwrap_or_else(|| p.module.clone())),
            ),
            (_, Some(ctx)) => {
                let (s, l) = self.vis_scope(&p.module, &vis);
                (
                    s,
                    l,
                    Some(ctx.vis_parent.clone().unwrap_or_else(|| p.module.clone())),
                )
            }
            (Node::TraitFn(_) | Node::TraitConst(_) | Node::TraitType(_), _) => {
                (None, "pub".to_string(), p.parent.clone())
            }
            _ => {
                let (s, l) = self.vis_scope(&p.module, &vis);
                (s, l, Some(p.module.clone()))
            }
        };

        let (file, line, source) = match span {
            Some(span) => {
                let start = span.start().line.max(1);
                let end = span.end().line.max(start);
                let lines = &p.file.lines;
                let text = lines
                    .get(start - 1..end.min(lines.len()))
                    .map(|l| l.join("\n"))
                    .unwrap_or_default();
                (p.file.rel.clone(), start, text)
            }
            None => (p.file.rel.clone(), 1, String::new()),
        };

        // Calls: path calls outside macros, resolved by Rust's rules. Method
        // calls and calls inside macros need type information; see
        // `semantic`.
        let mut calls = BTreeSet::new();
        for site in &body_col.sites {
            if let Some(target) = &site.target
                && !site.in_macro
                && *target != p.id
            {
                calls.insert(Call { to: target.clone() });
            }
        }
        let unresolved_calls = body_col
            .sites
            .iter()
            .filter(|s| s.method || s.in_macro || !s.known)
            .count() as u32;
        let mut uses = BTreeSet::new();
        for (target, ctx) in sig_col
            .targets
            .iter()
            .map(|t| (t, use_ctx))
            .chain(body_col.targets.iter().map(|t| (t, UseCtx::Body)))
        {
            if *target == p.id || calls.iter().any(|c| c.to == *target) {
                continue;
            }
            match self.kinds.get(target) {
                None | Some(Kind::Mod) => {}
                // A function named without being called (e.g. passed as a
                // value) is a use.
                Some(_) => {
                    uses.insert(Use {
                        to: target.clone(),
                        ctx,
                    });
                }
            }
        }
        // An impl uses its self type.
        if kind == Kind::Impl
            && let Some(ctx) = &p.impl_ctx
            && let Some(t) = &ctx.self_ty
        {
            uses.insert(Use {
                to: t.clone(),
                ctx: UseCtx::Impl,
            });
        }

        let sig_norm = format!("{sig}|{:?}", sig_col.refs);
        let body_norm = format!("{body_norm}|{:?}", body_col.refs);
        let mut children = self.children.get(&p.id).cloned().unwrap_or_default();
        children.dedup();

        let name_pos = match &p.node {
            Node::Item(Item::Fn(f)) => Some(f.sig.ident.span().start().into()),
            Node::ImplFn(f) => Some(f.sig.ident.span().start().into()),
            Node::TraitFn(f) => Some(f.sig.ident.span().start().into()),
            _ => None,
        };
        let info = ItemInfo {
            id: p.id.clone(),
            kind,
            name: p.name.clone(),
            module: p.module.clone(),
            vis_scope,
            vis_label,
            vis_parent,
            public: false,
            sig,
            sig_public,
            docs,
            sig_refs: sig_col.refs,
            body_refs: body_col.refs,
            doc_refs,
            parent: p.parent.clone(),
            impl_header: p.impl_ctx.as_ref().map(|c| c.header.clone()),
            trait_ref: p.impl_ctx.as_ref().and_then(|c| c.trait_ref.clone()),
            self_ty: p.self_ty.clone(),
            children,
            file,
            line,
            calls,
            uses,
            unresolved_calls,
            site_refs: BTreeMap::new(),
        };
        Some(ItemData {
            info,
            sig_norm,
            body_norm,
            source,
            name_pos,
            call_sites: body_col.sites,
        })
    }
}

fn reachable(
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
        Some(item) => {
            item.info.vis_scope.is_none()
                && match &item.info.vis_parent {
                    None => true,
                    Some(parent) => reachable(parent, items, memo),
                }
        }
    };
    memo.insert(id.to_string(), r);
    r
}

/// Walks a syntax tree, resolving every path. With `normalize`, paths are
/// shortened to the name a reader needs (`crate::a::Foo` → `Foo`).
///
/// Only certain resolutions are recorded: names that could refer to a
/// local variable, a generic parameter or an item declared inside the
/// body are left alone, and a name that resolves differently in two places
/// of one item is not linked at all.
struct Collector<'a> {
    an: &'a Analyzer,
    module: &'a str,
    self_ty: Option<&'a str>,
    normalize: bool,
    refs: BTreeMap<String, Ref>,
    /// Display keys that resolved to different targets.
    ambiguous: BTreeSet<String>,
    /// Items referred to (outside macro invocations).
    targets: BTreeSet<String>,
    /// Names bound locally or as generic parameters.
    shadowed: std::collections::HashSet<String>,
    sites: Vec<CallSite>,
    in_macro: bool,
}

impl<'a> Collector<'a> {
    fn new(an: &'a Analyzer, module: &'a str, self_ty: Option<&'a str>, normalize: bool) -> Self {
        Collector {
            an,
            module,
            self_ty,
            normalize,
            refs: BTreeMap::new(),
            ambiguous: BTreeSet::new(),
            targets: BTreeSet::new(),
            shadowed: Default::default(),
            sites: Vec::new(),
            in_macro: false,
        }
    }

    fn shadow(&mut self, names: impl IntoIterator<Item = String>) {
        self.shadowed.extend(names);
    }

    /// Resolves a path unless its first name is shadowed.
    fn resolve(&self, segs: &[String], ns: Option<Ns>) -> Option<Res> {
        if segs.is_empty() || self.shadowed.contains(&segs[0]) {
            return None;
        }
        if segs.len() == 1 && matches!(segs[0].as_str(), "self" | "crate" | "super") {
            return None;
        }
        self.an.resolve(self.module, segs, self.self_ty, ns)
    }

    fn handle_path(&mut self, path: &mut syn::Path, ns: Option<Ns>) {
        let segs = path_segments(path);
        let Some(res) = self.resolve(&segs, ns) else {
            return;
        };
        let start = res.display_start.min(segs.len() - 1);
        let r = Ref {
            id: res.id.clone(),
            path: res.path.clone(),
        };
        if self.normalize && start > 0 {
            path.leading_colon = None;
            path.segments = path.segments.iter().skip(start).cloned().collect();
        }
        let key = segs[start..].join("::");
        match self.refs.get(&key) {
            _ if self.ambiguous.contains(&key) => {}
            Some(existing) if existing.path != r.path => {
                self.refs.remove(&key);
                self.ambiguous.insert(key);
            }
            _ => {
                self.refs.insert(key, r);
            }
        }
        if let Some(id) = res.id
            && !self.in_macro
        {
            self.targets.insert(id);
        }
    }

    fn visit_macro_tokens(&mut self, tokens: &TokenStream) {
        use syn::parse::Parser;
        let parser = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated;
        let normalize = std::mem::replace(&mut self.normalize, false);
        let in_macro = std::mem::replace(&mut self.in_macro, true);
        if let Ok(exprs) = parser.parse2(tokens.clone()) {
            for mut expr in exprs {
                self.visit_expr_mut(&mut expr);
            }
        } else if let Ok(mut block) = syn::parse2::<syn::Block>(quote::quote!({ #tokens })) {
            self.visit_block_mut(&mut block);
        }
        self.normalize = normalize;
        self.in_macro = in_macro;
    }
}

impl VisitMut for Collector<'_> {
    fn visit_attribute_mut(&mut self, _: &mut Attribute) {}

    fn visit_visibility_mut(&mut self, _: &mut Visibility) {}

    /// Items nested in a body are not part of it.
    fn visit_item_mut(&mut self, _: &mut Item) {}

    fn visit_path_mut(&mut self, path: &mut syn::Path) {
        visit_mut::visit_path_mut(self, path);
        self.handle_path(path, None);
    }

    fn visit_type_path_mut(&mut self, tp: &mut syn::TypePath) {
        if let Some(q) = &mut tp.qself {
            self.visit_type_mut(&mut q.ty);
            return;
        }
        visit_mut::visit_path_mut(self, &mut tp.path);
        self.handle_path(&mut tp.path, Some(Ns::Type));
    }

    fn visit_expr_path_mut(&mut self, ep: &mut syn::ExprPath) {
        if let Some(q) = &mut ep.qself {
            self.visit_type_mut(&mut q.ty);
            return;
        }
        visit_mut::visit_path_mut(self, &mut ep.path);
        self.handle_path(&mut ep.path, Some(Ns::Value));
    }

    fn visit_expr_struct_mut(&mut self, e: &mut syn::ExprStruct) {
        if let Some(q) = &mut e.qself {
            self.visit_type_mut(&mut q.ty);
        } else {
            visit_mut::visit_path_mut(self, &mut e.path);
            self.handle_path(&mut e.path, Some(Ns::Type));
        }
        for field in e.fields.iter_mut() {
            self.visit_expr_mut(&mut field.expr);
        }
        if let Some(rest) = &mut e.rest {
            self.visit_expr_mut(rest);
        }
    }

    fn visit_macro_mut(&mut self, mac: &mut syn::Macro) {
        let tokens = mac.tokens.clone();
        self.visit_macro_tokens(&tokens);
    }

    fn visit_expr_call_mut(&mut self, call: &mut syn::ExprCall) {
        // Resolve before visiting, which may shorten the path.
        let site = match &*call.func {
            Expr::Path(p) if p.qself.is_none() => {
                let segs = path_segments(&p.path);
                let shadowed = self.shadowed.contains(&segs[0]);
                (!shadowed).then(|| {
                    let res = self.resolve(&segs, Some(Ns::Value));
                    let target = res
                        .as_ref()
                        .and_then(|r| r.id.clone())
                        .filter(|id| self.an.kinds.get(id).is_some_and(|k| k.is_callable()));
                    let pos = p.path.segments.last().unwrap().ident.span().start().into();
                    CallSite {
                        pos,
                        method: false,
                        in_macro: self.in_macro,
                        // Resolved outside the analyzed items (e.g. `Some(..)`)
                        // or to a constructor: known, but not a call target.
                        known: res.is_some(),
                        target,
                    }
                })
            }
            _ => None,
        };
        visit_mut::visit_expr_call_mut(self, call);
        if let Some(site) = site {
            self.sites.push(site);
        }
    }

    fn visit_expr_method_call_mut(&mut self, call: &mut syn::ExprMethodCall) {
        visit_mut::visit_expr_method_call_mut(self, call);
        // Which method is called depends on the receiver's type: only type
        // inference (rust-analyzer) can tell.
        self.sites.push(CallSite {
            pos: call.method.span().start().into(),
            method: true,
            in_macro: self.in_macro,
            known: false,
            target: None,
        });
    }
}

/// Names bound in patterns (parameters, `let`, closures, `match` arms) and
/// items declared inside a body.
#[derive(Default)]
struct Bindings(Vec<String>);

impl<'ast> syn::visit::Visit<'ast> for Bindings {
    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        self.0.push(p.ident.to_string());
        syn::visit::visit_pat_ident(self, p);
    }

    fn visit_item(&mut self, item: &'ast Item) {
        let ident = match item {
            Item::Fn(i) => Some(&i.sig.ident),
            Item::Struct(i) => Some(&i.ident),
            Item::Enum(i) => Some(&i.ident),
            Item::Union(i) => Some(&i.ident),
            Item::Const(i) => Some(&i.ident),
            Item::Static(i) => Some(&i.ident),
            Item::Type(i) => Some(&i.ident),
            Item::Trait(i) => Some(&i.ident),
            Item::Mod(i) => Some(&i.ident),
            _ => None,
        };
        self.0.extend(ident.map(|i| i.to_string()));
        if let Item::Use(u) = item {
            let mut entries = Vec::new();
            flatten_use(&u.tree, Vec::new(), &mut entries);
            self.0
                .extend(entries.into_iter().filter_map(|(alias, _)| alias));
        }
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        use syn::parse::Parser;
        let parser = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated;
        if let Ok(exprs) = parser.parse2(mac.tokens.clone()) {
            for expr in &exprs {
                self.visit_expr(expr);
            }
        } else {
            let tokens = &mac.tokens;
            if let Ok(block) = syn::parse2::<syn::Block>(quote::quote!({ #tokens })) {
                self.visit_block(&block);
            }
        }
    }
}

/// Generic parameter names an item declares itself.
fn node_generics(node: &Node) -> Vec<String> {
    match node {
        Node::Item(item) => match item {
            Item::Fn(f) => generic_names(&f.sig.generics),
            Item::Struct(i) => generic_names(&i.generics),
            Item::Enum(i) => generic_names(&i.generics),
            Item::Union(i) => generic_names(&i.generics),
            Item::Trait(i) => generic_names(&i.generics),
            Item::Type(i) => generic_names(&i.generics),
            Item::Impl(i) => generic_names(&i.generics),
            _ => Vec::new(),
        },
        Node::ImplFn(f) => generic_names(&f.sig.generics),
        Node::TraitFn(f) => generic_names(&f.sig.generics),
        Node::ImplType(t) => generic_names(&t.generics),
        Node::TraitType(t) => generic_names(&t.generics),
        _ => Vec::new(),
    }
}

/// Names bound inside an item's signature and body.
fn node_bindings(node: &Node) -> Vec<String> {
    use syn::visit::Visit;
    let mut b = Bindings::default();
    match node {
        Node::Item(Item::Fn(f)) => {
            b.visit_signature(&f.sig);
            b.visit_block(&f.block);
        }
        Node::Item(Item::Const(c)) => b.visit_expr(&c.expr),
        Node::Item(Item::Static(s)) => b.visit_expr(&s.expr),
        Node::ImplFn(f) => {
            b.visit_signature(&f.sig);
            b.visit_block(&f.block);
        }
        Node::TraitFn(f) => {
            b.visit_signature(&f.sig);
            if let Some(block) = &f.default {
                b.visit_block(block);
            }
        }
        Node::ImplConst(c) => b.visit_expr(&c.expr),
        _ => {}
    }
    b.0
}

/// Type and const parameter names of some generics.
fn generic_names(g: &syn::Generics) -> Vec<String> {
    g.params
        .iter()
        .filter_map(|p| match p {
            syn::GenericParam::Type(t) => Some(t.ident.to_string()),
            syn::GenericParam::Const(c) => Some(c.ident.to_string()),
            syn::GenericParam::Lifetime(_) => None,
        })
        .collect()
}

fn path_segments(path: &syn::Path) -> Vec<String> {
    path.segments.iter().map(|s| s.ident.to_string()).collect()
}

fn flatten_use(
    tree: &syn::UseTree,
    prefix: Vec<String>,
    out: &mut Vec<(Option<String>, Vec<String>)>,
) {
    match tree {
        syn::UseTree::Path(p) => {
            let mut prefix = prefix;
            prefix.push(p.ident.to_string());
            flatten_use(&p.tree, prefix, out);
        }
        syn::UseTree::Name(n) => {
            let name = n.ident.to_string();
            let mut path = prefix;
            if name == "self" {
                if let Some(last) = path.last().cloned() {
                    out.push((Some(last), path));
                }
                return;
            }
            path.push(name.clone());
            out.push((Some(name), path));
        }
        syn::UseTree::Rename(r) => {
            let mut path = prefix;
            if r.ident != "self" {
                path.push(r.ident.to_string());
            }
            if r.rename != "_" {
                out.push((Some(r.rename.to_string()), path));
            }
        }
        syn::UseTree::Glob(_) => out.push((None, prefix)),
        syn::UseTree::Group(g) => {
            for t in &g.items {
                flatten_use(t, prefix.clone(), out);
            }
        }
    }
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

fn is_test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("test")
            || (a.path().is_ident("cfg") && a.meta.to_token_stream().to_string() == "cfg (test)")
    })
}

fn path_attr(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|a| match &a.meta {
        syn::Meta::NameValue(nv) if nv.path.is_ident("path") => match &nv.value {
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Some(s.value()),
            _ => None,
        },
        _ => None,
    })
}

fn extract_docs(attrs: &[Attribute]) -> String {
    let mut lines = Vec::new();
    for a in attrs {
        if let syn::Meta::NameValue(nv) = &a.meta
            && nv.path.is_ident("doc")
            && let Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
        {
            let value = s.value();
            if value.trim().is_empty() {
                lines.push(String::new());
            }
            for line in value.lines() {
                lines.push(line.strip_prefix(' ').unwrap_or(line).to_string());
            }
        }
    }
    lines.join("\n").trim().to_string()
}

/// Bracketed link targets in markdown that are not followed by `(…)`.
fn intra_doc_links(docs: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = docs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'['
            && let Some(len) = docs[i + 1..].find(']')
        {
            let inner = &docs[i + 1..i + 1 + len];
            let after = docs[i + 2 + len..].chars().next();
            if !inner.contains('[') && after != Some('(') && !inner.is_empty() {
                out.push(inner.to_string());
            }
            if after == Some('(')
                && let Some(close) = docs[i + 2 + len..].find(')')
            {
                let target = &docs[i + 3 + len..i + 2 + len + close];
                if !target.contains('/') && !target.contains('.') {
                    out.push(target.to_string());
                }
            }
            i += len + 2;
            continue;
        }
        i += 1;
    }
    out
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// Attributes rustdoc shows in a definition.
fn shown_attrs(attrs: &[Attribute]) -> Vec<Attribute> {
    attrs
        .iter()
        .filter(|a| {
            ["repr", "non_exhaustive", "must_use", "deprecated"]
                .iter()
                .any(|n| a.path().is_ident(n))
        })
        .cloned()
        .collect()
}

fn strip_field_docs(fields: &mut syn::Fields) {
    for f in fields.iter_mut() {
        f.attrs.retain(|a| !a.path().is_ident("doc"));
    }
}

/// Removes private fields from a definition. Returns whether anything was hidden.
fn hide_private_fields(fields: &mut syn::Fields) -> bool {
    match fields {
        syn::Fields::Named(named) => {
            let before = named.named.len();
            let kept: syn::punctuated::Punctuated<syn::Field, syn::Token![,]> = named
                .named
                .iter()
                .filter(|f| matches!(f.vis, Visibility::Public(_)))
                .cloned()
                .collect();
            if kept.len() == before {
                return false;
            }
            named.named = kept;
            named.named.push(parse_quote!(__private_fields__: ()));
            true
        }
        syn::Fields::Unnamed(unnamed) => {
            let mut changed = false;
            for f in unnamed.unnamed.iter_mut() {
                if !matches!(f.vis, Visibility::Public(_)) {
                    f.ty = parse_quote!(_);
                    changed = true;
                }
            }
            changed
        }
        syn::Fields::Unit => false,
    }
}

fn fix_private_marker(s: &str) -> String {
    s.replace("__private_fields__: (),", "/* private fields */")
        .trim()
        .to_string()
}

fn shorten_expr(expr: &mut Expr) {
    let text = expr.to_token_stream().to_string();
    if text.len() > 40 || text.contains('{') {
        *expr = parse_quote!(_);
    }
}

fn vis_prefix(vis: &Visibility) -> String {
    match vis {
        Visibility::Inherited => String::new(),
        v => format!("{} ", v.to_token_stream().to_string().replace(' ', "")),
    }
}

fn unparse(item: Item) -> String {
    let file: syn::File = parse_quote!(#item);
    prettyplease::unparse(&file)
}

fn unparse_type(ty: &syn::Type) -> String {
    let text = unparse(parse_quote!(type __X = #ty;));
    text.trim()
        .trim_start_matches("type __X = ")
        .trim_end_matches(';')
        .to_string()
}

/// `m` is `s` or nested inside it.
pub fn within(m: &str, s: &str) -> bool {
    m == s || (m.len() > s.len() + 2 && m.starts_with(s) && m[s.len()..].starts_with("::"))
}

/// Turns `fn foo() {}` into `fn foo()`.
fn strip_body(s: &str) -> String {
    let s = s.trim_end();
    s.strip_suffix("{}").unwrap_or(s).trim_end().to_string()
}

/// Canonical text of a block, independent of the original formatting.
fn norm_block(block: &syn::Block) -> String {
    unparse(parse_quote!(fn __f() #block))
}

fn norm_expr(expr: &Expr) -> String {
    unparse(parse_quote!(const _: () = #expr;))
}
