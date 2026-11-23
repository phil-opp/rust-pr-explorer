//! Whether a public API change is breaking, in the spirit of Cargo's
//! SemVer rules (<https://doc.rust-lang.org/cargo/reference/semver.html>)
//! and `cargo-semver-checks`.
//!
//! Only what the definitions show is used. Where that isn't enough to
//! tell (a changed macro, a changed `repr`, a parse failure), the change is
//! labeled "check by hand" instead of guessed.

use crate::analyze::ItemData;
use model::{ApiChange, ApiLevel, Kind, SigDiff};
use quote::ToTokens;
use std::collections::HashSet;

/// Collects the reasons of one item's API change.
#[derive(Default)]
struct Verdict {
    breaking: Vec<String>,
    compatible: Vec<String>,
    unknown: Vec<String>,
}

impl Verdict {
    fn breaking(&mut self, s: impl Into<String>) {
        self.breaking.push(s.into());
    }
    fn compatible(&mut self, s: impl Into<String>) {
        self.compatible.push(s.into());
    }
    fn unknown(&mut self, s: impl Into<String>) {
        self.unknown.push(s.into());
    }

    fn finish(self) -> Option<ApiChange> {
        let (level, reasons) = if !self.breaking.is_empty() {
            (ApiLevel::Breaking, self.breaking)
        } else if !self.unknown.is_empty() {
            (ApiLevel::Unknown, self.unknown)
        } else if !self.compatible.is_empty() {
            (ApiLevel::Compatible, self.compatible)
        } else {
            return None;
        };
        Some(ApiChange {
            level,
            reasons,
            confirmed_by: Vec::new(),
        })
    }
}

/// What the classification needs besides the two versions of the item.
pub struct Context<'a> {
    /// Ids of the items that were public in the base.
    pub base_public: &'a HashSet<String>,
}

/// The API change of one item between base `b` and head `h`, if it is
/// public on either side and its public API changed.
pub fn classify(
    ctx: &Context,
    b: Option<&ItemData>,
    h: Option<&ItemData>,
    exports_added: &[String],
    exports_removed: &[String],
    sig_diff: Option<&SigDiff>,
) -> Option<ApiChange> {
    // Members of trait impls are the trait's API, labeled with the impl.
    let in_trait_impl = |d: &ItemData| d.info.kind == Kind::Method && d.info.trait_ref.is_some();
    if b.or(h).is_some_and(in_trait_impl) {
        return None;
    }
    let was = b.is_some_and(|b| b.info.public);
    let is = h.is_some_and(|h| h.info.public);
    let mut v = Verdict::default();
    match (b, h) {
        _ if !was && !is => return None,
        (Some(_), None) => v.breaking(removed_reason(b.unwrap())),
        (Some(_), Some(h)) if !is => {
            if h.doc_hidden {
                v.breaking("now `#[doc(hidden)]`, so no longer part of the public API");
            } else {
                v.breaking("no longer public");
            }
        }
        (_, Some(h)) if !was => added(ctx, h, b.is_some(), &mut v),
        (Some(b), Some(h)) => {
            for p in exports_removed {
                v.breaking(format!("no longer public as `{p}`"));
            }
            for p in exports_added {
                v.compatible(format!("now also public as `{p}`"));
            }
            if b.sig_norm != h.sig_norm {
                signature(b, h, sig_diff, &mut v);
            }
        }
        (None, Some(h)) => added(ctx, h, false, &mut v),
        (None, None) => return None,
    }
    v.finish()
}

fn removed_reason(b: &ItemData) -> String {
    match b.info.kind {
        Kind::Impl => "trait no longer implemented".into(),
        Kind::Mod => "module removed".into(),
        _ if b.info.reexport_of.is_some() => "re-export removed".into(),
        _ => "removed".into(),
    }
}

