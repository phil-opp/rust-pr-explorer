//! Resolving the paths inside an item.

use super::{
    Analyzer, CallSite, LineCol, Ns, Res,
    syntax::{flatten_use, path_segments},
};
use model::Ref;
use proc_macro2::TokenStream;
use std::collections::{BTreeMap, BTreeSet};
use syn::visit_mut::{self, VisitMut};
use syn::{Attribute, Expr, Item, Visibility};

/// Walks a syntax tree, resolving every path. With `normalize`, paths are
/// shortened to the name a reader needs (`crate::a::Foo` → `Foo`).
///
/// Only certain resolutions are recorded: names that could refer to a
/// local variable, a generic parameter or an item declared inside the
/// body are left alone, and a name that resolves differently in two places
/// of one item is not linked at all.
pub(super) struct Collector<'a> {
    an: &'a Analyzer,
    module: &'a str,
    self_ty: Option<&'a str>,
    normalize: bool,
    pub(super) refs: BTreeMap<String, Ref>,
    /// Display keys that resolved to different targets.
    ambiguous: BTreeSet<String>,
    /// Items referred to (outside macro invocations).
    pub(super) targets: BTreeSet<String>,
    /// Names bound locally or as generic parameters.
    shadowed: std::collections::HashSet<String>,
    /// `use` declarations inside the body, see [`Bindings::imports`].
    imports: Vec<(Option<String>, Vec<String>)>,
    pub(super) sites: Vec<CallSite>,
    in_macro: bool,
    /// Resolved paths by the source position of their first segment.
    pub(super) pos_refs: BTreeMap<String, Ref>,
    /// Paths outside the analyzed crates, by the position of their last
    /// segment.
    pub(super) ext_sites: BTreeMap<String, LineCol>,
}

impl<'a> Collector<'a> {
    pub(super) fn new(
        an: &'a Analyzer,
        module: &'a str,
        self_ty: Option<&'a str>,
        normalize: bool,
    ) -> Self {
        Collector {
            an,
            module,
            self_ty,
            normalize,
            refs: BTreeMap::new(),
            ambiguous: BTreeSet::new(),
            targets: BTreeSet::new(),
            shadowed: Default::default(),
            imports: Vec::new(),
            sites: Vec::new(),
            in_macro: false,
            pos_refs: BTreeMap::new(),
            ext_sites: BTreeMap::new(),
        }
    }

    pub(super) fn shadow(&mut self, names: impl IntoIterator<Item = String>) {
        self.shadowed.extend(names);
    }

    /// Adds the bindings and imports of the body.
    pub(super) fn bind(&mut self, bindings: Bindings) {
        self.shadow(bindings.names);
        self.imports.extend(bindings.imports);
    }

    /// Whether a path names a local binding. Only a single name can: in
    /// `files::f`, `files` is a module even if a local `files` exists.
    fn is_local(&self, segs: &[String]) -> bool {
        segs.len() == 1 && self.shadowed.contains(&segs[0])
    }

    /// Resolves a path unless it names a local binding.
    fn resolve(&self, segs: &[String], ns: Option<Ns>) -> Option<Res> {
        if segs.is_empty() || self.is_local(segs) {
            return None;
        }
        if segs.len() == 1 && matches!(segs[0].as_str(), "self" | "crate" | "super") {
            return None;
        }
        self.an
            .resolve_with_imports(self.module, &self.imports, segs, self.self_ty, ns)
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
        if let Some(first) = path.segments.first() {
            let pos = first.ident.span().start();
            if pos.line > 0 {
                self.pos_refs
                    .insert(format!("{}:{}", pos.line, pos.column), r.clone());
            }
        }
        if r.id.is_none()
            && !self.in_macro
            && let Some(last) = path.segments.last()
        {
            let pos = last.ident.span().start();
            if pos.line > 0 {
                self.ext_sites.entry(r.path.clone()).or_insert(pos.into());
            }
        }
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
                (!self.is_local(&segs)).then(|| {
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
/// items declared inside a body, and the body's `use` declarations.
#[derive(Default)]
pub(super) struct Bindings {
    pub(super) names: Vec<String>,
    /// Flattened `use` entries: the name each brings into scope (`None`
    /// for a glob) and its path.
    pub(super) imports: Vec<(Option<String>, Vec<String>)>,
}

impl<'ast> syn::visit::Visit<'ast> for Bindings {
    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        self.names.push(p.ident.to_string());
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
        self.names.extend(ident.map(|i| i.to_string()));
        // An import brings a name into scope instead of hiding it.
        if let Item::Use(u) = item {
            flatten_use(&u.tree, Vec::new(), &mut self.imports);
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
