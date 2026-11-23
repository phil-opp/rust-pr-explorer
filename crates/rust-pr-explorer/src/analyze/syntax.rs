//! Rendering and normalizing syntax: signatures, docs, attributes.

use super::{Node, collector::Bindings};
use model::{Kind, Ref};
use quote::ToTokens;
use std::collections::BTreeMap;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, Item, Visibility, parse_quote};

/// Char offsets where resolved paths start in a rendered signature. The
/// signature is parsed again so that only real paths count, not names
/// being defined (fields, variants, parameters, the item itself).
pub(super) fn sig_link_offsets(kind: Kind, sig: &str, refs: &BTreeMap<String, Ref>) -> Vec<u32> {
    use syn::visit::Visit;
    struct Paths<'s> {
        text: &'s str,
        refs: &'s BTreeMap<String, Ref>,
        out: Vec<u32>,
    }
    impl Paths<'_> {
        fn path(&mut self, path: &syn::Path) {
            let Some(first) = path.segments.first() else {
                return;
            };
            let start = first.ident.span().byte_range().start;
            // The display key: segments without generic arguments.
            let key = path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            let known = self.refs.contains_key(&key)
                || (1..path.segments.len()).any(|n| {
                    let prefix: Vec<String> = path
                        .segments
                        .iter()
                        .take(n)
                        .map(|s| s.ident.to_string())
                        .collect();
                    self.refs.contains_key(&prefix.join("::"))
                });
            if known && start <= self.text.len() {
                self.out.push(self.text[..start].chars().count() as u32);
            }
        }
    }
    impl<'ast> Visit<'ast> for Paths<'_> {
        fn visit_attribute(&mut self, _: &'ast Attribute) {}
        fn visit_visibility(&mut self, _: &'ast Visibility) {}
        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.path(path);
            syn::visit::visit_path(self, path);
        }
    }
    let mut paths = Paths {
        text: sig,
        refs,
        out: Vec::new(),
    };
    // Bodies are replaced by text of the same length, so offsets stay valid.
    let parsed: Option<syn::File> = match kind {
        Kind::Fn | Kind::Method | Kind::TraitMethod | Kind::Impl => {
            let text = sig.trim_end();
            let text = text
                .strip_suffix("{ ... }")
                .map(|t| format!("{t}{{     }}"))
                .or_else(|| text.strip_suffix(';').map(|t| format!("{t} {{}}")))
                .unwrap_or_else(|| format!("{text} {{}}"));
            syn::parse_str(&text).ok()
        }
        Kind::Trait => syn::parse_str(&sig.replace("{ ... }", "{     }")).ok(),
        Kind::Mod | Kind::Macro => None,
        _ => syn::parse_str(sig).ok(),
    };
    if let Some(file) = parsed {
        paths.visit_file(&file);
    }
    paths.out.sort_unstable();
    paths.out.dedup();
    paths.out
}

