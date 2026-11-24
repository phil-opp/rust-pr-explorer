//! `unsafe` code in an item or part, and how it changed between base and
//! head.
//!
//! This works on tokens, not on the syntax tree, so that `unsafe` code
//! inside macro arguments (`vec![unsafe { … }]`) and `macro_rules!`
//! bodies is found too. Comments are not tokens, so a changed `// SAFETY:`
//! comment alone is no change.

use model::{Status, UnsafeChange, UnsafeKind};
use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// A piece of `unsafe` code on one side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site {
    pub kind: UnsafeKind,
    /// Its tokens, for comparing.
    pub text: String,
    /// What it is, for display (see [`UnsafeChange::what`]).
    pub what: String,
    /// First and last line in the file.
    pub lines: (u32, u32),
}

/// Attributes that are unsafe to use: they can make the linker resolve a
/// symbol to this item, which bypasses Rust's type checks. Since Rust 2024
/// they must be written `#[unsafe(…)]`.
const UNSAFE_ATTRS: [&str; 3] = ["no_mangle", "export_name", "link_section"];

/// The `unsafe` code in `tokens`, in source order. Code inside a site
/// (an `unsafe` block inside an `unsafe fn`) is part of it, not a site of
/// its own.
pub fn sites(tokens: TokenStream) -> Vec<Site> {
    let mut out = Vec::new();
    scan(&tokens.into_iter().collect::<Vec<_>>(), &mut out);
    out
}

fn scan(tokens: &[TokenTree], out: &mut Vec<Site>) {
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            TokenTree::Ident(id) if id == "unsafe" => {
                let end = site_end(tokens, i);
                let kind = kind_after(&tokens[i + 1..=end]);
                out.push(site(kind, &tokens[i..=end]));
                i = end + 1;
                continue;
            }
            TokenTree::Ident(id) if id == "extern" => {
                // `extern "C" { … }`: foreign functions, unsafe to call
                // (and, before Rust 2024, written without `unsafe`).
                let mut j = i + 1;
                if matches!(tokens.get(j), Some(TokenTree::Literal(_))) {
                    j += 1;
                }
                if let Some(TokenTree::Group(g)) = tokens.get(j)
                    && g.delimiter() == Delimiter::Brace
                {
                    out.push(site(UnsafeKind::Extern, &tokens[i..=j]));
                    i = j + 1;
                    continue;
                }
            }
            TokenTree::Punct(p) if p.as_char() == '#' => {
                // An outer attribute: `#[no_mangle]`, `#[unsafe(no_mangle)]`.
                if let Some(TokenTree::Group(g)) = tokens.get(i + 1)
                    && g.delimiter() == Delimiter::Bracket
                {
                    let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                    let unsafe_attr = match inner.first() {
                        Some(TokenTree::Ident(id)) if id == "unsafe" => true,
                        Some(TokenTree::Ident(id)) => UNSAFE_ATTRS.iter().any(|a| id == a),
                        _ => false,
                    };
                    if unsafe_attr {
                        out.push(site(UnsafeKind::Attr, &tokens[i..=i + 1]));
                    }
                    // Doc comments and other attributes hold no code.
                    i += 2;
                    continue;
                }
            }
            TokenTree::Group(g) => scan(&g.stream().into_iter().collect::<Vec<_>>(), out),
            _ => {}
        }
        i += 1;
    }
}

/// The last token of the site that starts with the `unsafe` at `i`: the
/// next `{ … }` group, or a `;` (`unsafe fn f();`), whichever comes first.
fn site_end(tokens: &[TokenTree], i: usize) -> usize {
    for (j, t) in tokens.iter().enumerate().skip(i + 1) {
        match t {
            TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => return j,
            TokenTree::Punct(p) if p.as_char() == ';' => return j,
            _ => {}
        }
    }
    tokens.len() - 1
}

/// What follows an `unsafe`: `{` a block, `fn`, `impl`, `trait`, or
/// `extern "C" {` a block of foreign items; `extern "C" fn` is a function.
fn kind_after(rest: &[TokenTree]) -> UnsafeKind {
    for t in rest {
        match t {
            TokenTree::Group(g) if g.delimiter() == Delimiter::Brace => {
                return if rest
                    .first()
                    .is_some_and(|f| matches!(f, TokenTree::Ident(id) if id == "extern"))
                {
                    UnsafeKind::Extern
                } else {
                    UnsafeKind::Block
                };
            }
            TokenTree::Ident(id) if id == "fn" => return UnsafeKind::Fn,
            TokenTree::Ident(id) if id == "impl" => return UnsafeKind::Impl,
            TokenTree::Ident(id) if id == "trait" => return UnsafeKind::Trait,
            // `unsafe(no_mangle)` inside an attribute is handled there;
            // anything else (`unsafe` in a macro pattern) is shown as is.
            _ => {}
        }
    }
    UnsafeKind::Block
}

