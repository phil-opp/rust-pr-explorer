//! Turns one source tree into a flat map of items with resolved names,
//! rustdoc-style signatures and call/use edges.

use crate::source::CrateSrc;
use proc_macro2::TokenStream;
use quote::ToTokens;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use syn::spanned::Spanned;
use syn::visit_mut::{self, VisitMut};
use syn::{Attribute, Expr, Item, Visibility, parse_quote};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Mod,
    Struct,
    Enum,
    Union,
    Trait,
    Fn,
    Method,
    TraitMethod,
    Const,
    Static,
    TypeAlias,
    Macro,
    Impl,
    AssocConst,
    AssocType,
}

impl Kind {
    fn is_type_like(self) -> bool {
        matches!(
            self,
            Kind::Struct | Kind::Enum | Kind::Union | Kind::Trait | Kind::TypeAlias
        )
    }

    pub fn is_callable(self) -> bool {
        matches!(self, Kind::Fn | Kind::Method | Kind::TraitMethod)
    }
}

/// What a name refers to.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ref {
    /// Item id if the name refers to an item of an analyzed crate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Fully qualified path.
    pub path: String,
    /// Set when the target was guessed (method calls without type info).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inferred: bool,
}

/// How an item uses another one.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum UseCtx {
    Signature,
    Field,
    Impl,
    Body,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Call {
    pub to: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inferred: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Use {
    pub to: String,
    pub ctx: UseCtx,
}

/// One item of one side (base or head) of the pull request.
#[derive(Clone, Debug, Serialize)]
pub struct ItemData {
    pub id: String,
    pub kind: Kind,
    pub name: String,
    /// Id of the module the item is defined in (for a module: its parent).
    pub module: String,
    /// `None` means `pub`; otherwise the module the item is visible in.
    pub vis_scope: Option<String>,
    pub vis_label: String,
    /// The item whose visibility additionally restricts this one (the
    /// module for free items, the type for methods).
    pub vis_parent: Option<String>,
    /// Reachable from outside the crate.
    pub public: bool,
    pub sig: String,
    /// Signature with private fields hidden, if different from `sig`.
    pub sig_public: Option<String>,
    pub docs: String,
    pub sig_refs: BTreeMap<String, Ref>,
    pub body_refs: BTreeMap<String, Ref>,
    pub doc_refs: BTreeMap<String, Ref>,
    /// Owning type or impl for associated items.
    pub parent: Option<String>,
    /// `impl` header for associated items, e.g. `impl Display for Parser`.
    pub impl_header: Option<String>,
    pub trait_ref: Option<Ref>,
    pub self_ty: Option<String>,
    pub children: Vec<String>,
    pub file: String,
    pub line: usize,
    pub calls: BTreeSet<Call>,
    pub uses: BTreeSet<Use>,
    #[serde(skip)]
    pub sig_norm: String,
    #[serde(skip)]
    pub body_norm: String,
    #[serde(skip)]
    pub source: String,
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
    /// Method name → method ids.
    method_names: HashMap<String, Vec<String>>,
    /// Associated item id → id of the trait it belongs to.
    member_trait: HashMap<String, String>,
    enum_variants: HashMap<String, Vec<String>>,
    children: HashMap<String, Vec<String>>,
    crate_roots: BTreeSet<String>,
    warnings: Vec<String>,
}

/// Method names too common in std to guess a crate-local target from.
const COMMON_STD_METHODS: &[&str] = &[
    "as_ref",
    "as_mut",
    "as_str",
    "borrow",
    "chars",
    "clear",
    "clone",
    "cloned",
    "collect",
    "contains",
    "contains_key",
    "copied",
    "count",
    "default",
    "drain",
    "entry",
    "eq",
    "extend",
    "filter",
    "find",
    "first",
    "flat_map",
    "fmt",
    "fold",
    "get",
    "get_mut",
    "hash",
    "insert",
    "into",
    "into_iter",
    "is_empty",
    "is_none",
    "is_some",
    "iter",
    "iter_mut",
    "join",
    "keys",
    "last",
    "len",
    "lines",
    "map",
    "map_err",
    "max",
    "min",
    "new",
    "next",
    "ok",
    "ok_or",
    "parse",
    "peek",
    "pop",
    "push",
    "push_str",
    "remove",
    "replace",
    "rev",
    "skip",
    "sort",
    "split",
    "take",
    "to_owned",
    "to_string",
    "trim",
    "unwrap",
    "unwrap_or",
    "values",
    "write",
    "zip",
];

const PRELUDE: &[(&str, &str)] = &[
    ("Option", "core::option::Option"),
    ("Some", "core::option::Option::Some"),
    ("None", "core::option::Option::None"),
    ("Result", "core::result::Result"),
    ("Ok", "core::result::Result::Ok"),
    ("Err", "core::result::Result::Err"),
    ("String", "alloc::string::String"),
    ("ToString", "alloc::string::ToString"),
    ("Vec", "alloc::vec::Vec"),
    ("Box", "alloc::boxed::Box"),
    ("Clone", "core::clone::Clone"),
    ("Copy", "core::marker::Copy"),
    ("Send", "core::marker::Send"),
    ("Sync", "core::marker::Sync"),
    ("Sized", "core::marker::Sized"),
    ("Default", "core::default::Default"),
    ("Drop", "core::ops::Drop"),
    ("Fn", "core::ops::Fn"),
    ("FnMut", "core::ops::FnMut"),
    ("FnOnce", "core::ops::FnOnce"),
    ("From", "core::convert::From"),
    ("Into", "core::convert::Into"),
    ("TryFrom", "core::convert::TryFrom"),
    ("TryInto", "core::convert::TryInto"),
    ("AsRef", "core::convert::AsRef"),
    ("AsMut", "core::convert::AsMut"),
    ("Iterator", "core::iter::Iterator"),
    ("IntoIterator", "core::iter::IntoIterator"),
    ("Extend", "core::iter::Extend"),
    ("PartialEq", "core::cmp::PartialEq"),
    ("Eq", "core::cmp::Eq"),
    ("PartialOrd", "core::cmp::PartialOrd"),
    ("Ord", "core::cmp::Ord"),
    ("ToOwned", "alloc::borrow::ToOwned"),
];

impl Analyzer {
    pub fn run(root: &Path, crates: &[CrateSrc]) -> Analysis {
        let mut an = Analyzer {
            root: root.to_path_buf(),
            modules: HashMap::new(),
            kinds: HashMap::new(),
            pending: Vec::new(),
            impls: Vec::new(),
            assoc: HashMap::new(),
            method_names: HashMap::new(),
            member_trait: HashMap::new(),
            enum_variants: HashMap::new(),
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

    fn define(&mut self, module: &str, name: &str, id: &str, kind: Kind) {
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

    /// Pass 1: module tree, definitions and imports.
    fn collect_items(&mut self, module: &str, items: &[Item], src: &Rc<SrcFile>, dir: &Path) {
        for item in items {
            if is_test_only(item_attrs(item)) {
                continue;
            }
            let push = |an: &mut Analyzer, name: String, kind: Kind| {
                let id = format!("{module}::{name}");
                an.define(module, &name, &id, kind);
                an.pending.push(Pending {
                    id,
                    name,
                    module: module.to_string(),
                    file: src.clone(),
                    node: Node::Item(item.clone()),
                    parent: None,
                    self_ty: None,
                    impl_ctx: None,
                });
            };
            match item {
                Item::Fn(f) => push(self, f.sig.ident.to_string(), Kind::Fn),
                Item::Struct(s) => push(self, s.ident.to_string(), Kind::Struct),
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
                        self.register_assoc(&trait_id, &name, &id, kind, Some(&trait_id));
                        self.pending.push(Pending {
                            id,
                            name,
                            module: module.to_string(),
                            file: src.clone(),
                            node,
                            parent: Some(trait_id.clone()),
                            self_ty: Some(trait_id.clone()),
                            impl_ctx: None,
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
                    self.define(module, &name, &id, Kind::Mod);
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
                    });
                    self.collect_items(&id, &child_items, &child_src, &child_dir);
                }
                _ => {}
            }
        }
    }

    fn register_assoc(
        &mut self,
        owner: &str,
        name: &str,
        id: &str,
        kind: Kind,
        trait_id: Option<&str>,
    ) {
        self.assoc
            .entry(owner.to_string())
            .or_default()
            .push((name.to_string(), id.to_string()));
        self.kinds.insert(id.to_string(), kind);
        if kind.is_callable() {
            self.method_names
                .entry(name.to_string())
                .or_default()
                .push(id.to_string());
        }
        if let Some(t) = trait_id {
            self.member_trait.insert(id.to_string(), t.to_string());
        }
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
                    .resolve(&raw.module, &path_segments(&tp.path), None)
                    .and_then(|r| r.id)
                    .filter(|id| self.kinds.get(id).is_some_and(|k| k.is_type_like())),
                _ => None,
            };
            let trait_res = item.trait_.as_ref().map(|(path, _)| {
                let segs = path_segments(path);
                self.resolve(&raw.module, &segs, None).unwrap_or(Res {
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

            let (owner, impl_id, trait_id) = match &trait_res {
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
                                inferred: false,
                            }),
                            self_ty: self_id.clone(),
                            vis_parent: self_id.clone(),
                        })),
                    });
                    (impl_id.clone(), impl_id, tr.id.clone())
                }
                None => {
                    let owner = match &self_id {
                        Some(id) => id.clone(),
                        None => format!("{}::<impl {self_display}>", raw.module),
                    };
                    (owner.clone(), owner, None)
                }
            };
            let ctx = Rc::new(ImplCtx {
                header,
                trait_ref: trait_res.as_ref().map(|tr| Ref {
                    id: tr.id.clone(),
                    path: tr.path.clone(),
                    inferred: false,
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
                self.register_assoc(&owner, &name, &id, kind, trait_id.as_deref());
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
                });
            }
        }
    }

    fn crate_of(id: &str) -> &str {
        id.split("::").next().unwrap()
    }

    /// Resolves a path as written in `module`.
    fn resolve(&self, module: &str, segs: &[String], self_ty: Option<&str>) -> Option<Res> {
        self.resolve_depth(module, segs, self_ty, 0)
    }

    fn resolve_depth(
        &self,
        module: &str,
        segs: &[String],
        self_ty: Option<&str>,
        depth: usize,
    ) -> Option<Res> {
        if depth > 12 || segs.is_empty() {
            return None;
        }
        let first = segs[0].as_str();
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
            _ => match self.lookup_name(module, first, depth) {
                Some(r) => r,
                None => {
                    if self.crate_roots.contains(first) {
                        Res {
                            id: Some(first.to_string()),
                            path: first.to_string(),
                            display_start: 1,
                        }
                    } else if let Some((_, p)) = PRELUDE.iter().find(|(n, _)| *n == first) {
                        Res {
                            id: None,
                            path: p.to_string(),
                            display_start: 0,
                        }
                    } else if segs.len() > 1 {
                        Res {
                            id: None,
                            path: first.to_string(),
                            display_start: 0,
                        }
                    } else {
                        return None;
                    }
                }
            },
        };
        let mut reached_type = None;
        while idx < segs.len() {
            let seg = &segs[idx];
            match cur.id.as_deref() {
                Some(id) if self.kinds.get(id) == Some(&Kind::Mod) => {
                    let r = self.lookup_name(id, seg, depth + 1)?;
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
                        // Unknown associated item (e.g. derived); keep the type.
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

    fn lookup_name(&self, module: &str, name: &str, depth: usize) -> Option<Res> {
        let m = self.modules.get(module)?;
        if let Some(ids) = m.defs.get(name) {
            let id = ids[0].clone();
            return Some(Res {
                path: id.clone(),
                id: Some(id),
                display_start: 0,
            });
        }
        for u in &m.uses {
            if u.alias.as_deref() == Some(name) {
                if u.path.len() == 1 && u.path[0] == name {
                    // `use foo;` of an external crate (or an extern crate).
                    if self.crate_roots.contains(name) {
                        return Some(Res {
                            id: Some(name.to_string()),
                            path: name.to_string(),
                            display_start: 0,
                        });
                    }
                    return Some(Res {
                        id: None,
                        path: name.to_string(),
                        display_start: 0,
                    });
                }
                if let Some(r) = self.resolve_depth(module, &u.path, None, depth + 1) {
                    return Some(r);
                }
            }
        }
        for u in &m.uses {
            if u.alias.is_none()
                && let Some(Res {
                    id: Some(target), ..
                }) = self.resolve_depth(module, &u.path, None, depth + 1)
                && target != module
                && let Some(r) = self.lookup_name(&target, name, depth + 1)
            {
                return Some(r);
            }
        }
        None
    }

    /// Guesses the target of a method call without type information.
    fn resolve_method(
        &self,
        name: &str,
        receiver_ty: Option<&str>,
        simple_receiver: bool,
    ) -> Option<Ref> {
        if let Some(t) = receiver_ty
            && let Some((_, id)) = self.assoc.get(t).and_then(|m| {
                m.iter()
                    .find(|(n, id)| n == name && self.kinds[id].is_callable())
            })
        {
            return Some(Ref {
                id: Some(id.clone()),
                path: id.clone(),
                inferred: false,
            });
        }
        if receiver_ty.is_some() && simple_receiver {
            // The receiver's type is known and has no such method (e.g. a
            // method from a derive or an external trait).
            return None;
        }
        if COMMON_STD_METHODS.contains(&name) {
            return None;
        }
        let candidates = self.method_names.get(name)?;
        let methods: Vec<&String> = candidates
            .iter()
            .filter(|id| self.kinds.get(*id) != Some(&Kind::Fn))
            .collect();
        let target = if methods.len() == 1 {
            methods[0].clone()
        } else {
            // All candidates implement the same trait method: use the declaration.
            let traits: BTreeSet<_> = methods
                .iter()
                .map(|id| self.member_trait.get(*id))
                .collect();
            match traits.into_iter().collect::<Vec<_>>().as_slice() {
                [Some(t)] => {
                    let decl = format!("{t}::{name}");
                    if self.kinds.contains_key(&decl) {
                        decl
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        };
        Some(Ref {
            id: Some(target.clone()),
            path: target,
            inferred: true,
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
                let scope = match self.resolve(module, &segs, None) {
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
                if u.public
                    && u.alias.is_some()
                    && let Some(Res { id: Some(id), .. }) = self.resolve(module, &u.path, None)
                {
                    reexports.push((module.clone(), id));
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
            items.get_mut(id).unwrap().public = public;
        }
        for (module, target) in reexports {
            if items.get(&module).is_some_and(|m| m.public)
                && let Some(item) = items.get_mut(&target)
            {
                item.public = true;
                for child in item.children.clone() {
                    if let Some(c) = items.get_mut(&child)
                        && c.vis_scope.is_none()
                    {
                        c.public = true;
                    }
                }
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
                            body_col.add_params(&f.sig);
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
                    body_col.add_params(&f.sig);
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
                    body_col.add_params(&f.sig);
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
                            inferred: false,
                        },
                    );
                    continue;
                }
            }
            if segs.iter().all(|s| is_ident(s))
                && let Some(r) = self.resolve(scope, &segs, p.self_ty.as_deref())
            {
                doc_refs.insert(
                    key,
                    Ref {
                        id: r.id,
                        path: r.path,
                        inferred: false,
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

        let mut calls = BTreeSet::new();
        let mut uses = BTreeSet::new();
        for (target, ctx) in sig_col
            .targets
            .iter()
            .map(|t| (t, use_ctx))
            .chain(body_col.targets.iter().map(|t| (t, UseCtx::Body)))
        {
            if *target == p.id {
                continue;
            }
            let Some(target_kind) = self.kinds.get(target) else {
                continue;
            };
            if target_kind.is_callable() && ctx == UseCtx::Body {
                calls.insert(Call {
                    to: target.clone(),
                    inferred: false,
                });
            } else if *target_kind != Kind::Mod {
                uses.insert(Use {
                    to: target.clone(),
                    ctx,
                });
            }
        }
        for target in &body_col.inferred_calls {
            if *target != p.id && !calls.iter().any(|c| c.to == *target) {
                calls.insert(Call {
                    to: target.clone(),
                    inferred: true,
                });
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
        let certain: Vec<_> = body_col.refs.iter().filter(|(_, r)| !r.inferred).collect();
        let body_norm = format!("{body_norm}|{certain:?}");
        let mut children = self.children.get(&p.id).cloned().unwrap_or_default();
        children.dedup();

        Some(ItemData {
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
            sig_norm,
            body_norm,
            source,
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
            item.vis_scope.is_none()
                && match &item.vis_parent {
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
struct Collector<'a> {
    an: &'a Analyzer,
    module: &'a str,
    self_ty: Option<&'a str>,
    normalize: bool,
    refs: BTreeMap<String, Ref>,
    targets: BTreeSet<String>,
    inferred_calls: BTreeSet<String>,
    /// Local variables with a known local type (from function parameters).
    locals: HashMap<String, String>,
}

impl<'a> Collector<'a> {
    fn new(an: &'a Analyzer, module: &'a str, self_ty: Option<&'a str>, normalize: bool) -> Self {
        Collector {
            an,
            module,
            self_ty,
            normalize,
            refs: BTreeMap::new(),
            targets: BTreeSet::new(),
            inferred_calls: BTreeSet::new(),
            locals: HashMap::new(),
        }
    }

    /// Records parameter types so that `param.method()` can be resolved.
    fn add_params(&mut self, sig: &syn::Signature) {
        for input in &sig.inputs {
            let syn::FnArg::Typed(pt) = input else {
                continue;
            };
            let syn::Pat::Ident(pi) = &*pt.pat else {
                continue;
            };
            let mut ty = &*pt.ty;
            while let syn::Type::Reference(r) = ty {
                ty = &r.elem;
            }
            if let syn::Type::Path(tp) = ty
                && tp.qself.is_none()
                && let Some(Res { id: Some(id), .. }) =
                    self.an
                        .resolve(self.module, &path_segments(&tp.path), self.self_ty)
            {
                self.locals.insert(pi.ident.to_string(), id);
            }
        }
    }

    fn handle_path(&mut self, path: &mut syn::Path) {
        let segs = path_segments(path);
        if segs.len() == 1 && (segs[0] == "self" || segs[0] == "crate" || segs[0] == "super") {
            return;
        }
        let Some(res) = self.an.resolve(self.module, &segs, self.self_ty) else {
            return;
        };
        let start = res.display_start.min(segs.len() - 1);
        let r = Ref {
            id: res.id.clone(),
            path: res.path.clone(),
            inferred: false,
        };
        let short = segs[start..].join("::");
        let key = match self.refs.get(&short) {
            Some(existing) if existing.path != r.path => {
                // Name clash within one item: keep the path as written.
                segs.join("::")
            }
            _ => {
                if self.normalize && start > 0 {
                    path.leading_colon = None;
                    path.segments = path.segments.iter().skip(start).cloned().collect();
                }
                short
            }
        };
        self.refs.insert(key, r);
        if let Some(id) = res.id {
            self.targets.insert(id);
        }
    }

    fn visit_macro_tokens(&mut self, tokens: &TokenStream) {
        use syn::parse::Parser;
        let parser = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated;
        if let Ok(exprs) = parser.parse2(tokens.clone()) {
            for mut expr in exprs {
                let normalize = std::mem::replace(&mut self.normalize, false);
                self.visit_expr_mut(&mut expr);
                self.normalize = normalize;
            }
        } else if let Ok(block) = syn::parse2::<syn::Block>(quote::quote!({ #tokens })) {
            let mut block = block;
            let normalize = std::mem::replace(&mut self.normalize, false);
            self.visit_block_mut(&mut block);
            self.normalize = normalize;
        }
    }
}

impl VisitMut for Collector<'_> {
    fn visit_attribute_mut(&mut self, _: &mut Attribute) {}

    fn visit_visibility_mut(&mut self, _: &mut Visibility) {}

    fn visit_path_mut(&mut self, path: &mut syn::Path) {
        visit_mut::visit_path_mut(self, path);
        self.handle_path(path);
    }

    fn visit_type_path_mut(&mut self, tp: &mut syn::TypePath) {
        if let Some(q) = &mut tp.qself {
            self.visit_type_mut(&mut q.ty);
            return;
        }
        visit_mut::visit_type_path_mut(self, tp);
    }

    fn visit_expr_path_mut(&mut self, ep: &mut syn::ExprPath) {
        if let Some(q) = &mut ep.qself {
            self.visit_type_mut(&mut q.ty);
            return;
        }
        visit_mut::visit_expr_path_mut(self, ep);
    }

    fn visit_macro_mut(&mut self, mac: &mut syn::Macro) {
        let tokens = mac.tokens.clone();
        self.visit_macro_tokens(&tokens);
    }

    fn visit_expr_method_call_mut(&mut self, call: &mut syn::ExprMethodCall) {
        visit_mut::visit_expr_method_call_mut(self, call);
        let receiver = match &*call.receiver {
            Expr::Path(p) if p.qself.is_none() => p.path.get_ident().map(|i| i.to_string()),
            _ => None,
        };
        let name = call.method.to_string();
        let receiver_ty = match receiver.as_deref() {
            Some("self") => self.self_ty.map(str::to_string),
            Some(local) => self.locals.get(local).cloned(),
            None => None,
        };
        if let Some(r) = self
            .an
            .resolve_method(&name, receiver_ty.as_deref(), receiver.is_some())
        {
            let id = r.id.clone().unwrap();
            if r.inferred {
                self.inferred_calls.insert(id);
            } else {
                self.targets.insert(id);
            }
            let key = format!(".{name}");
            match self.refs.get(&key) {
                Some(existing) if existing.path != r.path => {}
                _ => {
                    self.refs.insert(key, r);
                }
            }
        }
    }
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
