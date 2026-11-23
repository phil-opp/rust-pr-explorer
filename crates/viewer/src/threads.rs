//! The pull request's review threads on GitHub, as they were when the page
//! was built: shown under the line they are on, in the source diff of the
//! changed item that contains it, and listed on the overview.
//!
//! A thread is placed by GitHub's position: its file, its line, and whether
//! that line is in the PR's version or the base's. Outdated threads have no
//! current line; they, and threads on lines no item shows, are only
//! listed (in full) on the overview, or on their file's card.

use crate::code::{esc, markdown_file_html};
use crate::data::Data;
use model::{Item, Kind, ReviewThread, Status};
use std::collections::HashMap;

/// Where each thread is shown, by its index in `PrMeta::threads`.
#[derive(Default)]
pub struct Placement {
    /// In the source diff of an item.
    pub by_item: HashMap<String, Vec<usize>>,
    /// On the card of a file outside the Rust analysis.
    pub by_file: HashMap<String, Vec<usize>>,
    /// The item or file each thread is shown on.
    pub at: HashMap<usize, Where>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Where {
    Item(String),
    File(String),
}

/// The lines an item takes in the PR's version of its file and in the
/// base's: (file, first line, last line).
fn ranges(it: &Item) -> (Option<(&str, usize, usize)>, Option<(&str, usize, usize)>) {
    let count = |tags: &[char]| it.source.iter().filter(|l| tags.contains(&l.0)).count();
    fn span(file: &str, start: usize, n: usize) -> Option<(&str, usize, usize)> {
        (n > 0).then(|| (file, start, start + n - 1))
    }
    let (old_start, new_start) = match it.status {
        Status::Removed => (it.info.line, it.info.line),
        _ => (it.old_line.unwrap_or(it.info.line), it.info.line),
    };
    let head = (it.status != Status::Removed)
        .then(|| span(&it.info.file, new_start, count(&[' ', '+'])))
        .flatten();
    let base = (it.status != Status::Added)
        .then(|| {
            span(
                it.old_file.as_deref().unwrap_or(&it.info.file),
                old_start,
                count(&[' ', '-']),
            )
        })
        .flatten();
    (head, base)
}

pub fn place(d: &Data) -> Placement {
    let mut p = Placement::default();
    let Some(pr) = &d.out.meta.pr else { return p };
    // Files that have a card under "Other changed files".
    let carded = |path: &str| {
        d.out
            .files
            .iter()
            .any(|f| f.path == path && (!f.analyzed || f.outside_items > 0 || !f.parts.is_empty()))
    };
    for (i, t) in pr.threads.iter().enumerate() {
        let item = t.line.and_then(|line| {
            // The innermost changed item around the line (a method, not
            // its impl).
            d.out
                .items
                .values()
                .filter(|it| it.info.kind != Kind::Mod && it.status != Status::Unchanged)
                .filter_map(|it| {
                    let (head, base) = ranges(it);
                    let (file, a, b) = if t.left { base } else { head }?;
                    (file == t.path && (a..=b).contains(&line)).then_some((b - a, it))
                })
                .min_by_key(|&(len, _)| len)
                .map(|(_, it)| it.info.id.clone())
        });
        match item {
            Some(id) => {
                p.by_item.entry(id.clone()).or_default().push(i);
                p.at.insert(i, Where::Item(id));
            }
            None if carded(&t.path) => {
                p.by_file.entry(t.path.clone()).or_default().push(i);
                p.at.insert(i, Where::File(t.path.clone()));
            }
            None => {}
        }
    }
    p
}

impl Data {
    pub fn threads(&self) -> &[ReviewThread] {
        self.out
            .meta
            .pr
            .as_ref()
            .map_or(&[][..], |pr| pr.threads.as_slice())
    }

