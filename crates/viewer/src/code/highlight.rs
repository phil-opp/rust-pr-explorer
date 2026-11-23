//! Syntax highlighting of Rust code, with names linked to their items.

use super::esc;
use crate::data::Data;
use model::{Ref, Status};
use std::collections::BTreeMap;

const KEYWORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "const",
    "continue",
    "crate",
    "dyn",
    "else",
    "enum",
    "extern",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "trait",
    "true",
    "type",
    "union",
    "unsafe",
    "use",
    "where",
    "while",
    "macro_rules",
];

/// A link (or, for names outside the analyzed crates, a hover target).
pub fn ref_html(d: &Data, text: &str, r: &Ref, extra: &str) -> String {
    let item = match r.id.as_deref() {
        Some(id) => d.item(id),
        // Outside the analyzed crates: linked if rust-analyzer described it.
        None => d.external(&r.path).and_then(|_| d.item(&r.path)),
    };
    if let Some(item) = item {
        format!(
            r#"<a class="ref {extra}" data-id="{}" data-kind="{}" data-path="{}">{}</a>"#,
            esc(&item.info.id),
            item.info.kind.css(),
            esc(&r.path),
            esc(text)
        )
    } else {
        format!(
            r#"<span class="ref ext {extra}" data-path="{}">{}</span>"#,
            esc(&r.path),
            esc(text)
        )
    }
}

/// Classes that point out, in source views, a reference to an item that
/// the PR adds (`new`), or a call of a function that it changes (`chg`),
/// as each of them needs a review of its own. Names of changed types are
/// not marked: they are everywhere, and calls are what a top-down review
/// follows. Once that item is reviewed, `rv-ok` adds a ✓ in review
/// mode (kept up to date by `review::refresh`). Not for references of an
/// item to itself (a recursive call), which need no review of their own.
fn mark_class(d: &Data, r: &Ref, of: &str) -> &'static str {
    let Some(it) =
        r.id.as_deref()
            .filter(|&id| id != of)
            .and_then(|id| d.item(id))
    else {
        return "";
    };
    if d.external(&it.info.id).is_some() || !crate::review::is_unit_item(it) {
        return "";
    }
    let done = crate::review::mark(&it.info.id) == Some(crate::review::Mark::Done);
    match (it.status, done) {
        (Status::Added, false) => "new",
        (Status::Added, true) => "new rv-ok",
        (Status::Changed, false) if it.info.kind.is_callable() => "chg",
        (Status::Changed, true) if it.info.kind.is_callable() => "chg rv-ok",
        _ => "",
    }
}

/// Where names in rendered code may be linked. Links are only ever placed
/// where the analysis recorded a resolved path, never by matching text.
#[derive(Clone, Copy)]
pub enum Links<'a> {
    None,
    /// (Part of) a signature: `text` starts at char offset `base` of the
    /// signature, and `at` are the offsets where resolved paths start.
    Sig {
        refs: &'a BTreeMap<String, Ref>,
        at: &'a [u32],
        base: usize,
    },
    /// A line of source: names are linked by `line:column`. `of` is the
    /// item the source belongs to.
    Source {
        line: usize,
        refs: &'a BTreeMap<String, Ref>,
        of: &'a str,
    },
}