/// Generic parameter names an item declares itself.
pub(super) fn node_generics(node: &Node) -> Vec<String> {
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
pub(super) fn node_bindings(node: &Node) -> Vec<String> {
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
pub(super) fn generic_names(g: &syn::Generics) -> Vec<String> {
    g.params
        .iter()
        .filter_map(|p| match p {
            syn::GenericParam::Type(t) => Some(t.ident.to_string()),
            syn::GenericParam::Const(c) => Some(c.ident.to_string()),
            syn::GenericParam::Lifetime(_) => None,
        })
        .collect()
}

pub(super) fn path_segments(path: &syn::Path) -> Vec<String> {
    path.segments.iter().map(|s| s.ident.to_string()).collect()
}

pub(super) fn flatten_use(
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

pub(super) fn item_attrs(item: &Item) -> &[Attribute] {
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

/// Whether a module belongs to an integration test or bench target, where
/// tests are the subject rather than test-only code.
pub(super) fn in_test_target(module: &str) -> bool {
    let target = module.split("::").next().unwrap_or("");
    target.starts_with("test:") || target.starts_with("bench:")
}

/// Whether a crate id is a target other than a library (`bin:…`,
/// `test:…`, …). Those are not part of any public API.
pub fn is_extra_target(id: &str) -> bool {
    id.split("::").next().unwrap_or("").contains(':')
}

/// Marked `#[test]` or `#[cfg(test)]`.
pub(super) fn is_test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("test")
            || (a.path().is_ident("cfg") && a.meta.to_token_stream().to_string() == "cfg (test)")
    })
}

pub(super) fn path_attr(attrs: &[Attribute]) -> Option<String> {
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

pub fn extract_docs(attrs: &[Attribute]) -> String {
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
pub(super) fn intra_doc_links(docs: &str) -> Vec<String> {
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

pub(super) fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// Whether the attributes include `#[doc(hidden)]`.
pub(super) fn is_doc_hidden(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("doc")
            && a.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|list| list.iter().any(|m| m.path().is_ident("hidden")))
    })
}

/// Attributes rustdoc shows in a definition.
pub(super) fn shown_attrs(attrs: &[Attribute]) -> Vec<Attribute> {
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

pub(super) fn strip_field_docs(fields: &mut syn::Fields) {
    for f in fields.iter_mut() {
        f.attrs.retain(|a| !a.path().is_ident("doc"));
    }
}

/// Removes private fields from a definition. Returns whether anything was hidden.
pub(super) fn hide_private_fields(fields: &mut syn::Fields) -> bool {
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

pub(super) fn fix_private_marker(s: &str) -> String {
    s.replace("__private_fields__: (),", "/* private fields */")
        .trim()
        .to_string()
}

pub(super) fn shorten_expr(expr: &mut Expr) {
    let text = expr.to_token_stream().to_string();
    if text.len() > 40 || text.contains('{') {
        *expr = parse_quote!(_);
    }
}

pub(super) fn vis_prefix(vis: &Visibility) -> String {
    match vis {
        Visibility::Inherited => String::new(),
        v => format!("{} ", v.to_token_stream().to_string().replace(' ', "")),
    }
}

pub(super) fn unparse(item: Item) -> String {
    let file: syn::File = parse_quote!(#item);
    prettyplease::unparse(&file)
}

pub(super) fn unparse_type(ty: &syn::Type) -> String {
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

/// Maximum line width of rendered function signatures.
const SIG_WIDTH: usize = 64;

/// Lays out a function signature the way rustfmt does: on one line if it
/// fits, otherwise one parameter per line, and a `where` clause with one
/// predicate per line.
pub fn wrap_fn_sig(sig: &str) -> String {
    let Ok(f) = syn::parse_str::<syn::ItemFn>(&format!("{sig} {{}}")) else {
        return sig.to_string();
    };
    let source = format!("{sig} {{}}");
    let text = |node: &dyn Spanned| -> String {
        let r = node.span().byte_range();
        source[r].split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let paren = f.sig.paren_token.span;
    let head = &source[..paren.open().byte_range().end];
    let params: Vec<String> = f.sig.inputs.iter().map(|a| text(a)).collect();
    let ret = match &f.sig.output {
        syn::ReturnType::Default => String::new(),
        syn::ReturnType::Type(_, ty) => format!(" -> {}", text(&**ty)),
    };
    let preds: Vec<String> = f
        .sig
        .generics
        .where_clause
        .iter()
        .flat_map(|w| w.predicates.iter().map(|p| text(p)))
        .collect();
    let where_block = if preds.is_empty() {
        String::new()
    } else {
        format!(
            "\nwhere\n{}",
            preds
                .iter()
                .map(|p| format!("    {p},"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let one_line = format!("{head}{}){ret}", params.join(", "));
    let last_line = one_line.rsplit('\n').next().unwrap_or("");
    if last_line.chars().count() <= SIG_WIDTH || params.is_empty() {
        return one_line + &where_block;
    }
    let params: String = params.iter().map(|p| format!("    {p},\n")).collect();
    format!("{head}\n{params}){ret}{where_block}")
}

/// Lays out the long one-line method signatures of a trait definition
/// like [`wrap_fn_sig`], indented by the trait's four spaces.
pub(super) fn wrap_trait_methods(text: &str) -> String {
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        let inner = body.strip_prefix("    ").unwrap_or(body);
        let (sig, end) = if let Some(s) = inner.strip_suffix(" { ... }") {
            (s, " { ... }")
        } else if let Some(s) = inner.strip_suffix(';') {
            (s, ";")
        } else {
            ("", "")
        };
        let is_fn = sig.split_whitespace().any(|w| w == "fn");
        if is_fn && body.starts_with("    ") && body.chars().count() > SIG_WIDTH + 4 {
            let wrapped = wrap_fn_sig(sig);
            for (i, l) in wrapped.lines().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                out.push_str("    ");
                out.push_str(l);
            }
            out.push_str(end);
            if line.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Turns `fn foo() {}` into `fn foo()`.
pub(super) fn strip_body(s: &str) -> String {
    let s = s.trim_end();
    s.strip_suffix("{}").unwrap_or(s).trim_end().to_string()
}

/// Canonical text of a block, independent of the original formatting.
pub(super) fn norm_block(block: &syn::Block) -> String {
    unparse(parse_quote!(fn __f() #block))
}

pub(super) fn norm_expr(expr: &Expr) -> String {
    unparse(parse_quote!(const _: () = #expr;))
}
