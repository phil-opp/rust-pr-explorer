//! Rust syntax highlighting with cross links, and doc comment rendering.

use crate::data::Data;
use model::{DiffSpan, Ref, SigDiff};
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
    /// A line of source: names are linked by `line:column`.
    Source {
        line: usize,
        refs: &'a BTreeMap<String, Ref>,
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
                    Links::Source { line, refs } => {
                        match refs.get(&format!("{line}:{}", start + 1)) {
                            Some(r) => out.push_str(&ref_html(d, name, r, "")),
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
                    Links::Source { line, refs } => match refs.get(&format!("{line}:{start}")) {
                        Some(r) => out.push_str(&ref_html(d, &path, r, "")),
                        None => out.push_str(&plain_path(&path)),
                    },
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

/// Splits code into identifiers, whitespace runs and single other
/// characters: the units a signature diff works on.
fn diff_tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let same = |n: char| {
            (c.is_alphanumeric() || c == '_') && (n.is_alphanumeric() || n == '_')
                || c.is_whitespace() && n.is_whitespace()
        };
        match chars.peek() {
            Some(&(_, n)) if same(n) => {}
            _ => {
                let end = chars.peek().map_or(s.len(), |&(j, _)| j);
                out.push(&s[start..end]);
                start = end;
            }
        }
        let _ = i;
    }
    out
}

/// A signature as a token-level diff against its previous version: removed
/// parts struck through, added parts highlighted. Names in the new version
/// stay cross links.
pub fn sig_diff_html(
    d: &Data,
    old: &str,
    new: &str,
    refs: &BTreeMap<String, Ref>,
    at: &[u32],
) -> String {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    let (a, b) = (diff_tokens(old), diff_tokens(new));
    // Char offset in `new` of each token.
    let mut offsets = vec![0];
    for t in &b {
        offsets.push(offsets.last().unwrap() + t.chars().count());
    }
    let links = |index: usize| Links::Sig {
        refs,
        at,
        base: offsets[index],
    };
    let mut out = String::new();
    let mut g = 0;
    for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
        g += 1;
        match op {
            DiffOp::Equal { new_index, len, .. } => out.push_str(&code_html(
                d,
                &b[new_index..new_index + len].concat(),
                links(new_index),
            )),
            DiffOp::Delete {
                old_index, old_len, ..
            } => out.push_str(&removed_html(
                d,
                &a[old_index..old_index + old_len].concat(),
                g,
            )),
            DiffOp::Insert {
                new_index, new_len, ..
            } => out.push_str(&added_html(
                d,
                &b[new_index..new_index + new_len].concat(),
                links(new_index),
                g,
            )),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                // One group: hovering either half highlights both.
                out.push_str(&removed_html(
                    d,
                    &a[old_index..old_index + old_len].concat(),
                    g,
                ));
                out.push_str(&added_html(
                    d,
                    &b[new_index..new_index + new_len].concat(),
                    links(new_index),
                    g,
                ));
            }
        }
    }
    out
}

fn added_html(d: &Data, text: &str, links: Links, g: u32) -> String {
    format!(
        r#"<ins class="sd" data-g="{g}">{}</ins>"#,
        code_html(d, text, links)
    )
}

/// Removed text is base code: what its names referred to is not known
/// here, so it gets no links.
fn removed_html(d: &Data, text: &str, g: u32) -> String {
    format!(
        r#"<del class="sd" data-g="{g}">{}</del>"#,
        code_html(d, text, Links::None)
    )
}

/// Renders diff spans; `cursor` is the char offset in the new signature.
fn spans_html(
    d: &Data,
    spans: &[DiffSpan],
    refs: &BTreeMap<String, Ref>,
    at: &[u32],
    cursor: &mut usize,
) -> String {
    let mut out = String::new();
    let new_text = |text: &str, cursor: &mut usize| {
        let links = Links::Sig {
            refs,
            at,
            base: *cursor,
        };
        *cursor += text.chars().count();
        links
    };
    for span in spans {
        match span {
            DiffSpan::Same { text } => {
                let links = new_text(text, cursor);
                out.push_str(&code_html(d, text, links))
            }
            DiffSpan::Added { text, g } => {
                let links = new_text(text, cursor);
                out.push_str(&added_html(d, text, links, *g))
            }
            DiffSpan::Removed { text, g } => out.push_str(&removed_html(d, text, *g)),
            DiffSpan::Wrap {
                before,
                inner,
                after,
                unwrap,
                g,
            } => {
                // One box around the whole wrapper, the unchanged inner type
                // inside it uncoloured. A removed wrapper is base text.
                let (class, edge) = if *unwrap {
                    ("wrap unwrap", "del")
                } else {
                    ("wrap", "ins")
                };
                let before_html = if *unwrap {
                    code_html(d, before, Links::None)
                } else {
                    let links = new_text(before, cursor);
                    code_html(d, before, links)
                };
                let inner_html = spans_html(d, inner, refs, at, cursor);
                let after_html = if *unwrap {
                    code_html(d, after, Links::None)
                } else {
                    let links = new_text(after, cursor);
                    code_html(d, after, links)
                };
                out.push_str(&format!(
                    r#"<span class="{class}" data-g="{g}" title="{}"><{edge} class="edge">{before_html}</{edge}>{inner_html}<{edge} class="edge">{after_html}</{edge}></span>"#,
                    if *unwrap { "wrapper removed" } else { "wrapper added" },
                ));
            }
        }
    }
    out
}

