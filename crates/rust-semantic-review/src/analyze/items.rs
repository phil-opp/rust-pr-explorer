//! Pass 3: signatures, docs, references and edges of each item.

use super::{
    Analyzer, ItemData, Node, Pending, Rendered,
    collector::Collector,
    syntax::{
        extract_docs, fix_private_marker, hide_private_fields, intra_doc_links, is_doc_hidden,
        is_ident, is_test_fn, node_bindings, node_generics, norm_block, norm_expr, shorten_expr,
        shown_attrs, sig_link_offsets, strip_body, strip_field_docs, unparse, unparse_type,
        vis_prefix, wrap_fn_sig, wrap_trait_methods,
    },
};
use model::{Call, ItemInfo, Kind, Ref, Use, UseCtx};
use std::collections::{BTreeMap, BTreeSet};
use syn::spanned::Spanned;
use syn::visit_mut::VisitMut;
use syn::{Attribute, Item, Visibility, parse_quote};

impl Analyzer {
    /// Pass 3: signatures, docs, references and edges.
    pub(super) fn build_items(&mut self) -> BTreeMap<String, ItemData> {
        self.lookup_memo.borrow_mut().clear();
        let pending = std::mem::take(&mut self.pending);
        let reexports = self.reexports();
        let mut items = BTreeMap::new();
        for p in pending {
            if items.contains_key(&p.id) {
                continue;
            }
            if let Some(item) = self.build_item(&p) {
                items.insert(p.id.clone(), item);
            }
        }
        self.mark_public_api(&mut items, &reexports);
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
                                wrap_fn_sig(&strip_body(&unparse(Item::Fn(f)))),
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
                            let text = wrap_trait_methods(&text);
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
                    let text = wrap_fn_sig(&strip_body(&unparse(
                        parse_quote!(#(#shown)* #vis #sig_ {}),
                    )));
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
                    let mut text = wrap_fn_sig(&strip_body(&unparse(parse_quote!(#sig_ {}))));
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
                    let text = unparse(parse_quote!(trait __X { #t2 }));
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

        let sig_links = sig_link_offsets(kind, &sig, &sig_col.refs);
        let sig_public_links = sig_public
            .as_deref()
            .map(|s| sig_link_offsets(kind, s, &sig_col.refs))
            .unwrap_or_default();
        let mut ext_sites = body_col.ext_sites.clone();
        ext_sites.extend(sig_col.ext_sites.clone());
        let mut source_refs = sig_col.pos_refs.clone();
        source_refs.extend(body_col.pos_refs.clone());
        // The text, plus every name's full target in order: names that
        // print the same (`io::Error` and `fmt::Error` both show as
        // `Error`) still differ here.
        let sig_norm = format!("{sig}|{:?}", ordered_targets(&sig_col.pos_refs));
        let body_norm = format!("{body_norm}|{:?}", ordered_targets(&body_col.pos_refs));
        let mut children = self.children.get(&p.id).cloned().unwrap_or_default();
        children.dedup();

        let ident = match &p.node {
            Node::Item(Item::Fn(f)) => Some(&f.sig.ident),
            Node::Item(Item::Struct(s)) => Some(&s.ident),
            Node::Item(Item::Enum(e)) => Some(&e.ident),
            Node::Item(Item::Union(u)) => Some(&u.ident),
            Node::Item(Item::Trait(t)) => Some(&t.ident),
            Node::Item(Item::Type(t)) => Some(&t.ident),
            Node::Item(Item::Const(c)) => Some(&c.ident),
            Node::Item(Item::Static(s)) => Some(&s.ident),
            Node::Item(Item::Macro(m)) => m.ident.as_ref(),
            Node::ImplFn(f) => Some(&f.sig.ident),
            Node::TraitFn(f) => Some(&f.sig.ident),
            _ => None,
        };
        let name_pos = ident.map(|i| i.span().start().into());
        let doc_hidden = is_doc_hidden(&attrs);
        let test_only = self.test_ids.contains(&p.id)
            || self.is_test_code(&p.module, &attrs)
            || p.impl_ctx.as_ref().is_some_and(|c| c.test_only)
            || p.parent.as_ref().is_some_and(|q| self.test_ids.contains(q));
        let test = kind == Kind::Fn && is_test_fn(&attrs);
        let info = ItemInfo {
            id: p.id.clone(),
            kind,
            public_paths: Vec::new(),
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
            source_refs,
            sig_links,
            sig_public_links,
            reexport_of: None,
            test_only,
            test,
        };
        Some(ItemData {
            info,
            sig_norm,
            body_norm,
            source,
            name_pos,
            doc_hidden,
            call_sites: body_col.sites,
            ext_sites,
            unsafe_sites: own_unsafe(&p.node),
        })
    }
}

/// The `unsafe` code of an item itself: for an impl or a trait only its
/// header, since its members are items of their own; none for a module.
fn own_unsafe(node: &Node) -> Vec<crate::unsafety::Site> {
    use quote::ToTokens;
    let tokens = match node {
        Node::Mod { .. } => return Vec::new(),
        Node::Item(Item::Impl(i)) => {
            let mut i = i.clone();
            i.items.clear();
            i.into_token_stream()
        }
        Node::Item(Item::Trait(t)) => {
            let mut t = t.clone();
            t.items.clear();
            t.into_token_stream()
        }
        Node::Item(item) => item.to_token_stream(),
        Node::ImplFn(f) => f.to_token_stream(),
        Node::ImplConst(c) => c.to_token_stream(),
        Node::ImplType(t) => t.to_token_stream(),
        Node::TraitFn(f) => f.to_token_stream(),
        Node::TraitConst(c) => c.to_token_stream(),
        Node::TraitType(t) => t.to_token_stream(),
    };
    crate::unsafety::sites(tokens)
}

/// The targets of resolved paths, in source order.
fn ordered_targets(pos_refs: &BTreeMap<String, Ref>) -> Vec<&str> {
    let mut v: Vec<((usize, usize), &str)> = pos_refs
        .iter()
        .filter_map(|(k, r)| {
            let (l, c) = k.split_once(':')?;
            Some(((l.parse().ok()?, c.parse().ok()?), r.path.as_str()))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, p)| p).collect()
}
