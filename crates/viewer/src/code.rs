//! Rust syntax highlighting with cross links, and doc comment rendering.

use crate::data::Data;
use model::Ref;
use pulldown_cmark::{BrokenLink, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};
use std::collections::BTreeMap;

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

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
    if let Some(item) = r.id.as_deref().and_then(|id| d.item(id)) {
        format!(
            r#"<a class="ref {} {extra}" data-id="{}" data-kind="{}" data-path="{}">{}</a>"#,
            if r.inferred { "inferred" } else { "" },
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

/// Syntax-highlights Rust code and links every name found in `refs`.
pub fn code_html(d: &Data, text: &str, refs: &BTreeMap<String, Ref>) -> String {
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
            out.push_str(&path_html(d, &path, refs));
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

/// Renders doc comment markdown. Intra-doc links become cross links; raw
/// HTML from the docs is shown as text, never interpreted.
pub fn docs_html(d: &Data, md: &str, refs: &BTreeMap<String, Ref>) -> String {
    if md.trim().is_empty() {
        return String::new();
    }
    // Unresolved `[links]` keep their text but lose the brackets.
    let callback = |link: BrokenLink<'_>| -> Option<(CowStr<'_>, CowStr<'_>)> {
        Some((format!("rpe-ref:{}", link.reference).into(), "".into()))
    };
    let parser = Parser::new_with_broken_link_callback(md, Options::ENABLE_TABLES, Some(callback));
    let mut events: Vec<Event> = Vec::new();
    // Closing tag for each open link, `None` for links shown as plain text.
    let mut link_stack: Vec<Option<&'static str>> = Vec::new();
    let mut code: Option<(bool, String)> = None;
    for event in parser {
        if let Some((rust, buf)) = &mut code {
            match event {
                Event::Text(t) => buf.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    let text: String = buf
                        .lines()
                        .filter(|l| {
                            !(*rust && (l.trim() == "#" || l.trim_start().starts_with("# ")))
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let html = if *rust {
                        code_html(d, &text, &BTreeMap::new())
                    } else {
                        esc(&text)
                    };
                    events.push(Event::Html(format!("<pre>{html}</pre>").into()));
                    code = None;
                }
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                let rust = match kind {
                    CodeBlockKind::Indented => true,
                    CodeBlockKind::Fenced(lang) => {
                        lang.is_empty()
                            || lang.split(',').any(|l| {
                                matches!(
                                    l.trim(),
                                    "rust" | "no_run" | "ignore" | "should_panic" | "compile_fail"
                                )
                            })
                    }
                };
                code = Some((rust, String::new()));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let target = dest_url
                    .strip_prefix("rpe-ref:")
                    .unwrap_or(&dest_url)
                    .to_string();
                if let Some(r) = refs.get(&target) {
                    let open = ref_html(d, "", r, "");
                    // Keep the opening tag only; the link text follows as events.
                    let close_at = open.rfind("</").unwrap_or(open.len());
                    let close = if open.starts_with("<a") {
                        "</a>"
                    } else {
                        "</span>"
                    };
                    events.push(Event::Html(open[..close_at].to_string().into()));
                    link_stack.push(Some(close));
                } else if dest_url.starts_with("https://") || dest_url.starts_with("http://") {
                    events.push(Event::Html(
                        format!(
                            r#"<a href="{}" target="_blank" rel="noopener">"#,
                            esc(&dest_url)
                        )
                        .into(),
                    ));
                    link_stack.push(Some("</a>"));
                } else {
                    link_stack.push(None);
                }
            }
            Event::End(TagEnd::Link) => {
                if let Some(Some(close)) = link_stack.pop() {
                    events.push(Event::Html(close.into()));
                }
            }
            Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image) => {}
            Event::Html(h) | Event::InlineHtml(h) => events.push(Event::Text(h)),
            other => events.push(other),
        }
    }
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, events.into_iter());
    out
}

/// The first paragraph of some docs.
pub fn first_paragraph(md: &str) -> &str {
    let md = md.trim();
    let mut end = md.len();
    let mut offset = 0;
    for line in md.split_inclusive('\n') {
        if line.trim().is_empty() {
            end = offset;
            break;
        }
        offset += line.len();
    }
    &md[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Meta, Output};

    fn empty() -> Data {
        Data::new(Output {
            meta: Meta {
                title: String::new(),
                base: String::new(),
                head: String::new(),
                url: None,
                resolution: String::new(),
            },
            crates: vec!["c".into()],
            items: BTreeMap::new(),
            warnings: Vec::new(),
        })
    }

    #[test]
    fn docs_never_contain_markup_from_the_source() {
        let d = empty();
        let html = docs_html(
            &d,
            "Hi <script>alert(1)</script> and <img src=x onerror=y>\n\n[x](javascript:alert(1))",
            &BTreeMap::new(),
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<img"), "{html}");
        assert!(!html.contains("javascript:"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    #[test]
    fn intra_doc_links_and_code_blocks() {
        let d = empty();
        let mut refs = BTreeMap::new();
        refs.insert(
            "`Vec`".to_string(),
            Ref {
                id: None,
                path: "alloc::vec::Vec".into(),
                inferred: false,
            },
        );
        let html = docs_html(
            &d,
            "Uses [`Vec`] and [`Missing`].\n\n```\nlet x = 1;\n# hidden();\n```",
            &refs,
        );
        assert!(html.contains(r#"data-path="alloc::vec::Vec""#), "{html}");
        assert!(html.contains("<code>Missing</code>"), "{html}");
        assert!(!html.contains("[<code>Missing"), "{html}");
        assert!(!html.contains("hidden"), "{html}");
        assert!(html.contains(r#"<span class="kw">let</span>"#), "{html}");
    }

    #[test]
    fn highlighting() {
        let d = empty();
        let html = code_html(
            &d,
            "fn f<'a>(c: char) -> &'a str { '\\n'; \"<b>\" } // x",
            &BTreeMap::new(),
        );
        assert!(html.contains(r#"<span class="kw">'a</span>"#), "{html}");
        assert!(html.contains(r#"<span class="str">'\n'</span>"#), "{html}");
        assert!(
            html.contains(r#"<span class="str">&quot;&lt;b&gt;&quot;</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"<span class="cm">// x</span>"#), "{html}");
    }

    #[test]
    fn module_containment() {
        use crate::data::within;
        assert!(within("a::b", "a"));
        assert!(within("a", "a"));
        assert!(!within("ab", "a"));
        assert!(!within("a", "a::b"));
    }
}