/// An item that is public now and wasn't before (`existed`: it was there,
/// but not public).
fn added(ctx: &Context, h: &ItemData, existed: bool, v: &mut Verdict) {
    let i = &h.info;
    // A new required item of a trait that already was public breaks every
    // implementation outside the crate.
    let in_old_trait = matches!(
        i.kind,
        Kind::TraitMethod | Kind::AssocConst | Kind::AssocType
    ) && i
        .parent
        .as_ref()
        .is_some_and(|p| ctx.base_public.contains(p));
    if in_old_trait && !existed {
        match trait_item_default(&h.source, i.kind) {
            Some(true) => v.compatible("trait item added with a default"),
            Some(false) => v.breaking(
                "trait item added without a default: implementations outside the crate must add it (unless the trait is sealed)",
            ),
            None => v.unknown("trait item added; whether it has a default couldn't be read"),
        }
        return;
    }
    v.compatible(match (i.kind, existed) {
        (Kind::Impl, _) => "trait implemented",
        (_, true) => "now public",
        _ if i.reexport_of.is_some() => "re-export added",
        _ => "added",
    });
}

/// Whether a trait item's source has a default (a body, a value or a type).
fn trait_item_default(source: &str, kind: Kind) -> Option<bool> {
    Some(match kind {
        Kind::TraitMethod => syn::parse_str::<syn::TraitItemFn>(source)
            .ok()?
            .default
            .is_some(),
        Kind::AssocConst => syn::parse_str::<syn::TraitItemConst>(source)
            .ok()?
            .default
            .is_some(),
        Kind::AssocType => syn::parse_str::<syn::TraitItemType>(source)
            .ok()?
            .default
            .is_some(),
        _ => return None,
    })
}

/// A changed signature (or a name in it that now means another item).
fn signature(b: &ItemData, h: &ItemData, sig_diff: Option<&SigDiff>, v: &mut Verdict) {
    let (old, new) = (&b.info.sig, &h.info.sig);
    if old == new {
        v.breaking("a name in the signature now refers to a different item");
        return;
    }
    let notes = || -> Vec<String> {
        sig_diff
            .map(|d| d.notes.iter().map(|n| n.text.clone()).collect())
            .unwrap_or_default()
    };
    match h.info.kind {
        Kind::Fn | Kind::Method | Kind::TraitMethod => function(old, new, h.info.kind, notes(), v),
        Kind::Struct => match (
            syn::parse_str::<syn::ItemStruct>(old),
            syn::parse_str::<syn::ItemStruct>(new),
        ) {
            (Ok(o), Ok(n)) => structure(&o, &n, v),
            _ => v.unknown("definition changed; it couldn't be compared"),
        },
        Kind::Union => match (
            syn::parse_str::<syn::ItemUnion>(old),
            syn::parse_str::<syn::ItemUnion>(new),
        ) {
            (Ok(o), Ok(n)) => union(&o, &n, v),
            _ => v.unknown("definition changed; it couldn't be compared"),
        },
        Kind::Enum => match (
            syn::parse_str::<syn::ItemEnum>(old),
            syn::parse_str::<syn::ItemEnum>(new),
        ) {
            (Ok(o), Ok(n)) => enumeration(&o, &n, v),
            _ => v.unknown("definition changed; it couldn't be compared"),
        },
        Kind::Trait => match (
            syn::parse_str::<syn::ItemTrait>(old),
            syn::parse_str::<syn::ItemTrait>(new),
        ) {
            (Ok(o), Ok(n)) => trait_header(&o, &n, v),
            _ => v.unknown("definition changed; it couldn't be compared"),
        },
        Kind::Const | Kind::Static | Kind::AssocConst => {
            if declared_type(old) != declared_type(new) || mutability(old) != mutability(new) {
                v.breaking("type changed");
            } else {
                v.compatible("value changed");
            }
        }
        Kind::TypeAlias | Kind::AssocType => v.breaking("type changed"),
        Kind::Macro => v.unknown("macro definition changed"),
        Kind::Impl => v.unknown("impl header changed"),
        Kind::Mod | Kind::Variant => {}
    }
}

/// `pub const X: T = …;` → `T`
fn declared_type(sig: &str) -> String {
    let after = sig.split_once(':').map_or("", |(_, t)| t);
    let ty = after.split(['=', ';']).next().unwrap_or("");
    ty.split_whitespace().collect()
}

fn mutability(sig: &str) -> bool {
    sig.split_whitespace().any(|w| w == "mut")
}