/// Per-line change markers (`+`, `-`, `~`, or space) of a diff, for the
/// gutter of multi-line definitions.
fn line_markers(spans: &[DiffSpan]) -> Vec<char> {
    #[derive(Default, Clone, Copy)]
    struct Line {
        same: bool,
        added: bool,
        removed: bool,
    }
    fn push(lines: &mut Vec<Line>, text: &str, set: &dyn Fn(&mut Line)) {
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                lines.push(Line::default());
            }
            if !part.trim().is_empty() {
                set(lines.last_mut().unwrap());
            }
        }
    }
    fn walk(spans: &[DiffSpan], lines: &mut Vec<Line>, in_wrap: bool) {
        for span in spans {
            match span {
                DiffSpan::Same { text } => push(lines, text, &|l| {
                    if in_wrap {
                        l.added = true;
                        l.removed = true
                    } else {
                        l.same = true
                    }
                }),
                DiffSpan::Added { text, .. } => push(lines, text, &|l| l.added = true),
                DiffSpan::Removed { text, .. } => push(lines, text, &|l| l.removed = true),
                DiffSpan::Wrap {
                    before,
                    inner,
                    after,
                    ..
                } => {
                    push(lines, before, &|l| {
                        l.added = true;
                        l.removed = true;
                    });
                    walk(inner, lines, true);
                    push(lines, after, &|l| {
                        l.added = true;
                        l.removed = true;
                    });
                }
            }
        }
    }
    let mut lines = vec![Line::default()];
    walk(spans, &mut lines, false);
    lines
        .iter()
        .map(|l| match (l.same, l.added, l.removed) {
            (_, false, false) => ' ',
            (false, true, false) => '+',
            (false, false, true) => '-',
            _ => '~',
        })
        .collect()
}

/// Text of a diff note: backticks become code.
fn note_html(text: &str) -> String {
    let mut out = String::new();
    for (i, part) in text.split('`').enumerate() {
        if i % 2 == 1 {
            out.push_str(&format!("<code>{}</code>", esc(part)));
        } else {
            out.push_str(&esc(part));
        }
    }
    out
}