    /// The threads shown in an item's source diff.
    pub fn item_threads(&self, id: &str) -> Vec<&ReviewThread> {
        self.placement
            .by_item
            .get(id)
            .map(|list| list.iter().map(|&i| &self.threads()[i]).collect())
            .unwrap_or_default()
    }
}

/// "2 review threads, 1 open".
fn count_label(list: &[&ReviewThread]) -> String {
    let open = list.iter().filter(|t| !t.resolved).count();
    let n = list.len();
    let what = format!("{n} review thread{}", if n == 1 { "" } else { "s" });
    match open {
        0 => format!("{what}, resolved"),
        _ if open == n && n == 1 => "1 open review thread".to_string(),
        _ if open == n => format!("{what}, all open"),
        _ => format!("{what}, {open} open"),
    }
}

/// For a source summary: "💬 1 open review thread".
pub fn summary_note(list: &[&ReviewThread]) -> String {
    if list.is_empty() {
        return String::new();
    }
    format!(
        r#" · <span class="rt-count{}">💬 {}</span>"#,
        if list.iter().any(|t| !t.resolved) {
            " rt-open"
        } else {
            ""
        },
        count_label(list)
    )
}

/// The state of a thread, as GitHub names it.
fn state_html(t: &ReviewThread) -> String {
    let mut out = String::new();
    if t.resolved {
        out.push_str(r#"<span class="rt-state rt-resolved">✓ Resolved</span>"#);
    } else {
        out.push_str(r#"<span class="rt-state rt-unresolved">● Open</span>"#);
    }
    if t.outdated {
        out.push_str(r#"<span class="rt-state rt-outdated">Outdated</span>"#);
    }
    out
}

/// "line 12", "lines 10–12" or, for an outdated thread, where it was.
fn line_label(t: &ReviewThread) -> String {
    let side = if t.left { " (base)" } else { "" };
    match (t.start_line, t.line, t.original_line) {
        (Some(a), Some(b), _) if a != b => format!("lines {a}–{b}{side}"),
        (_, Some(b), _) => format!("line {b}{side}"),
        (_, None, Some(o)) => format!("was at line {o}"),
        _ => "file".to_string(),
    }
}

fn author_html(login: &str) -> String {
    if login.is_empty() {
        r#"<span class="rt-author dim">ghost</span>"#.to_string()
    } else {
        format!(r#"<span class="rt-author">{}</span>"#, esc(login))
    }
}

/// `2026-11-23T15:33:01Z` → `2026-11-23`.
fn date(s: &str) -> &str {
    s.get(..10).unwrap_or(s)
}

/// A thread with all its comments. Resolved ones start folded, and so do
/// those listed `with_place` (on the overview, away from their code).
pub fn thread_html(d: &Data, t: &ReviewThread, with_place: bool) -> String {
    let first = t.comments.first();
    let replies = t.comments.len().saturating_sub(1);
    let head = format!(
        r#"{}{}<span class="dim">{}{}</span>{}"#,
        state_html(t),
        first.map_or(String::new(), |c| author_html(&c.author)),
        if with_place {
            format!("{} {} · ", esc(&t.path), line_label(t))
        } else if t.left {
            "on the base version · ".to_string()
        } else {
            String::new()
        },
        match replies {
            0 => "no replies".to_string(),
            1 => "1 reply".to_string(),
            n => format!("{n} replies"),
        },
        if crate::code::web_url(&t.url) && t.url.starts_with("https://github.com/") {
            format!(
                r#"<a class="rt-gh" href="{}" target="_blank" rel="noopener">GitHub ↗</a>"#,
                esc(&t.url)
            )
        } else {
            String::new()
        }
    );
    let comments: String = t
        .comments
        .iter()
        .map(|c| {
            format!(
                r#"<div class="rt-comment"><div class="rt-meta">{} <span class="dim">{}</span></div><div class="docs">{}</div></div>"#,
                author_html(&c.author),
                esc(date(&c.created_at)),
                markdown_file_html(d, &c.body.replace("\r\n", "\n"))
            )
        })
        .collect();
    format!(
        r#"<details class="rthread{}"{}><summary>{head}</summary>{comments}</details>"#,
        if t.resolved { " resolved" } else { "" },
        if t.resolved || with_place {
            ""
        } else {
            " open"
        },
    )
}

/// The threads on one line of a source diff, as a row of the diff: inside
/// `<pre>` (`split`: inside the side-by-side grid).
pub fn row_html(d: &Data, list: &[&ReviewThread], split: bool) -> String {
    if list.is_empty() {
        return String::new();
    }
    format!(
        r#"<span class="rt-row{}">{}</span>"#,
        if split { " rt-split" } else { "" },
        list.iter()
            .map(|t| thread_html(d, t, false))
            .collect::<String>()
    )
}

/// The threads on a file's card, with their lines.
pub fn file_html(d: &Data, path: &str) -> String {
    let Some(list) = d.placement.by_file.get(path) else {
        return String::new();
    };
    let threads: Vec<&ReviewThread> = list.iter().map(|&i| &d.threads()[i]).collect();
    format!(
        r#"<div class="rt-file"><div class="rt-file-h">💬 {}</div>{}</div>"#,
        count_label(&threads),
        threads
            .iter()
            .map(|t| {
                format!(
                    r#"<div class="rt-at">{}</div>{}"#,
                    line_label(t),
                    thread_html(d, t, false)
                )
            })
            .collect::<String>()
    )
}

/// The overview's list of every thread: one line each for those shown on
/// an item or file, the whole thread for the rest.
pub fn overview_html(d: &Data) -> String {
    let all = d.threads();
    if all.is_empty() {
        return String::new();
    }
    let refs: Vec<&ReviewThread> = all.iter().collect();
    let mut placed = String::new();
    let mut rest = String::new();
    for (i, t) in all.iter().enumerate() {
        let snippet: String = t
            .comments
            .first()
            .map(|c| {
                let line = c.body.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                let mut s: String = line.chars().take(90).collect();
                if s.len() < line.len() {
                    s.push('…');
                }
                s
            })
            .unwrap_or_default();
        let link = match d.placement.at.get(&i) {
            Some(Where::Item(id)) => d.item(id).map(|it| {
                format!(
                    r#"<a data-open="{}" title="Open on the canvas">{}</a>"#,
                    esc(id),
                    esc(&d.display_name(it))
                )
            }),
            Some(Where::File(path)) => Some(format!(
                r#"<a data-goto-file="{}">{}</a>"#,
                esc(path),
                esc(path)
            )),
            None => None,
        };
        match link {
            Some(link) => placed.push_str(&format!(
                r#"<div class="rt-line{}">{}<span class="rt-where">{link} <span class="dim">{}</span></span>{}<span class="rt-snippet">{}</span></div>"#,
                if t.resolved { " resolved" } else { "" },
                state_html(t),
                line_label(t),
                t.comments
                    .first()
                    .map_or(String::new(), |c| author_html(&c.author)),
                esc(&snippet)
            )),
            None => rest.push_str(&thread_html(d, t, true)),
        }
    }
    let rest = if rest.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="rt-rest-h">Not on a line this page shows (outdated, or outside the changed items):</div>{rest}"#
        )
    };
    format!(
        r#"<h2>Review threads {} <span class="h-note">{}</span></h2><div class="rt-overview">{placed}{rest}</div>"#,
        crate::pages::help_html(
            "The pull request's review threads on GitHub when this page was built. Each is also shown under its line, in the item's source."
        ),
        count_label(&refs)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Output, PrMeta, SourceLine, ThreadComment};

    fn thread(path: &str, line: usize, left: bool) -> ReviewThread {
        ReviewThread {
            path: path.to_string(),
            line: Some(line),
            left,
            url: "https://github.com/o/r/pull/1#discussion_r1".to_string(),
            comments: vec![ThreadComment {
                author: "a".to_string(),
                body: "Why <b>this</b>?".to_string(),
                created_at: "2026-11-23T10:00:00Z".to_string(),
            }],
            ..Default::default()
        }
    }

    fn item(id: &str, kind: Kind, status: Status, line: usize, source: &str) -> Item {
        let mut it = Item::default();
        it.info.id = id.to_string();
        it.info.kind = kind;
        it.info.file = "src/lib.rs".to_string();
        it.info.line = line;
        it.status = status;
        it.source = source
            .lines()
            .map(|l| SourceLine(l.chars().next().unwrap(), l[1..].to_string()))
            .collect();
        it
    }

    fn data(items: Vec<Item>, threads: Vec<ReviewThread>) -> Data {
        let out = Output {
            meta: model::Meta {
                pr: Some(PrMeta {
                    threads,
                    ..Default::default()
                }),
                ..Default::default()
            },
            crates: Vec::new(),
            items: items
                .into_iter()
                .map(|it| (it.info.id.clone(), it))
                .collect(),
            files: Vec::new(),
            externals: Default::default(),
            warnings: Vec::new(),
        };
        Data::new(out)
    }

    #[test]
    fn threads_go_on_the_innermost_changed_item() {
        // An impl at lines 10–14 with a method at 11–13; the method's
        // line 12 was replaced (base line 12 removed, head line 12 added).
        let d = data(
            vec![
                item(
                    "c::Impl",
                    Kind::Impl,
                    Status::Changed,
                    10,
                    " a\n b\n-c\n+C\n d\n e",
                ),
                item(
                    "c::Impl::m",
                    Kind::Method,
                    Status::Changed,
                    11,
                    " b\n-c\n+C\n d",
                ),
                item("c::other", Kind::Fn, Status::Unchanged, 20, " x\n y"),
            ],
            vec![
                thread("src/lib.rs", 12, false),
                thread("src/lib.rs", 12, true),
                thread("src/lib.rs", 10, false),
                thread("src/lib.rs", 21, false),
                thread("src/main.rs", 12, false),
            ],
        );
        let p = &d.placement;
        assert_eq!(p.by_item["c::Impl::m"], [0, 1]);
        assert_eq!(p.by_item["c::Impl"], [2]);
        // An unchanged item isn't shown with its source; another file has
        // no card.
        assert!(!p.at.contains_key(&3) && !p.at.contains_key(&4));
        let html = overview_html(&d);
        assert!(html.contains("5 review threads, all open"));
        // Unplaced threads are shown whole, with their comments escaped.
        assert!(html.contains("src/main.rs line 12"));
        assert!(html.contains("&lt;b&gt;this&lt;/b&gt;"));
    }

    #[test]
    fn thread_rows_appear_in_the_source_diff() {
        let mut resolved = thread("src/lib.rs", 12, false);
        resolved.resolved = true;
        let d = data(
            vec![item(
                "c::f",
                Kind::Fn,
                Status::Changed,
                10,
                " a\n b\n-c\n+C\n d",
            )],
            vec![resolved, thread("src/lib.rs", 12, true)],
        );
        let it = d.item("c::f").unwrap();
        let html = crate::code::marked_html(d.source_seg(it, None, false, &Default::default()));
        assert_eq!(html.matches(r#"class="rt-row"#).count(), 2);
        assert!(html.contains("2 review threads, 1 open"));
        // The open thread on the base side, right after the removed line.
        let del = html.find(r#"data-t="-""#).unwrap();
        let add = html.find(r#"data-t="+""#).unwrap();
        let open = html.find(r#"<details class="rthread" open>"#).unwrap();
        let closed = html.find(r#"<details class="rthread resolved">"#).unwrap();
        assert!(del < open && open < add && add < closed);
    }
}