fn function(old: &str, new: &str, kind: Kind, notes: Vec<String>, v: &mut Verdict) {
    let parse = |s: &str| -> Option<syn::ImplItemFn> {
        let s = s.trim_end().trim_end_matches(';');
        syn::parse_str::<syn::ImplItemFn>(&format!("{s} {{}}")).ok()
    };
    let (Some(o), Some(n)) = (parse(old), parse(new)) else {
        v.unknown("signature changed; it couldn't be compared");
        return;
    };
    let attrs = |f: &syn::ImplItemFn| -> Vec<String> {
        f.attrs.iter().map(|a| a.to_token_string_lossy()).collect()
    };
    if attrs(&o) != attrs(&n) {
        v.compatible("attributes changed");
    }
    let (mut os, mut ns) = (o.sig.clone(), n.sig.clone());
    let const_change = (os.constness.is_some(), ns.constness.is_some());
    let is_unsafe = |s: &syn::Signature| matches!(s.safety, syn::Safety::Unsafe(_));
    let unsafe_change = (is_unsafe(&os), is_unsafe(&ns));
    os.constness = None;
    ns.constness = None;
    os.safety = syn::Safety::Default;
    ns.safety = syn::Safety::Default;
    let rest_same = os.to_token_stream().to_string() == ns.to_token_stream().to_string()
        && o.vis.to_token_stream().to_string() == n.vis.to_token_stream().to_string();
    match const_change {
        (false, true) => v.compatible("now `const`"),
        (true, false) => v.breaking("no longer `const`: callers in constant contexts break"),
        _ => {}
    }
    match unsafe_change {
        (false, true) => v.breaking("now `unsafe`: callers must use an `unsafe` block"),
        // Implementations of an `unsafe` trait method may stay `unsafe`
        // only if the trait's stays so.
        (true, false) if kind == Kind::TraitMethod => {
            v.breaking("no longer `unsafe`: implementations that are `unsafe` break")
        }
        (true, false) => v.compatible("no longer `unsafe`"),
        _ => {}
    }
    if !rest_same {
        if notes.is_empty() {
            v.breaking("signature changed");
        } else {
            for n in notes {
                v.breaking(n);
            }
        }
    }
}

trait Lossy {
    fn to_token_string_lossy(&self) -> String;
}

impl<T: ToTokens> Lossy for T {
    fn to_token_string_lossy(&self) -> String {
        self.to_token_stream().to_string()
    }
}

fn has_attr(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

fn repr(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("repr"))
        .map(|a| a.to_token_string_lossy())
        .collect()
}

/// `#[non_exhaustive]` and `#[repr]`, shared by structs, unions and enums.
fn type_attrs(old: &[syn::Attribute], new: &[syn::Attribute], v: &mut Verdict) {
    match (
        has_attr(old, "non_exhaustive"),
        has_attr(new, "non_exhaustive"),
    ) {
        (false, true) => v.breaking(
            "now `#[non_exhaustive]`: outside the crate it can no longer be built with a literal or matched exhaustively",
        ),
        (true, false) => v.compatible("no longer `#[non_exhaustive]`"),
        _ => {}
    }
    if repr(old) != repr(new) {
        v.unknown("`#[repr]` changed");
    }
}

fn generics(old: &syn::Generics, new: &syn::Generics, v: &mut Verdict) {
    let s = |g: &syn::Generics| {
        (
            g.params
                .iter()
                .map(|p| p.to_token_string_lossy())
                .collect::<Vec<_>>(),
            g.where_clause.to_token_string_lossy(),
        )
    };
    let ((op, ow), (np, nw)) = (s(old), s(new));
    if op == np && ow == nw {
        return;
    }
    // Type parameters with defaults appended: existing uses still work.
    let appended_defaults = ow == nw
        && np.len() > op.len()
        && np[..op.len()] == op[..]
        && new.params.iter().skip(op.len()).all(|p| match p {
            syn::GenericParam::Type(t) => t.default.is_some(),
            _ => false,
        });
    if appended_defaults {
        v.compatible("type parameter with a default added");
    } else {
        v.breaking("generic parameters or bounds changed");
    }
}

struct Field {
    name: String,
    public: bool,
    ty: String,
}

fn fields(f: &syn::Fields) -> Vec<Field> {
    f.iter()
        .enumerate()
        .map(|(i, f)| Field {
            name: f
                .ident
                .as_ref()
                .map_or_else(|| i.to_string(), |i| i.to_string()),
            public: matches!(f.vis, syn::Visibility::Public(_)),
            ty: f.ty.to_token_string_lossy(),
        })
        .collect()
}