/// A structured signature diff: the signature (with a change gutter for
/// multi-line definitions) and one note per change. Hovering a change
/// or a note highlights everything that belongs to it.
pub fn structured_diff_html(
    d: &Data,
    diff: &SigDiff,
    refs: &BTreeMap<String, Ref>,
    at: &[u32],
) -> String {
    let code = spans_html(d, &diff.spans, refs, at, &mut 0);
    let markers = line_markers(&diff.spans);
    let sig = if markers.len() > 1 {
        let gutter: String = markers
            .iter()
            .map(|m| {
                format!(
                    r#"<span class="m m{}">{}</span>"#,
                    match m {
                        '+' => "a",
                        '-' => "r",
                        '~' => "c",
                        _ => "n",
                    },
                    // A line changed in place is shown by the marks in
                    // it; the gutter only flags whole lines.
                    if *m == '~' { ' ' } else { *m }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            r#"<div class="sig-lines"><pre class="sig-gutter">{gutter}</pre><pre class="sig sig-diff">{code}</pre></div>"#
        )
    } else {
        format!(r#"<pre class="sig sig-diff">{code}</pre>"#)
    };
    let notes: String = diff
        .notes
        .iter()
        .map(|n| {
            let groups = n
                .groups
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            format!(r#"<li data-gs="{groups}">{}</li>"#, note_html(&n.text))
        })
        .collect();
    format!(r#"<div class="sig-block">{sig}<ul class="sig-notes">{notes}</ul></div>"#)
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
                        code_html(d, &text, Links::None)
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
/// A line that defines a link reference (`[name]: target`).
fn is_link_def(line: &str) -> bool {
    let t = line.trim_start();
    line.len() - t.len() <= 3
        && t.starts_with('[')
        && t.find("]:").is_some_and(|i| !t[1..i].contains(['[', ']']))
}

/// Splits Markdown into blocks separated by blank lines (not inside code
/// fences), plus its link reference definitions, which every block needs.
fn doc_blocks(md: &str) -> (Vec<String>, String) {
    let mut blocks = Vec::new();
    let mut cur = String::new();
    let mut fence: Option<&str> = None;
    for line in md.lines() {
        let t = line.trim_start();
        match fence {
            Some(f) if t.starts_with(f) => fence = None,
            Some(_) => {}
            None if t.starts_with("```") => fence = Some("```"),
            None if t.starts_with("~~~") => fence = Some("~~~"),
            None if t.is_empty() => {
                if !cur.is_empty() {
                    blocks.push(std::mem::take(&mut cur));
                }
                continue;
            }
            None => {}
        }
        cur.push_str(line);
        cur.push('\n');
    }
    if !cur.is_empty() {
        blocks.push(cur);
    }
    let defs: String = md
        .lines()
        .filter(|l| is_link_def(l))
        .map(|l| format!("{l}\n"))
        .collect();
    (blocks, defs)
}

/// A part of an item's view (signature, a doc paragraph, the source, …)
/// and the change marker it gets: `m-add`, `m-del`, `m-chg`, or none.
pub struct Seg {
    pub bar: &'static str,
    pub html: String,
}

impl Seg {
    pub fn new(bar: &'static str, html: String) -> Seg {
        Seg { bar, html }
    }
}

/// Renders parts in order. Neighboring parts with the same marker share
/// one continuous bar, so an added item has a single bar along all of it.
pub fn marked_html(segs: impl IntoIterator<Item = Seg>) -> String {
    let mut out = String::new();
    let mut run: Option<(&'static str, String)> = None;
    let flush = |out: &mut String, run: &mut Option<(&'static str, String)>| {
        if let Some((bar, html)) = run.take() {
            out.push_str(&format!(r#"<div class="mark {bar}">{html}</div>"#));
        }
    };
    for seg in segs {
        if seg.html.is_empty() {
            continue;
        }
        match &mut run {
            Some((bar, html)) if *bar == seg.bar => html.push_str(&seg.html),
            _ => {
                flush(&mut out, &mut run);
                if seg.bar.is_empty() {
                    out.push_str(&seg.html);
                } else {
                    run = Some((seg.bar, seg.html));
                }
            }
        }
    }
    flush(&mut out, &mut run);
    out
}

/// Docs with their changes marked paragraph by paragraph (a list or code
/// block counts as one): added, changed and removed ones get a marker,
/// removed ones are struck out, and a changed paragraph shows either its
/// changed words or, if that isn't safe, its previous text on demand.
pub fn docs_diff_segs(d: &Data, old: &str, new: &str, refs: &BTreeMap<String, Ref>) -> Vec<Seg> {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    let (old, new) = (clean_marks(old), clean_marks(new));
    let (a, a_defs) = doc_blocks(&old);
    let (b, b_defs) = doc_blocks(&new);
    let no_refs = BTreeMap::new();
    let block = |text: &str, defs: &str, refs: &BTreeMap<String, Ref>, bar: &'static str| {
        let html = if text.lines().all(is_link_def) {
            // Definitions render as nothing; show a changed one as text.
            if bar.is_empty() {
                String::new()
            } else {
                format!(
                    r#"<div class="docs"><div class="dblk"><code>{}</code></div></div>"#,
                    esc(text.trim())
                )
            }
        } else {
            format!(
                r#"<div class="docs"><div class="dblk">{}</div></div>"#,
                docs_html(d, &format!("{text}\n{defs}"), refs)
            )
        };
        Seg::new(bar, html)
    };
    let mut out = Vec::new();
    for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
        let (old_range, new_range) = (op.old_range(), op.new_range());
        match op {
            DiffOp::Equal { .. } => {
                for t in &b[new_range] {
                    out.push(block(t, &b_defs, refs, ""));
                }
            }
            DiffOp::Insert { .. } => {
                for t in &b[new_range] {
                    out.push(block(t, &b_defs, refs, "m-add"));
                }
            }
            DiffOp::Delete { .. } => {
                for t in &a[old_range] {
                    out.push(block(t, &a_defs, &no_refs, "m-del"));
                }
            }
            DiffOp::Replace { .. }
                if old_range.len() == new_range.len()
                    && old_range
                        .clone()
                        .zip(new_range.clone())
                        .all(|(i, j)| word_diff_md(&a[i], &b[j]).is_some()) =>
            {
                // Paragraphs that changed a few words: mark the words.
                for (i, j) in old_range.zip(new_range) {
                    let md = word_diff_md(&a[i], &b[j]).unwrap_or_default();
                    out.push(Seg::new(
                        "m-chg",
                        format!(
                            r#"<div class="docs"><div class="dblk">{}</div></div>"#,
                            marked_docs_html(d, &format!("{md}\n{b_defs}"), refs)
                        ),
                    ));
                }
            }
            DiffOp::Replace { .. } => {
                for t in &b[new_range] {
                    out.push(block(t, &b_defs, refs, "m-chg"));
                }
                let previous = marked_html(
                    a[old_range]
                        .iter()
                        .map(|t| block(t, &a_defs, &no_refs, "m-del")),
                );
                out.push(Seg::new(
                    "",
                    format!(
                        r#"<details class="dblk-old"><summary>previous text</summary>{previous}</details>"#
                    ),
                ));
            }
        }
    }
    out
}

/// [`docs_diff_segs`], rendered on their own.
#[cfg(test)]
pub fn docs_diff_html(d: &Data, old: &str, new: &str, refs: &BTreeMap<String, Ref>) -> String {
    marked_html(docs_diff_segs(d, old, new, refs))
}

// Private-use characters that mark word changes in Markdown before it is
// rendered; they are turned into tags afterwards.
const INS_OPEN: char = '\u{E000}';
const INS_CLOSE: char = '\u{E001}';
const DEL_OPEN: char = '\u{E002}';
const DEL_CLOSE: char = '\u{E003}';

/// Words, runs of whitespace, and single other characters.
fn word_tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    for k in 0..chars.len() {
        let (i, c) = chars[k];
        let end = chars.get(k + 1).map_or(s.len(), |&(j, _)| j);
        let next = chars.get(k + 1).map(|&(_, n)| class(n));
        if class(c) == 2 || next != Some(class(c)) {
            out.push(&s[start..end]);
            start = end;
        }
        let _ = i;
    }
    out
}

/// A paragraph's changes word by word, as Markdown with change marks, if
/// that can be shown without changing how the Markdown parses: the change
/// must not touch Markdown syntax, link text or targets, or the start of
/// a line, and most of the text must be unchanged. `None` otherwise.
fn word_diff_md(old: &str, new: &str) -> Option<String> {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    let fenced = |t: &str| {
        let t = t.trim_start();
        t.starts_with("```") || t.starts_with("~~~") || t.starts_with("    ")
    };
    if fenced(old) || fenced(new) || old.lines().any(is_link_def) || new.lines().any(is_link_def) {
        return None;
    }
    let (a, b) = (word_tokens(old), word_tokens(new));
    // Per token: whether it may be marked (not syntax, not inside a link's
    // brackets or target, not at the start of a line).
    let markable = |toks: &[&str]| -> Vec<bool> {
        let mut out = Vec::with_capacity(toks.len());
        let (mut bracket, mut target, mut angle) = (0i32, false, false);
        let mut line_start = true;
        let mut prev = "";
        for t in toks {
            let syntax = t.chars().all(|c| "*_[]()<>#!|~`\\-+=:".contains(c))
                || t.starts_with('_')
                || t.ends_with('_');
            let ws = t.chars().all(char::is_whitespace);
            let inside = bracket > 0 || target || angle;
            out.push(!syntax && !inside && !(line_start && !ws));
            match *t {
                "[" => bracket += 1,
                "]" => bracket -= 1,
                "(" if prev == "]" => target = true,
                ")" if target => target = false,
                "<" => angle = true,
                ">" => angle = false,
                _ => {}
            }
            if t.contains('\n') {
                line_start = true;
            } else if !ws {
                line_start = false;
            }
            prev = t;
        }
        out
    };
    let (ma, mb) = (markable(&a), markable(&b));
    let ops = capture_diff_slices(Algorithm::Myers, &a, &b);
    let words = |toks: &[&str]| toks.iter().filter(|t| !t.trim().is_empty()).count();
    let same: usize = ops
        .iter()
        .filter_map(|op| match op {
            DiffOp::Equal { new_index, len, .. } => Some(words(&b[*new_index..new_index + len])),
            _ => None,
        })
        .sum();
    if same * 2 < words(&a).max(words(&b)) {
        return None;
    }
    // Segments: unchanged token ranges, or changes (old range, new range).
    // Changes separated only by whitespace are merged, so that a rewritten
    // phrase reads as one removal and one insertion.
    enum Seg {
        Same(std::ops::Range<usize>),
        Change(std::ops::Range<usize>, std::ops::Range<usize>),
    }
    let mut segs: Vec<Seg> = Vec::new();
    for op in ops {
        let (o, n) = (op.old_range(), op.new_range());
        match op {
            DiffOp::Equal { .. } => segs.push(Seg::Same(n)),
            _ => {
                let ws_between = match segs.as_slice() {
                    [.., Seg::Change(..), Seg::Same(r)] => {
                        b[r.clone()].iter().all(|t| t.trim().is_empty())
                    }
                    _ => false,
                };
                if ws_between {
                    let Some(Seg::Same(r)) = segs.pop() else {
                        unreachable!()
                    };
                    let Some(Seg::Change(po, pn)) = segs.pop() else {
                        unreachable!()
                    };
                    // The whitespace is in both versions.
                    let olen = r.len();
                    segs.push(Seg::Change(
                        po.start..po.end + olen + o.len(),
                        pn.start..n.end,
                    ));
                } else if let Some(Seg::Change(po, pn)) = segs.last_mut()
                    && po.end == o.start
                    && pn.end == n.start
                {
                    po.end = o.end;
                    pn.end = n.end;
                } else {
                    segs.push(Seg::Change(o, n));
                }
            }
        }
    }
    let mut out = String::new();
    let mark = |out: &mut String, toks: &[&str], ok: &[bool], open: char, close: char| -> bool {
        if toks.is_empty() {
            return true;
        }
        if toks.iter().any(|t| t.contains('\n')) || !ok.iter().all(|&m| m) {
            return false;
        }
        out.push(open);
        out.push_str(&toks.concat());
        out.push(close);
        true
    };
    for seg in segs {
        let fine = match seg {
            Seg::Same(n) => {
                out.push_str(&b[n].concat());
                true
            }
            Seg::Change(o, n) => {
                mark(&mut out, &a[o.clone()], &ma[o], DEL_OPEN, DEL_CLOSE)
                    && mark(&mut out, &b[n.clone()], &mb[n], INS_OPEN, INS_CLOSE)
            }
        };
        if !fine {
            return None;
        }
    }
    Some(out)
}

/// Renders Markdown carrying word change marks.
fn marked_docs_html(d: &Data, md: &str, refs: &BTreeMap<String, Ref>) -> String {
    docs_html(d, md, refs)
        .replace(INS_OPEN, r#"<ins class="wd">"#)
        .replace(INS_CLOSE, "</ins>")
        .replace(DEL_OPEN, r#"<del class="wd">"#)
        .replace(DEL_CLOSE, "</del>")
}

/// Docs before and after, side by side, with changed paragraphs aligned.
pub fn docs_compare_html(d: &Data, it: &model::Item) -> String {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    let old = clean_marks(it.old_docs.as_deref().unwrap_or(""));
    let new = clean_marks(&it.info.docs);
    let (a, a_defs) = doc_blocks(&old);
    let (b, b_defs) = doc_blocks(&new);
    let no_refs = BTreeMap::new();
    let render =
        |blocks: &[String], defs: &str, refs: &BTreeMap<String, Ref>, bar: &'static str| {
            marked_html(blocks.iter().map(|t| {
                Seg::new(
                    bar,
                    format!(
                        r#"<div class="dblk">{}</div>"#,
                        docs_html(d, &format!("{t}\n{defs}"), refs)
                    ),
                )
            }))
        };
    let mut rows = String::new();
    for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
        let (o, n) = (op.old_range(), op.new_range());
        let (left, right) = match op {
            DiffOp::Equal { .. } => (
                render(&a[o], &a_defs, &no_refs, ""),
                render(&b[n], &b_defs, &it.info.doc_refs, ""),
            ),
            DiffOp::Delete { .. } => (render(&a[o], &a_defs, &no_refs, "m-del"), String::new()),
            DiffOp::Insert { .. } => (
                String::new(),
                render(&b[n], &b_defs, &it.info.doc_refs, "m-add"),
            ),
            DiffOp::Replace { .. } => (
                render(&a[o], &a_defs, &no_refs, "m-chg"),
                render(&b[n], &b_defs, &it.info.doc_refs, "m-chg"),
            ),
        };
        rows.push_str(&format!(
            r#"<div class="cmp-cell">{left}</div><div class="cmp-cell">{right}</div>"#
        ));
    }
    format!(
        r#"<div class="cmp docs"><div class="cmp-h">Before (base)</div><div class="cmp-h">After (PR)</div>{rows}</div>"#
    )
}

/// Removes the characters used as change marks from text to render.
fn clean_marks(s: &str) -> String {
    s.replace([INS_OPEN, INS_CLOSE, DEL_OPEN, DEL_CLOSE], "\u{FFFD}")
}

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
                pr: None,
            },
            crates: vec!["c".into()],
            items: BTreeMap::new(),
            externals: BTreeMap::new(),
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
            Links::None,
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
    fn signature_diff() {
        let d = empty();
        let html = sig_diff_html(
            &d,
            "pub fn calc(input: &str) -> f64",
            "pub fn calc(input: &str, env: &Env) -> Result<f64>",
            &BTreeMap::new(),
            &[],
        );
        assert!(
            html.contains(r#"<ins class="sd" data-g="2">, env: &amp;Env</ins>"#),
            "{html}"
        );
        // Unchanged text is not marked.
        assert!(html.starts_with(r#"<span class="kw">pub</span>"#), "{html}");
        // Both halves of a replacement share a group, for linked hover.
        let replaced = sig_diff_html(&d, "fn f(x: u8)", "fn f(x: u16)", &BTreeMap::new(), &[]);
        assert!(
            replaced.contains(
                r#"<del class="sd" data-g="2">u8</del><ins class="sd" data-g="2">u16</ins>"#
            ),
            "{replaced}"
        );
    }

    #[test]
    fn structured_signature_diff() {
        let d = empty();
        let diff = SigDiff {
            spans: vec![
                DiffSpan::Same {
                    text: "pub enum E {\n    A,\n".into(),
                },
                DiffSpan::Removed {
                    text: "    B,\n".into(),
                    g: 1,
                },
                DiffSpan::Same {
                    text: "    C(".into(),
                },
                DiffSpan::Wrap {
                    before: "Option<".into(),
                    inner: vec![DiffSpan::Same { text: "u8".into() }],
                    after: ">".into(),
                    unwrap: false,
                    g: 2,
                },
                DiffSpan::Same {
                    text: "),\n".into(),
                },
                DiffSpan::Added {
                    text: "    D,\n".into(),
                    g: 3,
                },
                DiffSpan::Same { text: "}".into() },
            ],
            notes: vec![model::DiffNote {
                text: "variant `B` removed".into(),
                groups: vec![1],
            }],
        };
        assert_eq!(line_markers(&diff.spans), [' ', ' ', '-', '~', '+', ' ']);
        let html = structured_diff_html(&d, &diff, &BTreeMap::new(), &[]);
        assert!(html.contains(r#"<span class="wrap" data-g="2""#), "{html}");
        assert!(
            html.contains(r#"<ins class="edge">Option&lt;</ins>u8<ins class="edge">&gt;</ins>"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<li data-gs="1">variant <code>B</code> removed</li>"#),
            "{html}"
        );
        assert!(html.contains("sig-gutter"), "{html}");
    }

    #[test]
    fn method_links_by_position_only() {
        let d = empty();
        let mut sites = BTreeMap::new();
        sites.insert(
            "7:10".to_string(),
            Ref {
                id: None,
                path: "c::A::get".into(),
            },
        );
        // Line 7: `a.get()` (name at column 10) is linked; `b.get()` is not.
        let html = code_html(
            &d,
            "    let a.get() + b.get();",
            Links::Source {
                line: 7,
                refs: &sites,
            },
        );
        assert_eq!(html.matches("c::A::get").count(), 1, "{html}");
        assert!(
            html.contains(r#".<span class="ref ext " data-path="c::A::get">get</span>"#),
            "{html}"
        );
    }

    #[test]
    fn signature_links_only_at_recorded_offsets() {
        let d = empty();
        let mut refs = BTreeMap::new();
        refs.insert(
            "Op".to_string(),
            Ref {
                id: None,
                path: "c::Op".into(),
            },
        );
        // `Op(Op)`: only the type (offset 3) is a path.
        let html = code_html(
            &d,
            "Op(Op)",
            Links::Sig {
                refs: &refs,
                at: &[3],
                base: 0,
            },
        );
        assert_eq!(
            html,
            r#"Op(<span class="ref ext " data-path="c::Op">Op</span>)"#
        );
    }

    #[test]
    fn module_containment() {
        use crate::data::within;
        assert!(within("a::b", "a"));
        assert!(within("a", "a"));
        assert!(!within("ab", "a"));
        assert!(!within("a", "a::b"));
    }

    #[test]
    fn doc_blocks_keep_code_fences_whole() {
        let md = "Intro.\n\n```\nlet a = 1;\n\nlet b = 2;\n```\n\nSee [x].\n\n[x]: https://example.com\n";
        let (blocks, defs) = doc_blocks(md);
        assert_eq!(
            blocks,
            [
                "Intro.\n",
                "```\nlet a = 1;\n\nlet b = 2;\n```\n",
                "See [x].\n",
                "[x]: https://example.com\n"
            ]
        );
        assert_eq!(defs, "[x]: https://example.com\n");
    }

    #[test]
    fn docs_diff_marks_blocks() {
        let d = empty();
        let html = docs_diff_html(
            &d,
            "Same.\n\nOld text.\n\nGone.",
            "Same.\n\nNew text.\n\nGone.\n\nAdded, see [x].\n\n[x]: https://example.com",
            &BTreeMap::new(),
        );
        assert!(
            html.starts_with(r#"<div class="docs"><div class="dblk"><p>Same.</p>"#),
            "{html}"
        );
        assert!(
            html.contains(
                r#"<div class="mark m-chg"><div class="docs"><div class="dblk"><p>New text.</p>"#
            ),
            "{html}"
        );
        assert!(html.contains(r#"<details class="dblk-old"><summary>previous text</summary><div class="mark m-del"><div class="docs"><div class="dblk"><p>Old text.</p>"#), "{html}");
        // The link definition applies to the block that uses it, and the
        // two added blocks share one marker.
        assert!(
            html.contains(r#"<div class="mark m-add"><div class="docs"><div class="dblk"><p>Added, see <a href="https://example.com""#),
            "{html}"
        );
        assert!(
            html.ends_with(r#"<div class="docs"><div class="dblk"><code>[x]: https://example.com</code></div></div></div>"#),
            "{html}"
        );
        assert_eq!(html.matches("mark m-add").count(), 1);
    }

    #[test]
    fn word_diff_marks_changed_words_only_where_safe() {
        assert_eq!(
            word_diff_md(
                "Evaluates an expression.",
                "Evaluates an arithmetic expression."
            ),
            Some("Evaluates an \u{E000}arithmetic \u{E001}expression.".into())
        );
        // A rewritten phrase is one removal and one insertion.
        assert_eq!(
            word_diff_md(
                "Tokenizes `input` and creates a parser for it.",
                "Tokenizes `input` and creates a parser positioned at its first token."
            ),
            Some(
                "Tokenizes `input` and creates a parser \u{E002}for it\u{E003}\u{E000}positioned at its first token\u{E001}."
                    .into()
            )
        );
        // Mostly rewritten: block level instead.
        assert_eq!(word_diff_md("One two three.", "Four five six."), None);
        // Link text, link targets and Markdown syntax are left alone.
        assert_eq!(word_diff_md("See [`Foo`] here.", "See [`Bar`] here."), None);
        assert_eq!(
            word_diff_md("See [x](a.html) now.", "See [x](b.html) now."),
            None
        );
        assert_eq!(word_diff_md("A *b* c d e.", "A **b** c d e."), None);
        // A list marker at the start of a line is syntax.
        assert_eq!(
            word_diff_md("- a b c\n- d e f", "- a b c\n- d e g"),
            Some("- a b c\n- d e \u{E002}f\u{E003}\u{E000}g\u{E001}".into())
        );
        assert_eq!(word_diff_md("a b c d\ne", "a b c d\n- e"), None);
    }

    #[test]
    fn word_diff_renders_as_ins_and_del() {
        let d = empty();
        let html = docs_diff_html(
            &d,
            "Evaluates an expression.",
            "Evaluates a parsed expression.",
            &BTreeMap::new(),
        );
        assert_eq!(
            html,
            r#"<div class="mark m-chg"><div class="docs"><div class="dblk"><p>Evaluates <del class="wd">an</del><ins class="wd">a parsed</ins> expression.</p>
</div></div></div>"#
        );
    }

    #[test]
    fn source_diff_follows_the_settings() {
        use model::{Item, SourceLine, Status};
        let d = empty();
        let mut it = Item {
            status: Status::Changed,
            ..Default::default()
        };
        it.changes.body = true;
        it.info.kind = model::Kind::Fn;
        it.info.line = 10;
        it.old_line = Some(4);
        it.source = vec![
            SourceLine(' ', "fn f() {".into()),
            SourceLine('-', "  a();".into()),
            SourceLine('+', "    a();".into()),
            SourceLine('-', "    b();".into()),
            SourceLine('+', "    c();".into()),
            SourceLine(' ', "}".into()),
        ];
        let html = d.source_html(&it, true, false);
        assert!(
            html.contains(">+2</span> <span class=\"d-del\">−2<"),
            "{html}"
        );
        crate::settings::set(crate::settings::Settings {
            ignore_ws: true,
            split: true,
            ..Default::default()
        });
        let html = d.source_html(&it, true, false);
        crate::settings::set(Default::default());
        // Only `b()` → `c()` counts, shown in one row: old line 6, new line 12.
        assert!(
            html.contains(">+1</span> <span class=\"d-del\">−1<"),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="no">6</span><span class="ln del" data-t="-">"#),
            "{html}"
        );
        assert!(
            html.contains(r#"<span class="no">12</span><span class="ln add" data-t="+">"#),
            "{html}"
        );
    }

    #[test]
    fn private_fields_are_expandable_from_outside() {
        use model::{Item, Status};
        let mut out = empty().out;
        let mut it = Item {
            status: Status::Unchanged,
            ..Default::default()
        };
        it.info.id = "c::m::S".into();
        it.info.kind = model::Kind::Struct;
        it.info.module = "c::m".into();
        it.info.sig = "pub struct S {\n    secret: u8,\n}".into();
        it.info.sig_public = Some("pub struct S {\n    /* private fields */\n}".into());
        out.items.insert(it.info.id.clone(), it);
        let d = Data::new(out);
        let it = d.item("c::m::S").unwrap();
        let outside = d.sig_html(it, "c");
        assert!(outside.contains("private fields */"), "{outside}");
        assert!(
            outside.contains(r#"<details class="private-def">"#),
            "{outside}"
        );
        assert!(outside.contains("secret"), "{outside}");
        // Inside the module everything is shown already.
        let inside = d.sig_html(it, "c::m");
        assert!(!inside.contains("private-def"), "{inside}");
    }

    #[test]
    fn neighboring_markers_are_joined() {
        let seg = |bar, html: &str| Seg::new(bar, html.to_string());
        assert_eq!(
            marked_html([
                seg("m-add", "a"),
                seg("m-add", "b"),
                seg("", ""),
                seg("m-add", "c"),
                seg("", "d"),
                seg("m-chg", "e"),
                seg("m-del", "f"),
            ]),
            r#"<div class="mark m-add">abc</div>d<div class="mark m-chg">e</div><div class="mark m-del">f</div>"#
        );
    }

    #[test]
    fn an_added_item_has_one_marker() {
        use model::{Item, SourceLine, Status};
        let mut out = empty().out;
        let mut it = Item {
            status: Status::Added,
            ..Default::default()
        };
        it.info.id = "c::f".into();
        it.info.kind = model::Kind::Fn;
        it.info.module = "c".into();
        it.info.sig = "pub fn f()".into();
        it.info.docs = "Does things.\n\nIn detail.".into();
        it.source = vec![SourceLine('+', "pub fn f() {}".into())];
        out.items.insert(it.info.id.clone(), it);
        let d = Data::new(out);
        let it = d.item("c::f").unwrap();
        let html = d.item_body_html(it, "c", true, d.source_seg(it, false, true));
        assert_eq!(html.matches("class=\"mark").count(), 1, "{html}");
        assert!(html.starts_with(r#"<div class="mark m-add">"#), "{html}");
    }

    #[test]
    fn small_body_changes_are_shown_directly() {
        use model::{Item, SourceLine, Status};
        let d = empty();
        let mut it = Item {
            status: Status::Changed,
            ..Default::default()
        };
        it.changes.body = true;
        it.info.kind = model::Kind::Fn;
        it.info.line = 1;
        it.old_line = Some(1);
        let mut lines: Vec<SourceLine> =
            (0..12).map(|i| SourceLine(' ', format!("l{i};"))).collect();
        lines.insert(6, SourceLine('-', "old();".into()));
        lines.insert(7, SourceLine('+', "new();".into()));
        it.source = lines.clone();
        let html = d.source_html(&it, false, true);
        // The hunk with two lines of context comes first, outside the
        // collapsed whole source; skipped lines are marked.
        let (hunk, whole) = html.split_once("<details").unwrap();
        assert!(hunk.contains("old();") && hunk.contains("new();"), "{hunk}");
        assert!(hunk.contains("l4;") && hunk.contains("l7;"), "{hunk}");
        assert!(!hunk.contains("l2;") && !hunk.contains("l9;"), "{hunk}");
        assert_eq!(hunk.matches("⋯").count(), 2, "{hunk}");
        assert!(whole.contains("l0;"), "{whole}");
        // Five changed lines or more: collapsed as before.
        for k in 0..3 {
            lines.insert(1, SourceLine('+', format!("more{k}();")));
        }
        it.source = lines;
        let html = d.source_html(&it, false, true);
        assert!(
            html.starts_with("<div class=\"mark m-chg\"><details"),
            "{html}"
        );
    }
}