/// A path shown without a link (keywords still highlighted).
fn plain_path(path: &str) -> String {
    path.split("::")
        .map(|seg| {
            if KEYWORDS.contains(&seg) {
                format!(r#"<span class="kw">{seg}</span>"#)
            } else {
                esc(seg)
            }
        })
        .collect::<Vec<_>>()
        .join("::")
}

/// Syntax-highlights Rust code, linking names as `links` allows.
pub fn code_html(d: &Data, text: &str, links: Links) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let span = |out: &mut String, class: &str, s: &[char]| {
        let s: String = s.iter().collect();
        out.push_str(&format!(r#"<span class="{class}">{}</span>"#, esc(&s)));
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            let end = chars[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |p| i + p);
            span(&mut out, "cm", &chars[i..end]);
            i = end;
        } else if c == '"'
            || (c == 'r' && matches!(next, Some('"') | Some('#')) && is_raw_string(&chars[i..]))
        {
            let end = string_end(&chars, i);
            span(&mut out, "str", &chars[i..end]);
            i = end;
        } else if c == '\'' {
            // Char literal or lifetime.
            if let Some(end) = char_literal_end(&chars, i) {
                span(&mut out, "str", &chars[i..end]);
                i = end;
            } else {
                let end = ident_end(&chars, i + 1);
                span(&mut out, "kw", &chars[i..end]);
                i = end.max(i + 1);
            }
        } else if c == '#'
            && (next == Some('[') || (next == Some('!') && chars.get(i + 2) == Some(&'[')))
        {
            let end = chars[i..]
                .iter()
                .position(|&c| c == ']' || c == '\n')
                .map_or(chars.len(), |p| i + p + 1)
                .min(chars.len());
            span(&mut out, "attr", &chars[i..end]);
            i = end;
        } else if c.is_ascii_digit() && !prev_is_ident(&chars, i) {
            let mut end = i;
            while end < chars.len()
                && (chars[end].is_ascii_alphanumeric()
                    || chars[end] == '_'
                    || (chars[end] == '.'
                        && chars.get(end + 1).is_some_and(|c| c.is_ascii_digit())))
            {
                end += 1;
            }
            span(&mut out, "num", &chars[i..end]);
            i = end;
        } else if is_ident_start(c) || (c == '.' && next.is_some_and(is_ident_start)) {
            let start = i;
            let mut end = if c == '.' { i + 1 } else { i };
            end = ident_end(&chars, end);
            while end + 2 < chars.len() + 1
                && chars.get(end) == Some(&':')
                && chars.get(end + 1) == Some(&':')
                && chars.get(end + 2).is_some_and(|&c| is_ident_start(c))
            {
                end = ident_end(&chars, end + 2);
            }
            let path: String = chars[start..end].iter().collect();
            if let Some(name) = path.strip_prefix('.') {
                // A method call or field access.
                out.push('.');
                match links {
                    Links::Source { line, refs, of } => {
                        match refs.get(&format!("{line}:{}", start + 1)) {
                            Some(r) => out.push_str(&ref_html(d, name, r, mark_class(d, r, of))),
                            None => out.push_str(&esc(name)),
                        }
                    }
                    _ => out.push_str(&esc(name)),
                }
            } else {
                match links {
                    Links::Sig { refs, at, base }
                        if at.binary_search(&((base + start) as u32)).is_ok() =>
                    {
                        out.push_str(&path_html(d, &path, refs))
                    }
                    Links::Source { line, refs, of } => {
                        match refs.get(&format!("{line}:{start}")) {
                            Some(r) => out.push_str(&ref_html(d, &path, r, mark_class(d, r, of))),
                            None => out.push_str(&plain_path(&path)),
                        }
                    }
                    _ => out.push_str(&plain_path(&path)),
                }
            }
            i = end;
        } else {
            out.push_str(&esc(&c.to_string()));
            i += 1;
        }
    }
    out
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn prev_is_ident(chars: &[char], i: usize) -> bool {
    i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')
}

fn ident_end(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
        i += 1;
    }
    i
}

fn is_raw_string(chars: &[char]) -> bool {
    let hashes = chars[1..].iter().take_while(|&&c| c == '#').count();
    chars.get(1 + hashes) == Some(&'"')
}

fn string_end(chars: &[char], start: usize) -> usize {
    if chars[start] == 'r' {
        let hashes = chars[start + 1..].iter().take_while(|&&c| c == '#').count();
        let body = start + 2 + hashes;
        let mut i = body;
        while i < chars.len() {
            if chars[i] == '"'
                && chars[i + 1..]
                    .iter()
                    .take(hashes)
                    .filter(|&&c| c == '#')
                    .count()
                    == hashes
            {
                return (i + 1 + hashes).min(chars.len());
            }
            i += 1;
        }
        return chars.len();
    }
    let mut i = start + 1;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '"' => return i + 1,
            _ => i += 1,
        }
    }
    chars.len()
}

fn char_literal_end(chars: &[char], start: usize) -> Option<usize> {
    match chars.get(start + 1)? {
        '\\' => {
            let close = chars[start + 2..]
                .iter()
                .take(10)
                .position(|&c| c == '\'')?;
            Some(start + 2 + close + 1)
        }
        _ if chars.get(start + 2) == Some(&'\'') => Some(start + 3),
        _ => None,
    }
}

fn path_html(d: &Data, path: &str, refs: &BTreeMap<String, Ref>) -> String {
    if let Some(name) = path.strip_prefix('.') {
        return match refs.get(path) {
            Some(r) => format!(".{}", ref_html(d, name, r, "")),
            None => esc(path),
        };
    }
    if KEYWORDS.contains(&path) {
        return format!(r#"<span class="kw">{path}</span>"#);
    }
    if let Some(r) = refs.get(path) {
        return ref_html(d, path, r, "");
    }
    // Link the longest known prefix and continue with the rest.
    let segs: Vec<&str> = path.split("::").collect();
    for n in (1..segs.len()).rev() {
        let head = segs[..n].join("::");
        if let Some(r) = refs.get(&head) {
            return format!(
                "{}::{}",
                ref_html(d, &head, r, ""),
                path_html(d, &segs[n..].join("::"), refs)
            );
        }
    }
    if segs.len() > 1 {
        return segs
            .iter()
            .map(|s| path_html(d, s, refs))
            .collect::<Vec<_>>()
            .join("::");
    }
    esc(path)
}