fn style(f: &syn::Fields) -> &'static str {
    match f {
        syn::Fields::Named(_) => "named fields",
        syn::Fields::Unnamed(_) => "a tuple struct",
        syn::Fields::Unit => "a unit struct",
    }
}

fn structure(o: &syn::ItemStruct, n: &syn::ItemStruct, v: &mut Verdict) {
    type_attrs(&o.attrs, &n.attrs, v);
    generics(&o.generics, &n.generics, v);
    if style(&o.fields) != style(&n.fields) {
        v.breaking(format!(
            "changed from {} to {}",
            style(&o.fields),
            style(&n.fields)
        ));
        return;
    }
    // Outside the crate, a struct whose fields are all public (and that
    // isn't `#[non_exhaustive]`) can be built with a literal and matched
    // without `..`: adding any field breaks that.
    let constructible =
        fields(&o.fields).iter().all(|f| f.public) && !has_attr(&o.attrs, "non_exhaustive");
    compare_fields(
        &fields(&o.fields),
        &fields(&n.fields),
        constructible,
        "field",
        v,
    );
}

fn union(o: &syn::ItemUnion, n: &syn::ItemUnion, v: &mut Verdict) {
    type_attrs(&o.attrs, &n.attrs, v);
    generics(&o.generics, &n.generics, v);
    let (of, nf) = (
        fields(&syn::Fields::Named(o.fields.clone())),
        fields(&syn::Fields::Named(n.fields.clone())),
    );
    let constructible = of.iter().all(|f| f.public);
    compare_fields(&of, &nf, constructible, "field", v);
}

fn compare_fields(old: &[Field], new: &[Field], constructible: bool, what: &str, v: &mut Verdict) {
    let mut private_changed = false;
    for f in old {
        let now = new.iter().find(|n| n.name == f.name);
        match (f.public, now) {
            (true, None) => v.breaking(format!("public {what} `{}` removed", f.name)),
            (true, Some(n)) if !n.public => {
                v.breaking(format!("{what} `{}` no longer public", f.name))
            }
            (true, Some(n)) if n.ty != f.ty => {
                v.breaking(format!("type of public {what} `{}` changed", f.name))
            }
            (false, None) => private_changed = true,
            (false, Some(n)) if n.ty != f.ty => private_changed = true,
            _ => {}
        }
    }
    for n in new {
        let before = old.iter().find(|o| o.name == n.name);
        let newly_public = n.public && !before.is_some_and(|o| o.public);
        if newly_public {
            if constructible {
                v.breaking(format!(
                    "public {what} `{}` added: struct literals and patterns without `..` outside the crate break",
                    n.name
                ));
            } else {
                v.compatible(format!("public {what} `{}` added", n.name));
            }
        } else if !n.public && before.is_none() {
            if constructible {
                v.breaking(format!(
                    "private {what} added: the type can no longer be built outside the crate"
                ));
            } else {
                private_changed = true;
            }
        }
    }
    if private_changed && v.breaking.is_empty() && v.compatible.is_empty() {
        v.compatible("only private fields changed");
    }
}

fn enumeration(o: &syn::ItemEnum, n: &syn::ItemEnum, v: &mut Verdict) {
    type_attrs(&o.attrs, &n.attrs, v);
    generics(&o.generics, &n.generics, v);
    let exhaustive = !has_attr(&o.attrs, "non_exhaustive");
    for ov in &o.variants {
        let name = ov.ident.to_string();
        let Some(nv) = n.variants.iter().find(|x| x.ident == ov.ident) else {
            v.breaking(format!("variant `{name}` removed"));
            continue;
        };
        if style(&ov.fields) != style(&nv.fields) {
            v.breaking(format!("variant `{name}` changed its kind"));
            continue;
        }
        let ofs = fields(&ov.fields);
        let nfs = fields(&nv.fields);
        // Variant fields are public as the enum is.
        let same = ofs.len() == nfs.len()
            && ofs
                .iter()
                .zip(&nfs)
                .all(|(a, b)| a.name == b.name && a.ty == b.ty);
        if !same {
            let added_only = nfs.len() > ofs.len()
                && ofs
                    .iter()
                    .all(|a| nfs.iter().any(|b| a.name == b.name && a.ty == b.ty));
            if added_only && has_attr(&ov.attrs, "non_exhaustive") {
                v.compatible(format!(
                    "fields added to `#[non_exhaustive]` variant `{name}`"
                ));
            } else {
                v.breaking(format!("fields of variant `{name}` changed"));
            }
        }
        match (
            has_attr(&ov.attrs, "non_exhaustive"),
            has_attr(&nv.attrs, "non_exhaustive"),
        ) {
            (false, true) => v.breaking(format!("variant `{name}` now `#[non_exhaustive]`")),
            (true, false) => {
                v.compatible(format!("variant `{name}` no longer `#[non_exhaustive]`"))
            }
            _ => {}
        }
        let disc = |x: &syn::Variant| {
            x.discriminant
                .as_ref()
                .map(|(_, e)| e.to_token_string_lossy())
        };
        if disc(ov) != disc(nv) {
            v.unknown(format!("discriminant of variant `{name}` changed"));
        }
    }
    for nv in &n.variants {
        if o.variants.iter().any(|x| x.ident == nv.ident) {
            continue;
        }
        if exhaustive {
            v.breaking(format!(
                "variant `{}` added: exhaustive `match`es outside the crate break",
                nv.ident
            ));
        } else {
            v.compatible(format!(
                "variant `{}` added to a `#[non_exhaustive]` enum",
                nv.ident
            ));
        }
    }
}

