//! Pass 1 and 2: loading module files, collecting items and impls.

use super::{
    Analyzer, ImplCtx, ModData, Node, Ns, Pending, RawImpl, Res, SrcFile, UseEntry,
    collector::Collector,
    syntax::{
        flatten_use, generic_names, item_attrs, path_attr, path_segments, skipped, strip_body,
        unparse, unparse_type,
    },
};
use model::{Kind, Ref};
use quote::ToTokens;
use std::path::Path;
use std::rc::Rc;
use syn::visit_mut::VisitMut;
use syn::{Attribute, Item, Visibility, parse_quote};

/// Modules a crate may read from files it already loaded as other
/// modules (one file included twice with `#[path]`, which rustc allows).
/// Without a limit, files that include the next file twice each double the
/// number of modules, and a PR could make the analysis run for hours.
const MAX_REPEATED_LOADS: usize = 1000;

impl Analyzer {
    /// Reads and parses a source file for module `id`. Files outside the
    /// checkout are refused, and so is a file that an enclosing module of
    /// `id` was loaded from (a `#[path]` cycle). A file may be included as
    /// several unrelated modules, like rustc allows, up to
    /// [`MAX_REPEATED_LOADS`] times per crate. The error's flag is
    /// set when the file is the same module, declared again (under another
    /// `cfg`).
    pub(super) fn load_file(
        &mut self,
        path: &Path,
        id: &str,
    ) -> Result<(syn::File, Rc<SrcFile>), (String, bool)> {
        let canonical = std::fs::canonicalize(path)
            .map_err(|e| (format!("cannot read {}: {e}", path.display()), false))?;
        if !canonical.starts_with(&self.canonical_root) {
            return Err((
                format!("{} is outside the checkout; not read", path.display()),
                false,
            ));
        }
        let krate = Self::crate_of(id).to_string();
        let ids = self
            .loaded
            .get(&(krate.clone(), canonical.clone()))
            .map(Vec::as_slice)
            .unwrap_or_default();
        if ids.iter().any(|first| first == id) {
            return Err((
                format!(
                    "module `{id}` is declared more than once (under different `cfg`s); the first declaration is used"
                ),
                true,
            ));
        }
        if let Some(outer) = ids
            .iter()
            .find(|first| id.starts_with(&format!("{first}::")))
        {
            return Err((
                format!(
                    "{} is included inside itself (module `{outer}` through a `#[path]`); not read again",
                    path.display()
                ),
                false,
            ));
        }
        if !ids.is_empty() {
            let n = self.repeated_loads.entry(krate.clone()).or_default();
            *n += 1;
            if *n > MAX_REPEATED_LOADS {
                // Warned once, when the limit is reached; the callers skip
                // empty warnings.
                let warning = if *n == MAX_REPEATED_LOADS + 1 {
                    format!(
                        "crate `{krate}` includes files as more than {MAX_REPEATED_LOADS} further modules (`#[path]`); the rest are not read"
                    )
                } else {
                    String::new()
                };
                return Err((warning, false));
            }
        }
        self.loaded
            .entry((krate, canonical))
            .or_default()
            .push(id.to_string());
        let text = std::fs::read_to_string(path)
            .map_err(|e| (format!("cannot read {}: {e}", path.display()), false))?;
        let file = syn::parse_file(&text)
            .map_err(|e| (format!("cannot parse {}: {e}", path.display()), false))?;
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

    /// An id not used yet: `base`, or for a second definition at the same
    /// path (`#[cfg(unix)]` and `#[cfg(windows)]` variants) `base` with its
    /// `cfg` (and a number if that is taken too).
    fn unique_id(&self, base: &str, attrs: &[Attribute], kind: Kind) -> String {
        let Some(existing) = self.kinds.get(base) else {
            return base.to_string();
        };
        // Different name spaces (a module and a function `helper`): both
        // are legal, the later one is tagged with its kind.
        let space = |k: Kind| match k {
            Kind::Fn | Kind::Method | Kind::TraitMethod | Kind::Const | Kind::Static => 1,
            Kind::AssocConst => 1,
            Kind::Macro => 2,
            Kind::Impl => 3,
            _ => 0,
        };
        if space(*existing) != space(kind) {
            let tagged = format!("{base} [{}]", kind.label());
            if !self.kinds.contains_key(&tagged) {
                return tagged;
            }
        }
        let cfg: Option<String> = attrs
            .iter()
            .find(|a| a.path().is_ident("cfg"))
            .map(|a| a.meta.to_token_stream().to_string().replace(' ', ""));
        let tagged = match &cfg {
            Some(c) => format!("{base} [{c}]"),
            None => format!("{base} #2"),
        };
        let mut id = tagged.clone();
        let mut n = 2;
        while self.kinds.contains_key(&id) {
            n += 1;
            id = format!("{tagged} #{n}");
        }
        id
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
    pub(super) fn collect_items(
        &mut self,
        module: &str,
        items: &[Item],
        src: &Rc<SrcFile>,
        dir: &Path,
    ) {
        for item in items {
            if skipped(module, item_attrs(item)) {
                continue;
            }
            let push = |an: &mut Analyzer, name: String, kind: Kind| -> String {
                let id = an.unique_id(&format!("{module}::{name}"), item_attrs(item), kind);
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
                    id: id.clone(),
                    name,
                    module: module.to_string(),
                    file: src.clone(),
                    node: Node::Item(item.clone()),
                    parent: None,
                    self_ty: None,
                    impl_ctx: None,
                    outer_generics: Vec::new(),
                });
                id
            };
            match item {
                Item::Fn(f) => {
                    push(self, f.sig.ident.to_string(), Kind::Fn);
                }
                Item::Struct(s) => {
                    let id = push(self, s.ident.to_string(), Kind::Struct);
                    if !matches!(s.fields, syn::Fields::Named(_)) {
                        self.value_structs.insert(id);
                    }
                }
                Item::Union(u) => {
                    push(self, u.ident.to_string(), Kind::Union);
                }
                Item::Enum(e) => {
                    let id = push(self, e.ident.to_string(), Kind::Enum);
                    self.enum_variants
                        .insert(id, e.variants.iter().map(|v| v.ident.to_string()).collect());
                }
                Item::Const(c) => {
                    push(self, c.ident.to_string(), Kind::Const);
                }
                Item::Static(s) => {
                    push(self, s.ident.to_string(), Kind::Static);
                }
                Item::Type(t) => {
                    push(self, t.ident.to_string(), Kind::TypeAlias);
                }
                Item::Macro(m) => {
                    if let Some(ident) = &m.ident {
                        push(self, ident.to_string(), Kind::Macro);
                    }
                }
                Item::Trait(t) => {
                    let trait_id = push(self, t.ident.to_string(), Kind::Trait);
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
                            match self.load_file(file_path, &id) {
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
                                Err((e, _)) => {
                                    if !e.is_empty() {
                                        self.warnings.push(e);
                                    }
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
    pub(super) fn collect_impls(&mut self) {
        // Lookups made while modules were still being collected may have
        // missed later definitions.
        self.lookup_memo.borrow_mut().clear();
        let impls = std::mem::take(&mut self.impls);
        for raw in impls {
            let item = &raw.item;
            if skipped(&raw.module, &item.attrs) {
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
                    // The self type is spelled out when it is more than a
                    // name (`W<u8>`, `&T`), so that impls for `W<u8>` and
                    // `W<u16>` stay apart.
                    let bare = self_id
                        .as_deref()
                        .and_then(|i| i.rsplit("::").next())
                        .is_some_and(|n| n == self_display);
                    let impl_id = if bare {
                        format!("{base}::<impl {trait_display}>")
                    } else {
                        format!("{base}::<impl {trait_display} for {self_display}>")
                    };
                    let impl_id = self.unique_id(&impl_id, &item.attrs, Kind::Impl);
                    let name = format!("impl {trait_display} for {self_display}");
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
                        if skipped(&raw.module, &f.attrs) {
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
                let attrs = match member {
                    syn::ImplItem::Fn(f) => &f.attrs,
                    syn::ImplItem::Const(c) => &c.attrs,
                    syn::ImplItem::Type(t) => &t.attrs,
                    _ => continue,
                };
                let id = self.unique_id(&format!("{impl_id}::{name}"), attrs, kind);
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
}