fn site(kind: UnsafeKind, tokens: &[TokenTree]) -> Site {
    let stream: TokenStream = tokens.iter().cloned().collect();
    let first = tokens.first().map_or(0, |t| t.span().start().line);
    let last = tokens.last().map_or(first, |t| t.span().end().line);
    Site {
        kind,
        text: stream.to_string(),
        what: what(kind, tokens),
        lines: (first as u32, last.max(first) as u32),
    }
}

/// The site's code up to its body: `unsafe fn read(p: *const u8) -> u8`
/// becomes `unsafe fn read`, a block `unsafe { … }`.
fn what(kind: UnsafeKind, tokens: &[TokenTree]) -> String {
    let head: TokenStream = tokens
        .iter()
        .take_while(|t| !matches!(t, TokenTree::Group(g) if g.delimiter() == Delimiter::Brace))
        .cloned()
        .collect();
    let text = tidy(&head.to_string());
    match kind {
        UnsafeKind::Block => "unsafe { … }".into(),
        UnsafeKind::Attr => text,
        UnsafeKind::Extern => format!("{text} {{ … }}"),
        // The name, without parameters and generics.
        UnsafeKind::Fn | UnsafeKind::Trait => {
            let mut words = Vec::new();
            for w in text.split(' ') {
                let name = w.split(['(', '<', ':']).next().unwrap_or(w);
                words.push(name.to_string());
                if words.len() >= 2 && words[words.len() - 2].ends_with(kind_word(kind)) {
                    break;
                }
            }
            words.join(" ")
        }
        UnsafeKind::Impl => {
            let text = text.split(" where ").next().unwrap_or(&text).to_string();
            shorten(&text, 80)
        }
    }
}

fn kind_word(kind: UnsafeKind) -> &'static str {
    match kind {
        UnsafeKind::Fn => "fn",
        _ => "trait",
    }
}

/// Token text with the spaces `TokenStream`'s `Display` puts around
/// punctuation taken out where Rust code has none.
fn tidy(s: &str) -> String {
    let mut s = s.to_string();
    for (from, to) in [
        (" :: ", "::"),
        (":: ", "::"),
        (" < ", "<"),
        (" <", "<"),
        ("< ", "<"),
        (" >", ">"),
        (" ,", ","),
        ("# [", "#["),
        ("[ ", "["),
        (" ]", "]"),
        (" (", "("),
        ("( ", "("),
        (" )", ")"),
        ("& ", "&"),
        ("* const", "*const"),
        ("* mut", "*mut"),
        (" : ", ": "),
    ] {
        s = s.replace(from, to);
    }
    s
}

fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

/// How the `unsafe` code changed: sites with the same tokens on both
/// sides are unchanged and left out; the rest are paired by kind and
/// description (`unsafe fn read` with `unsafe fn read`), then by kind in
/// order, as changed; what is left is added or removed. Sorted by line.
pub fn compare(old: &[Site], new: &[Site]) -> Vec<UnsafeChange> {
    let mut old_left: Vec<&Site> = old.iter().collect();
    let mut new_left: Vec<&Site> = Vec::new();
    for n in new {
        match old_left
            .iter()
            .position(|o| o.kind == n.kind && o.text == n.text)
        {
            Some(k) => {
                old_left.remove(k);
            }
            None => new_left.push(n),
        }
    }
    let mut out = Vec::new();
    let mut unpaired = Vec::new();
    for n in new_left {
        match old_left
            .iter()
            .position(|o| o.kind == n.kind && o.what == n.what)
        {
            Some(k) => out.push(changed(old_left.remove(k), n)),
            None => unpaired.push(n),
        }
    }
    for n in unpaired {
        // Blocks have no name: pair them in order.
        match old_left
            .iter()
            .position(|o| o.kind == n.kind && n.kind == UnsafeKind::Block)
        {
            Some(k) => out.push(changed(old_left.remove(k), n)),
            None => out.push(UnsafeChange {
                kind: n.kind,
                status: Status::Added,
                what: n.what.clone(),
                lines: Some(n.lines),
                old_lines: None,
            }),
        }
    }
    out.extend(old_left.into_iter().map(|o| UnsafeChange {
        kind: o.kind,
        status: Status::Removed,
        what: o.what.clone(),
        lines: None,
        old_lines: Some(o.lines),
    }));
    out.sort_by_key(|c| (c.lines.or(c.old_lines), c.status == Status::Removed));
    out
}