/// A trait's own header; its items are items of their own.
fn trait_header(o: &syn::ItemTrait, n: &syn::ItemTrait, v: &mut Verdict) {
    generics(&o.generics, &n.generics, v);
    match (o.unsafety.is_some(), n.unsafety.is_some()) {
        (false, true) => v.breaking("now an `unsafe trait`: implementations must be `unsafe impl`"),
        (true, false) => v.breaking("no longer an `unsafe trait`: `unsafe impl`s break"),
        _ => {}
    }
    let bounds = |t: &syn::ItemTrait| -> Vec<String> {
        t.supertraits
            .iter()
            .map(|b| b.to_token_string_lossy())
            .collect()
    };
    let (ob, nb) = (bounds(o), bounds(n));
    if ob != nb {
        if nb.iter().any(|b| !ob.contains(b)) {
            v.breaking("supertrait added: implementations must also implement it");
        }
        if ob.iter().any(|b| !nb.contains(b)) {
            v.breaking("supertrait removed: code that relies on it breaks");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::ItemInfo;

    fn item(id: &str, kind: Kind, sig: &str, source: &str, parent: Option<&str>) -> ItemData {
        ItemData {
            info: ItemInfo {
                id: id.into(),
                kind,
                sig: sig.into(),
                public: true,
                parent: parent.map(Into::into),
                ..Default::default()
            },
            sig_norm: sig.into(),
            body_norm: String::new(),
            source: source.into(),
            name_pos: None,
            doc_hidden: false,
            call_sites: Vec::new(),
            ext_sites: Default::default(),
        }
    }

    fn check(kind: Kind, old: &str, new: &str) -> ApiChange {
        let base_public = HashSet::new();
        let ctx = Context {
            base_public: &base_public,
        };
        let b = item("t::x", kind, old, old, None);
        let h = item("t::x", kind, new, new, None);
        classify(&ctx, Some(&b), Some(&h), &[], &[], None).unwrap()
    }

    fn level(kind: Kind, old: &str, new: &str) -> ApiLevel {
        check(kind, old, new).level
    }

    #[test]
    fn struct_fields() {
        use ApiLevel::*;
        // All fields public: adding one breaks literals.
        assert_eq!(
            level(
                Kind::Struct,
                "pub struct S { pub a: u8 }",
                "pub struct S { pub a: u8, pub b: u8 }"
            ),
            Breaking
        );
        // With a private field, adding a public one is fine.
        assert_eq!(
            level(
                Kind::Struct,
                "pub struct S { pub a: u8, c: u8 }",
                "pub struct S { pub a: u8, pub b: u8, c: u8 }"
            ),
            Compatible
        );
        assert_eq!(
            level(
                Kind::Struct,
                "pub struct S { a: u8 }",
                "pub struct S { a: u16, b: u8 }"
            ),
            Compatible
        );
        assert_eq!(
            level(
                Kind::Struct,
                "pub struct S { pub a: u8 }",
                "pub struct S { pub a: u16 }"
            ),
            Breaking
        );
        assert_eq!(
            level(
                Kind::Struct,
                "pub struct S { pub a: u8 }",
                "#[non_exhaustive]\npub struct S { pub a: u8 }"
            ),
            Breaking
        );
        assert_eq!(
            level(
                Kind::Struct,
                "#[non_exhaustive]\npub struct S { pub a: u8 }",
                "#[non_exhaustive]\npub struct S { pub a: u8, pub b: u8 }"
            ),
            Compatible
        );
        assert_eq!(
            level(
                Kind::Struct,
                "pub struct S<T> { a: T }",
                "pub struct S<T, A = ()> { a: T }"
            ),
            Compatible
        );
        assert_eq!(
            level(
                Kind::Struct,
                "pub struct S<T> { a: T }",
                "pub struct S<T, A> { a: T }"
            ),
            Breaking
        );
    }

    #[test]
    fn enum_variants() {
        use ApiLevel::*;
        assert_eq!(
            level(Kind::Enum, "pub enum E { A }", "pub enum E { A, B }"),
            Breaking
        );
        assert_eq!(
            level(
                Kind::Enum,
                "#[non_exhaustive]\npub enum E { A }",
                "#[non_exhaustive]\npub enum E { A, B }"
            ),
            Compatible
        );
        assert_eq!(
            level(Kind::Enum, "pub enum E { A(u8) }", "pub enum E { A(u16) }"),
            Breaking
        );
        let c = check(Kind::Enum, "pub enum E { A, B }", "pub enum E { A }");
        assert_eq!(c.reasons, ["variant `B` removed"]);
    }

    #[test]
    fn functions() {
        use ApiLevel::*;
        assert_eq!(
            level(Kind::Fn, "pub fn f(a: u8)", "pub const fn f(a: u8)"),
            Compatible
        );
        assert_eq!(
            level(Kind::Fn, "pub const fn f(a: u8)", "pub fn f(a: u8)"),
            Breaking
        );
        assert_eq!(
            level(Kind::Fn, "pub fn f(a: u8)", "pub unsafe fn f(a: u8)"),
            Breaking
        );
        assert_eq!(
            level(Kind::Fn, "pub unsafe fn f(a: u8)", "pub fn f(a: u8)"),
            Compatible
        );
        assert_eq!(
            level(Kind::Fn, "pub fn f(a: u8)", "pub fn f(a: u8, b: u8)"),
            Breaking
        );
        assert_eq!(
            level(Kind::Fn, "pub fn f()", "#[must_use]\npub fn f()"),
            Compatible
        );
        assert_eq!(
            level(Kind::Const, "pub const X: u8 = 1;", "pub const X: u8 = 2;"),
            Compatible
        );
        assert_eq!(
            level(Kind::Const, "pub const X: u8 = 1;", "pub const X: u16 = 1;"),
            Breaking
        );
    }

    #[test]
    fn trait_items() {
        use ApiLevel::*;
        let base_public = HashSet::from(["t::T".to_string()]);
        let ctx = Context {
            base_public: &base_public,
        };
        let new = |source: &str| {
            let h = item(
                "t::T::f",
                Kind::TraitMethod,
                "fn f(&self);",
                source,
                Some("t::T"),
            );
            classify(&ctx, None, Some(&h), &[], &[], None)
                .unwrap()
                .level
        };
        assert_eq!(new("fn f(&self);"), Breaking);
        assert_eq!(new("/// Docs.\nfn f(&self) {}"), Compatible);
        assert_eq!(
            level(Kind::Trait, "pub trait T {}", "pub trait T: Clone {}"),
            Breaking
        );
    }

    #[test]
    fn removals_and_paths() {
        let base_public = HashSet::new();
        let ctx = Context {
            base_public: &base_public,
        };
        let b = item("t::f", Kind::Fn, "pub fn f()", "pub fn f() {}", None);
        let removed = classify(&ctx, Some(&b), None, &[], &[], None).unwrap();
        assert_eq!(removed.level, ApiLevel::Breaking);
        let moved = classify(
            &ctx,
            Some(&b),
            Some(&b),
            &["t::g".into()],
            &["t::f".into()],
            None,
        )
        .unwrap();
        assert_eq!(moved.level, ApiLevel::Breaking);
        assert_eq!(moved.reasons, ["no longer public as `t::f`"]);
        // Body-only changes are no API change.
        assert!(classify(&ctx, Some(&b), Some(&b), &[], &[], None).is_none());
    }
}
