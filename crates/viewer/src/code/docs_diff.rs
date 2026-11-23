//! Word diffs of doc comments and Markdown files.

use super::{
    esc,
    markdown::{doc_blocks, docs_html, is_link_def, render_markdown},
};
use crate::data::Data;
use model::Ref;
use std::collections::BTreeMap;

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
    diff_segs(d, old, new, refs, true)
}

/// [`docs_diff_segs`] for a Markdown file.
pub fn markdown_diff_segs(d: &Data, old: &str, new: &str) -> Vec<Seg> {
    diff_segs(d, old, new, &BTreeMap::new(), false)
}

fn diff_segs(
    d: &Data,
    old: &str,
    new: &str,
    refs: &BTreeMap<String, Ref>,
    doc_comment: bool,
) -> Vec<Seg> {
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
                render_markdown(d, &format!("{text}\n{defs}"), refs, doc_comment)
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
                            marked_docs_html(d, &format!("{md}\n{b_defs}"), refs, doc_comment)
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
pub(super) fn word_diff_md(old: &str, new: &str) -> Option<String> {
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
        // `target`: parenthesis depth inside a link destination, which
        // may itself contain balanced parentheses.
        let (mut bracket, mut target, mut angle) = (0i32, 0i32, false);
        let mut line_start = true;
        let mut prev = "";
        for t in toks {
            let syntax = t.chars().all(|c| "*_[]()<>#!|~`\\-+=:".contains(c))
                || t.starts_with('_')
                || t.ends_with('_');
            let ws = t.chars().all(char::is_whitespace);
            let inside = bracket > 0 || target > 0 || angle;
            out.push(!syntax && !inside && !(line_start && !ws));
            match *t {
                "[" => bracket += 1,
                "]" => bracket -= 1,
                "(" if prev == "]" || target > 0 => target += 1,
                ")" if target > 0 => target -= 1,
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
fn marked_docs_html(d: &Data, md: &str, refs: &BTreeMap<String, Ref>, doc_comment: bool) -> String {
    render_markdown(d, md, refs, doc_comment)
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
