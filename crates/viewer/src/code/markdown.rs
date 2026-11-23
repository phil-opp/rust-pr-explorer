//! Doc comment and Markdown rendering, with intra-doc links.

use super::{
    esc,
    highlight::{Links, code_html, ref_html},
};
use crate::data::Data;
use model::Ref;
use pulldown_cmark::{BrokenLink, CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};
use std::collections::BTreeMap;

/// Renders doc comment markdown. Intra-doc links become cross links; raw
/// HTML from the docs is shown as text, never interpreted.
pub fn docs_html(d: &Data, md: &str, refs: &BTreeMap<String, Ref>) -> String {
    render_markdown(d, md, refs, true)
}

/// Renders a Markdown file (not a doc comment): code blocks without a
/// language are plain text, and no lines of them are hidden.
pub fn markdown_file_html(d: &Data, md: &str) -> String {
    render_markdown(d, md, &BTreeMap::new(), false)
}

pub(super) fn render_markdown(
    d: &Data,
    md: &str,
    refs: &BTreeMap<String, Ref>,
    doc_comment: bool,
) -> String {
    if md.trim().is_empty() {
        return String::new();
    }
    // `[text]` without a link definition: an intra-doc link if it resolves;
    // otherwise shown as written (`arr[0]`, `[u8]`).
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
                            // rustdoc hides `# ` lines of Rust examples.
                            !(doc_comment
                                && *rust
                                && (l.trim() == "#" || l.trim_start().starts_with("# ")))
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
                    CodeBlockKind::Indented => doc_comment,
                    CodeBlockKind::Fenced(lang) => {
                        (doc_comment && lang.is_empty())
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
                } else if let Some(reference) = dest_url.strip_prefix("rpe-ref:") {
                    if reference.starts_with('`') {
                        // An intra-doc link we can't resolve: its code.
                        link_stack.push(None);
                    } else {
                        // Not a link at all: the brackets as written.
                        events.push(Event::Text("[".into()));
                        link_stack.push(Some("]"));
                    }
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
pub(super) fn is_link_def(line: &str) -> bool {
    let t = line.trim_start();
    line.len() - t.len() <= 3
        && t.starts_with('[')
        && t.find("]:").is_some_and(|i| !t[1..i].contains(['[', ']']))
}

/// Splits Markdown into blocks separated by blank lines (not inside code
/// fences), plus its link reference definitions, which every block needs.
pub(super) fn doc_blocks(md: &str) -> (Vec<String>, String) {
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
