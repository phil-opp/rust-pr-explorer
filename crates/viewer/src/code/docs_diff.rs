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
    let (a, a_defs) = doc_blocks(old);
    let (b, b_defs) = doc_blocks(new);
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
    // Changed blocks: their changed words marked in place, or, if that
    // can't be shown well, the new blocks plus the old ones on demand.
    let changed = |out: &mut Vec<Seg>, old: &[String], new: &[String]| {
        // Both sides link the same way, so that only real changes differ.
        let render = |blocks: &[String], defs: &str| -> String {
            blocks
                .iter()
                .map(|t| block(t, defs, refs, "m-chg").html)
                .collect()
        };
        if let Some(html) = word_diff_html(&render(old, &a_defs), &render(new, &b_defs)) {
            out.push(Seg::new("m-chg", html));
            return;
        }
        for t in new {
            out.push(block(t, &b_defs, refs, "m-chg"));
        }
        let previous = marked_html(old.iter().map(|t| block(t, &a_defs, &no_refs, "m-del")));
        out.push(Seg::new(
            "",
            format!(
                r#"<details class="dblk-old"><summary>previous text</summary>{previous}</details>"#
            ),
        ));
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
            DiffOp::Replace { .. } if old_range.len() == new_range.len() => {
                // Paragraph by paragraph.
                for (i, j) in old_range.zip(new_range) {
                    changed(&mut out, &a[i..i + 1], &b[j..j + 1]);
                }
            }
            DiffOp::Replace { .. } => changed(&mut out, &a[old_range], &b[new_range]),
        }
    }
    out
}

/// [`docs_diff_segs`], rendered on their own.
#[cfg(test)]
pub fn docs_diff_html(d: &Data, old: &str, new: &str, refs: &BTreeMap<String, Ref>) -> String {
    marked_html(docs_diff_segs(d, old, new, refs))
}

/// Splits rendered HTML into tags, character references, words, runs of
/// whitespace and single other characters.
fn html_tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let len = match c {
            '<' => rest.find('>').map_or(1, |i| i + 1),
            // Bytes, not a string slice: the 12th byte can be inside a
            // multibyte character.
            '&' => rest
                .bytes()
                .take(12)
                .position(|b| b == b';')
                .filter(|&i| rest[1..i].chars().all(|c| c.is_alphanumeric() || c == '#'))
                .map_or(1, |i| i + 1),
            c if c.is_whitespace() => rest
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len()),
            c if c.is_alphanumeric() || c == '_' => rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len()),
            c => c.len_utf8(),
        };
        out.push(&rest[..len]);
        rest = &rest[len..];
    }
    out
}

fn is_tag(t: &str) -> bool {
    t.starts_with('<') && t.len() > 1
}

/// Visible text other than whitespace.
fn is_text(t: &str) -> bool {
    !is_tag(t) && !t.trim().is_empty()
}

