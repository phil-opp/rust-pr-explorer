//! Signature diffs: old and new code with the changed tokens marked.

use super::{
    esc,
    highlight::{Links, code_html},
};
use crate::data::Data;
use model::{DiffSpan, Ref, SigDiff};
use std::collections::BTreeMap;

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

/// Text of a diff note: backticks become code.
pub fn note_html(text: &str) -> String {
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

/// A structured signature diff: the signature and one note per change. Hovering a change
/// or a note highlights everything that belongs to it.
pub fn structured_diff_html(
    d: &Data,
    diff: &SigDiff,
    refs: &BTreeMap<String, Ref>,
    at: &[u32],
) -> String {
    let code = spans_html(d, &diff.spans, refs, at, &mut 0);
    // No gutter: added parts are highlighted, removed ones struck out.
    let sig = format!(r#"<pre class="sig sig-diff">{code}</pre>"#);
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
            format!(
                r#"<li data-gs="{groups}">{}{}</li>"#,
                note_html(&n.text),
                n.docs
                    .as_deref()
                    .map(|docs| member_docs_html(d, docs))
                    .unwrap_or_default()
            )
        })
        .collect();
    format!(r#"<div class="sig-block">{sig}<ul class="sig-notes">{notes}</ul></div>"#)
}

/// A field's or variant's doc comment under its note: the first paragraph,
/// and the rest behind an expander.
fn member_docs_html(d: &Data, docs: &str) -> String {
    let (first, rest) = split_first_paragraph(docs);
    let more = if rest.is_empty() {
        String::new()
    } else {
        format!(
            r#"<details class="note-more"><summary>More</summary>{}</details>"#,
            crate::code::docs_html(d, rest, &BTreeMap::new())
        )
    };
    format!(
        r#"<div class="docs note-docs">{}{more}</div>"#,
        crate::code::docs_html(d, first, &BTreeMap::new())
    )
}

/// Markdown split after its first paragraph (at the first blank line
/// outside a code block): the paragraph, and the rest (possibly empty).
pub fn split_first_paragraph(md: &str) -> (&str, &str) {
    let mut fence = false;
    let mut seen_text = false;
    let mut at = 0;
    for line in md.split_inclusive('\n') {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
        }
        if t.is_empty() && !fence && seen_text {
            let rest = md[at..].trim();
            return (md[..at].trim_end(), rest);
        }
        seen_text |= !t.is_empty();
        at += line.len();
    }
    (md.trim_end(), "")
}
