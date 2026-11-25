//! `unsafe` code that the PR adds, changes or removes (see
//! `model::UnsafeChange`): a tag wherever an item is listed, a line per
//! change on its row and card, a stripe on its lines in source diffs, and
//! a summary at the top of the overview.

use crate::code::esc;
use crate::data::Data;
use model::{Item, PartChange, Status, UnsafeChange};

/// Whether any `unsafe` code was added or changed, not only removed.
fn adds(changes: &[UnsafeChange]) -> bool {
    changes.iter().any(|c| c.status != Status::Removed)
}

/// One change as a sentence: "new `unsafe impl Send for Buf`".
fn sentence(c: &UnsafeChange) -> String {
    let what = match c.kind {
        model::UnsafeKind::Block => "`unsafe` block".to_string(),
        _ => format!("`{}`", c.what),
    };
    let line = match (c.lines, c.old_lines) {
        (Some((a, _)), _) | (None, Some((a, _))) => format!(", line {a}"),
        _ => String::new(),
    };
    match c.status {
        Status::Added => format!("new {what}{line}"),
        Status::Removed => format!("{what} removed{line}"),
        _ => format!("{what} changed{line}"),
    }
}

/// The tooltip of a tag: every change, one per line.
fn title(changes: &[UnsafeChange]) -> String {
    changes
        .iter()
        .map(|c| sentence(c).replace('`', ""))
        .collect::<Vec<_>>()
        .join("\n")
}

fn tag_html(changes: &[UnsafeChange]) -> String {
    if changes.is_empty() {
        return String::new();
    }
    if adds(changes) {
        format!(
            r#"<span class="unsafe-tag" title="Adds or changes unsafe code:&#10;{}">⚠ unsafe</span>"#,
            esc(&title(changes))
        )
    } else {
        format!(
            r#"<span class="unsafe-tag gone" title="Removes unsafe code:&#10;{}">unsafe removed</span>"#,
            esc(&title(changes))
        )
    }
}

/// The tag of an item that adds, changes or removes `unsafe` code: "⚠
/// unsafe", or a quieter "unsafe removed". Empty for other items.
pub fn tag(it: &Item) -> String {
    tag_html(&it.unsafe_code)
}

/// [`tag`] for a part of a file (Rust code outside items).
pub fn part_tag(p: &PartChange) -> String {
    tag_html(&p.unsafe_code)
}

/// What changed, one line each, for an item's row and card.
pub fn lines_html(changes: &[UnsafeChange]) -> String {
    if changes.is_empty() {
        return String::new();
    }
    let lines: String = changes
        .iter()
        .map(|c| {
            format!(
                r#"<li class="{}">{}</li>"#,
                if c.status == Status::Removed {
                    "gone"
                } else {
                    ""
                },
                crate::code::note_html(&sentence(c))
            )
        })
        .collect();
    format!(r#"<ul class="unsafe-why">{lines}</ul>"#)
}

/// Whether a line of an item's source diff is inside `unsafe` code that
/// changed: `old` for a line of the base's version. Lines are numbered as
/// in their file.
pub fn marks_line(changes: &[UnsafeChange], old: bool, line: usize) -> bool {
    let line = line as u32;
    changes.iter().any(|c| {
        let range = if old { c.old_lines } else { c.lines };
        range.is_some_and(|(a, b)| (a..=b).contains(&line))
    })
}

/// The note on the first line of a changed piece of `unsafe` code in a
/// source diff: "⚠ new unsafe block". Empty on other lines.
pub fn note_at(changes: &[UnsafeChange], old: bool, line: usize) -> String {
    let line = line as u32;
    let Some(c) = changes.iter().find(|c| {
        let range = if old { c.old_lines } else { c.lines };
        // A changed one is noted on its new lines only.
        range.is_some_and(|(a, _)| a == line) && (!old || c.status == Status::Removed)
    }) else {
        return String::new();
    };
    // Short, since the line's code comes first: the kind, not the name.
    let kind = c.kind.label();
    let text = match c.status {
        Status::Added => format!("new {kind}"),
        Status::Removed => format!("{kind} removed"),
        _ => format!("{kind} changed"),
    };
    format!(r#"<span class="uns-note">⚠ {}</span>"#, esc(&text))
}

/// The overview's summary of the `unsafe` code that the PR adds, changes
/// or removes: one row per item or file part, with what changed. Shown
/// above everything else, since it is what a reviewer should look at
/// first. Empty if the PR doesn't touch `unsafe` code.
pub fn overview_html(d: &Data) -> String {
    let mut items: Vec<&Item> = d.items().filter(|it| !it.unsafe_code.is_empty()).collect();
    items.sort_by(|a, b| {
        adds(&b.unsafe_code)
            .cmp(&adds(&a.unsafe_code))
            .then_with(|| d.reading_order(a, b))
    });
    let mut rows: Vec<String> = items
        .iter()
        .map(|it| {
            format!(
                r#"<div class="unsafe-row{}"><div class="unsafe-name"><span class="kind">{}</span><a class="ref" data-id="{}" data-kind="{}" data-path="{}"{}><code>{}</code></a> <span class="dim">in {}</span>{}</div>{}</div>"#,
                if adds(&it.unsafe_code) { "" } else { " gone" },
                it.info.kind.label(),
                esc(&it.info.id),
                it.info.kind.css(),
                esc(&it.info.id),
                crate::nav::item_href(&it.info.id),
                crate::review::item_tag(&it.info.id, &d.display_name(it)),
                d.mod_label_html(d.mod_of(it)),
                crate::data::test_tag_of(it),
                lines_html(&it.unsafe_code),
            )
        })
        .collect();
    // Code outside items: `extern` blocks, macro calls.
    for f in &d.out.files {
        for p in f.parts.iter().filter(|p| !p.unsafe_code.is_empty()) {
            rows.push(format!(
                r#"<div class="unsafe-row{}"><div class="unsafe-name"><span class="kind">{}</span><a data-goto-file="{}"{}><code>{}</code></a> <span class="dim">in <code>{}</code></span></div>{}</div>"#,
                if adds(&p.unsafe_code) { "" } else { " gone" },
                esc(&p.kind),
                esc(&f.path),
                crate::nav::file_href(&f.path),
                esc(&p.name),
                esc(&f.path),
                lines_html(&p.unsafe_code),
            ));
        }
    }
    if rows.is_empty() {
        return String::new();
    }
    let n = rows.len();
    let adding = items.iter().filter(|it| adds(&it.unsafe_code)).count()
        + d.out
            .files
            .iter()
            .flat_map(|f| &f.parts)
            .filter(|p| adds(&p.unsafe_code))
            .count();
    let summary = match (adding, n - adding) {
        (0, gone) => format!("removed in {gone} place{}", plural(gone)),
        (a, 0) => format!("added or changed in {a} place{}", plural(a)),
        (a, gone) => format!(
            "added or changed in {a} place{}, removed in {gone}",
            plural(a)
        ),
    };
    format!(
        r#"<section class="unsafe-box{}"><h2><span class="unsafe-sign">⚠</span> Unsafe code <span class="h-note">{summary}</span> {}</h2>{}</section>"#,
        if adding == 0 { " gone" } else { "" },
        crate::pages::help_html(
            "Items and code that add, change or remove unsafe blocks, functions, impls, traits, extern blocks or unsafe attributes (#[no_mangle], #[unsafe(…)]). The compiler doesn't check this code's safety rules: check each one by hand. Safe code that unsafe code relies on can break it too, which this list doesn't show."
        ),
        rows.join("")
    )
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