/// A paragraph's changes word by word: the new HTML with removed text in
/// `<del>` and added text in `<ins>`. It keeps the new version's structure
/// (removed text only brings along inline tags that open and close within
/// it), so the result is valid wherever the new HTML is. `None` for code
/// blocks, and for text rewritten so much that marking words won't help.
pub(super) fn word_diff_html(old: &str, new: &str) -> Option<String> {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    if old.contains("<pre") || new.contains("<pre") {
        return None;
    }
    let (a, b) = (html_tokens(old), html_tokens(new));
    // A line break in a paragraph is just a space.
    let norm = |toks: &[&str]| -> Vec<String> {
        toks.iter()
            .map(|t| {
                if t.trim().is_empty() {
                    " ".to_string()
                } else {
                    t.to_string()
                }
            })
            .collect()
    };
    let (ka, kb) = (norm(&a), norm(&b));
    let ops = capture_diff_slices(Algorithm::Myers, &ka, &kb);
    let words = |toks: &[&str]| toks.iter().filter(|t| is_text(t)).count();
    let same: usize = ops
        .iter()
        .filter_map(|op| match op {
            DiffOp::Equal { new_index, len, .. } => Some(words(&b[*new_index..new_index + len])),
            _ => None,
        })
        .sum();
    // At least half of the shorter version must remain.
    if same * 2 < words(&a).min(words(&b)) {
        return None;
    }
    // Segments: unchanged (old range, new range), or changed. Changes
    // separated only by whitespace are merged, so that a rewritten phrase
    // reads as one removal and one insertion.
    type R = std::ops::Range<usize>;
    let mut segs: Vec<(bool, R, R)> = Vec::new();
    for op in ops {
        let (o, n) = (op.old_range(), op.new_range());
        if let DiffOp::Equal { .. } = op {
            segs.push((false, o, n));
            continue;
        }
        let ws_between = match segs.as_slice() {
            [.., (true, ..), (false, _, r)] => {
                !b[r.clone()].iter().any(|t| is_text(t) || is_tag(t))
            }
            _ => false,
        };
        if ws_between {
            segs.pop();
        }
        match segs.last_mut() {
            Some((true, po, pn)) => {
                po.end = o.end;
                pn.end = n.end;
            }
            _ => segs.push((true, o, n)),
        }
    }
    // Short unchanged bits between changes ("of", "that", a comma) make a
    // rewritten sentence hard to read: they join the changes around them
    // when they are no longer than either.
    let chars = |toks: &[&str]| -> usize {
        toks.iter()
            .filter(|t| is_text(t))
            .map(|t| t.chars().count())
            .sum()
    };
    let size = |o: &R, n: &R| chars(&a[o.clone()]).max(chars(&b[n.clone()]));
    let mut k = 1;
    while k + 1 < segs.len() {
        let (chg, ref o, ref n) = segs[k];
        let (before, after) = (&segs[k - 1], &segs[k + 1]);
        if !chg
            && before.0
            && after.0
            && size(o, n) <= size(&before.1, &before.2).min(size(&after.1, &after.2))
        {
            let (o_end, n_end) = (after.1.end, after.2.end);
            segs[k - 1].1.end = o_end;
            segs[k - 1].2.end = n_end;
            segs.drain(k..k + 2);
            k = k.saturating_sub(1).max(1);
        } else {
            k += 1;
        }
    }
    // A change of tags alone (a link target, emphasis) would be invisible:
    // extend it to the next word, which then shows as changed.
    let mut k = 0;
    while k < segs.len() {
        let (chg, o, n) = segs[k].clone();
        let tags_only = chg
            && !a[o.clone()].iter().chain(&b[n.clone()]).any(|t| is_text(t))
            && a[o.clone()].iter().chain(&b[n.clone()]).any(|t| is_tag(t));
        if !tags_only {
            k += 1;
            continue;
        }
        // Swallow what follows up to and including the next word.
        loop {
            let (next_chg, so, sn) = segs.get(k + 1).cloned()?;
            let word = b[sn.clone()].iter().position(|t| is_text(t));
            match word {
                Some(w) if !next_chg && w + 1 < sn.len() => {
                    let cut = w + 1;
                    segs[k].1.end = so.start + cut;
                    segs[k].2.end = sn.start + cut;
                    segs[k + 1] = (false, so.start + cut..so.end, sn.start + cut..sn.end);
                    break;
                }
                _ => {
                    segs[k].1.end = so.end;
                    segs[k].2.end = sn.end;
                    segs.remove(k + 1);
                    let has_word = word.is_some() || a[so].iter().any(|t| is_text(t));
                    if has_word {
                        break;
                    }
                }
            }
        }
        // A change right after it (closing tags, say) joins it.
        if let Some((true, no, nn)) = segs.get(k + 1).cloned() {
            segs[k].1.end = no.end;
            segs[k].2.end = nn.end;
            segs.remove(k + 1);
        }
        k += 1;
    }
    let mut out = String::new();
    for (chg, o, n) in segs {
        if !chg {
            out.push_str(&b[n].concat());
            continue;
        }
        let (old, new) = (&a[o], &b[n]);
        if old.iter().any(|t| is_text(t)) {
            let keep_tags = inline_balanced(old);
            let text: String = old
                .iter()
                .filter(|t| keep_tags || !is_tag(t))
                .copied()
                .collect();
            // Spaces before it stay outside the mark, as for insertions.
            out.push_str(&text[..text.len() - text.trim_start().len()]);
            out.push_str(&format!(r#"<del class="wd">{}</del>"#, text.trim()));
        }
        // The new tokens in order, their text marked: as a whole if its
        // tags are inline ones, else between the tags.
        if new.iter().any(|t| is_text(t)) && inline_balanced(new) {
            let run = new.concat();
            let inner = run.trim();
            let lead = &run[..run.len() - run.trim_start().len()];
            let trail = &run[run.trim_end().len()..];
            out.push_str(&format!(r#"{lead}<ins class="wd">{inner}</ins>{trail}"#));
            continue;
        }
        let mut k = 0;
        while k < new.len() {
            if is_tag(new[k]) {
                out.push_str(new[k]);
                k += 1;
                continue;
            }
            let end = new[k..]
                .iter()
                .position(|t| is_tag(t))
                .map_or(new.len(), |e| k + e);
            let run = new[k..end].concat();
            if new[k..end].iter().any(|t| is_text(t)) {
                // Spaces around the words stay outside the mark.
                let inner = run.trim();
                let lead = &run[..run.len() - run.trim_start().len()];
                let trail = &run[run.trim_end().len()..];
                out.push_str(&format!(r#"{lead}<ins class="wd">{inner}</ins>{trail}"#));
            } else {
                out.push_str(&run);
            }
            k = end;
        }
    }
    Some(out)
}

/// Whether the tags among some tokens are inline ones that each close
/// again among them, so that they can be kept in removed text.
fn inline_balanced(toks: &[&str]) -> bool {
    let mut stack = Vec::new();
    for t in toks.iter().filter(|t| is_tag(t)) {
        let (close, body) = match t.strip_prefix("</") {
            Some(b) => (true, b),
            None => (false, &t[1..]),
        };
        let name: String = body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !matches!(
            name.as_str(),
            "code" | "em" | "strong" | "a" | "span" | "del"
        ) {
            return false;
        }
        if close {
            if stack.pop() != Some(name) {
                return false;
            }
        } else {
            stack.push(name);
        }
    }
    stack.is_empty()
}

/// Docs before and after, side by side, with changed paragraphs aligned.
pub fn docs_compare_html(d: &Data, it: &model::Item) -> String {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    let old = it.old_docs.as_deref().unwrap_or("");
    let new = &it.info.docs;
    let (a, a_defs) = doc_blocks(old);
    let (b, b_defs) = doc_blocks(new);
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

/// Two versions of a changed line of highlighted code, with the text that
/// differs between them wrapped in `<span class="wc">`. Only the text is
/// compared, not the highlighting or links around it, and a change of
/// whitespace alone isn't marked. `None` if less than half of the shorter
/// line remains, where marking words wouldn't help.
pub fn mark_changed_tokens(old: &str, new: &str) -> Option<(String, String)> {
    use similar::{Algorithm, DiffOp, capture_diff_slices};
    let (a, b) = (html_tokens(old), html_tokens(new));
    // Positions of the text tokens, and what they are compared by.
    let text = |toks: &[&str]| -> (Vec<usize>, Vec<String>) {
        toks.iter()
            .enumerate()
            .filter(|(_, t)| !is_tag(t))
            .map(|(i, t)| {
                let key = if t.trim().is_empty() { " " } else { t };
                (i, key.to_string())
            })
            .unzip()
    };
    let ((ia, ka), (ib, kb)) = (text(&a), text(&b));
    let mut changed_a = vec![false; a.len()];
    let mut changed_b = vec![false; b.len()];
    let mut same = 0;
    for op in capture_diff_slices(Algorithm::Myers, &ka, &kb) {
        if let DiffOp::Equal { new_index, len, .. } = op {
            same += ib[new_index..new_index + len]
                .iter()
                .filter(|&&i| is_text(b[i]))
                .count();
            continue;
        }
        for &i in &ia[op.old_range()] {
            changed_a[i] = is_text(a[i]);
        }
        for &i in &ib[op.new_range()] {
            changed_b[i] = is_text(b[i]);
        }
    }
    let words = |toks: &[&str]| toks.iter().filter(|t| is_text(t)).count();
    if same * 2 < words(&a).min(words(&b)) {
        return None;
    }
    // Marks runs of changed text; whitespace between two changed tokens
    // of a run belongs to it.
    let render = |toks: &[&str], changed: &[bool]| -> String {
        let mut out = String::new();
        let mut open = false;
        for (k, t) in toks.iter().enumerate() {
            let inner_ws = !is_tag(t)
                && !is_text(t)
                && open
                && toks[k + 1..]
                    .iter()
                    .zip(&changed[k + 1..])
                    .find(|(t, _)| is_tag(t) || is_text(t))
                    .is_some_and(|(t, &c)| !is_tag(t) && c);
            let mark = changed[k] || inner_ws;
            if open && !mark {
                out.push_str("</span>");
                open = false;
            }
            if mark && !open {
                out.push_str(r#"<span class="wc">"#);
                open = true;
            }
            out.push_str(t);
        }
        if open {
            out.push_str("</span>");
        }
        out
    };
    Some((render(&a, &changed_a), render(&b, &changed_b)))
}

/// A line's highlighted HTML with the characters `added` (ranges of its
/// text, in chars) marked as added, and `removed` text shown struck out at
/// the given columns. For reformatted lines that also gained or lost a few
/// tokens (see `model::Reformatted`). `re_added` and `re_removed` are the
/// same for path segments of paths that still name the same item, which
/// are marked as no change.
pub fn mark_spans(
    html: &str,
    added: &[(usize, usize)],
    removed: &[(usize, &str)],
    re_added: &[(usize, usize)],
    re_removed: &[(usize, &str)],
) -> String {
    let spans = with_class(added, ADDED).chain(with_class(re_added, REQUALIFIED));
    let inserts = removed
        .iter()
        .map(|&(at, text)| (at, text, REMOVED))
        .chain(re_removed.iter().map(|&(at, text)| (at, text, UNQUALIFIED)));
    mark_columns(html, &spans.collect::<Vec<_>>(), inserts.collect())
}

/// A line's highlighted HTML with the characters `removed` (ranges of its
/// text, in chars) marked as removed, and `unqualified` as path segments
/// removed from a path that still names the same item.
pub fn mark_removed_spans(
    html: &str,
    removed: &[(usize, usize)],
    unqualified: &[(usize, usize)],
) -> String {
    let spans = with_class(removed, REMOVED).chain(with_class(unqualified, UNQUALIFIED));
    mark_columns(html, &spans.collect::<Vec<_>>(), Vec::new())
}

/// The attributes of marked tokens: added, removed, and path segments
/// added to or removed from a path that names the same item.
const ADDED: &str = r#"class="wc ti""#;
const REMOVED: &str = r#"class="wc td""#;
const REQUALIFIED: &str = r#"class="wc tq" title="The path names the same item as before""#;
const UNQUALIFIED: &str = r#"class="wc tq td" title="The path names the same item as before""#;

fn with_class<'a>(
    spans: &'a [(usize, usize)],
    attrs: &'static str,
) -> impl Iterator<Item = (usize, usize, &'static str)> + 'a {
    spans.iter().map(move |&(s, e)| (s, e, attrs))
}

/// Marks the characters of `spans` (ranges of the line's text, in chars,
/// with the attributes of their mark) and puts the struck-out text of
/// `inserts` (with the attributes of its mark) before the given columns.
fn mark_columns(
    html: &str,
    spans: &[(usize, usize, &str)],
    mut inserts: Vec<(usize, &str, &str)>,
) -> String {
    let marked = |col: usize| {
        spans
            .iter()
            .find(|&&(s, e, _)| s <= col && col < e)
            .map(|&(.., attrs)| attrs)
    };
    inserts.sort_by_key(|&(at, ..)| at);
    let mut inserts = inserts.into_iter().peekable();
    let mut out = String::new();
    // The attributes of the open mark, if any.
    let mut open: Option<&str> = None;
    // Closes the mark, then adds the removed text that goes before `col`:
    // what separates it from the token before, and the text struck out.
    let mut insert = |out: &mut String, open: &mut Option<&str>, col: usize| {
        while let Some((_, text, attrs)) = inserts.next_if(|&(at, ..)| at <= col) {
            if open.take().is_some() {
                out.push_str("</span>");
            }
            let body = text.trim_start();
            out.push_str(&super::esc(&text[..text.len() - body.len()]));
            out.push_str(&format!("<span {attrs}>"));
            out.push_str(&super::esc(body));
            out.push_str("</span>");
        }
    };
    let mut col = 0;
    for t in html_tokens(html) {
        if is_tag(t) {
            if open.take().is_some() {
                out.push_str("</span>");
            }
            out.push_str(t);
            continue;
        }
        // An entity is one character; other text goes char by char.
        let pieces: Vec<&str> = if t.starts_with('&') && t.len() > 1 {
            vec![t]
        } else {
            t.char_indices()
                .map(|(i, c)| &t[i..i + c.len_utf8()])
                .collect()
        };
        for p in pieces {
            insert(&mut out, &mut open, col);
            let m = marked(col);
            if open.is_some() && open != m {
                out.push_str("</span>");
                open = None;
            }
            if let Some(attrs) = m
                && open.is_none()
            {
                out.push_str(&format!("<span {attrs}>"));
                open = Some(attrs);
            }
            out.push_str(p);
            col += 1;
        }
    }
    insert(&mut out, &mut open, usize::MAX);
    if open.is_some() {
        out.push_str("</span>");
    }
    out
}