fn changed(o: &Site, n: &Site) -> UnsafeChange {
    UnsafeChange {
        kind: n.kind,
        status: Status::Changed,
        what: n.what.clone(),
        lines: Some(n.lines),
        old_lines: Some(o.lines),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(code: &str) -> Vec<Site> {
        sites(code.parse().unwrap())
    }

    fn kinds(code: &str) -> Vec<(UnsafeKind, String, (u32, u32))> {
        of(code)
            .into_iter()
            .map(|s| (s.kind, s.what, s.lines))
            .collect()
    }

    #[test]
    fn finds_every_kind() {
        let code = "fn a(p: *const u8) -> u8 {\n    let x = unsafe { *p };\n    x\n}\n\
            pub unsafe fn b<T>(p: *const T) -> T { p.read() }\n\
            unsafe impl<T: Send> Send for Buf<T> {}\n\
            pub unsafe trait Zeroable: Sized {}\n\
            unsafe extern \"C\" { fn abs(x: i32) -> i32; }\n\
            extern \"C\" { fn labs(x: i64) -> i64; }\n\
            extern \"C\" fn callback() {}\n\
            #[unsafe(no_mangle)] pub fn exported() {}\n\
            #[no_mangle] pub fn old_style() {}\n\
            /// Docs that mention unsafe { code }.\nfn docs() {}\n\
            fn in_macro() { println!(\"{}\", unsafe { X }); }\n\
            trait T { unsafe fn raw(&self); }";
        let got = kinds(code);
        let want = [
            (UnsafeKind::Block, "unsafe { … }", 2),
            (UnsafeKind::Fn, "unsafe fn b", 5),
            (UnsafeKind::Impl, "unsafe impl<T: Send> Send for Buf<T>", 6),
            (UnsafeKind::Trait, "unsafe trait Zeroable", 7),
            (UnsafeKind::Extern, "unsafe extern \"C\" { … }", 8),
            (UnsafeKind::Extern, "extern \"C\" { … }", 9),
            (UnsafeKind::Attr, "#[unsafe(no_mangle)]", 11),
            (UnsafeKind::Attr, "#[no_mangle]", 12),
            (UnsafeKind::Block, "unsafe { … }", 15),
            (UnsafeKind::Fn, "unsafe fn raw", 16),
        ];
        assert_eq!(
            got.iter()
                .map(|(k, w, l)| (*k, w.as_str(), l.0))
                .collect::<Vec<_>>(),
            want
        );
    }

    #[test]
    fn a_site_covers_the_code_inside_it() {
        let s = of("unsafe fn f(p: *mut u8) {\n    unsafe {\n        *p = 1;\n    }\n}");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].lines, (1, 5));
    }

    #[test]
    fn changes_are_added_changed_or_removed() {
        let old = of("fn f() {\n    unsafe { a() };\n    unsafe { b() };\n}\n\
            unsafe impl Send for X {}");
        let new = of(
            "fn f() {\n    unsafe { a() };\n    unsafe { b2() };\n    unsafe { c() };\n}\n\
            unsafe fn g() {}",
        );
        let c = compare(&old, &new);
        let got: Vec<(Status, UnsafeKind, &str)> = c
            .iter()
            .map(|c| (c.status, c.kind, c.what.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (Status::Changed, UnsafeKind::Block, "unsafe { … }"),
                (Status::Added, UnsafeKind::Block, "unsafe { … }"),
                (Status::Removed, UnsafeKind::Impl, "unsafe impl Send for X"),
                (Status::Added, UnsafeKind::Fn, "unsafe fn g"),
            ]
        );
        assert_eq!(c[0].lines, Some((3, 3)));
        assert_eq!(c[0].old_lines, Some((3, 3)));
        // Formatting and comments are no change.
        let a = of("fn f() { unsafe { a(1, 2) } }");
        let b = of("fn f() {\n    // SAFETY: fine.\n    unsafe {\n        a(1, 2)\n    }\n}");
        assert!(compare(&a, &b).is_empty());
    }
}
