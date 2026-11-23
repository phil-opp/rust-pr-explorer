//! What changed since a review: when an item or file is marked as
//! reviewed, the version the reviewer saw is saved with the mark. When the
//! mark goes stale on a page generated for a newer revision, that saved
//! version is compared with the current one, so the reviewer only has to
//! read what is new.

use crate::code::esc;
use crate::data::Data;
use crate::files::hunks_html;
use model::{ChangedFile, Item, SourceLine};
use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffTag, capture_diff_slices, group_diff_ops};
use std::collections::BTreeSet;

/// Lines of context around each change.
const CONTEXT: usize = 3;

/// One side (base or head) of what the reviewer saw.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<String>,
    /// Line number of the first line in its file.
    #[serde(default)]
    pub start: usize,
    /// Line numbers of every line, when they don't follow each other (the
    /// parts of a file that its diff shows). Empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nos: Vec<usize>,
}

impl Side {
    fn no(&self, i: usize) -> usize {
        self.nos.get(i).copied().unwrap_or(self.start + i)
    }
}

/// The version of a unit that the reviewer marked: both sides of its code,
/// and everything else its review mark depends on, as one fact per line
/// ("status: changed", "now public as `crate::Op`", …).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// When it was marked, in milliseconds since the Unix epoch.
    #[serde(default)]
    pub at: f64,
    /// The head revision the page showed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rev: String,
    #[serde(default)]
    pub head: Side,
    #[serde(default)]
    pub base: Side,
    /// The language for syntax colors: `rust`, `python`, …
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lang: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub facts: Vec<String>,
}

/// The current version of a unit (an item id or a `file:` unit).
pub fn snapshot(d: &Data, unit: &str) -> Option<Snapshot> {
    let mut s = match unit.strip_prefix("file:") {
        Some(path) => file_snapshot(d.out.files.iter().find(|f| f.path == path)?),
        None => item_snapshot(d.item(unit)?),
    };
    s.rev = match &d.out.meta.pr {
        Some(pr) => pr.head_rev.clone(),
        None => d.out.meta.head.clone(),
    };
    Some(s)
}

fn item_snapshot(it: &Item) -> Snapshot {
    let side = |skip: char, start: usize| Side {
        lines: it
            .source
            .iter()
            .filter(|l| l.0 != skip)
            .map(|l| l.1.clone())
            .collect(),
        start,
        nos: Vec::new(),
    };
    let c = &it.changes;
    let mut facts = vec![
        format!("status: {}", it.status.css()),
        format!("kind: {}", it.info.kind.label()),
    ];
    for (on, what) in [
        (c.signature, "signature changed"),
        (c.body, "body changed"),
        (c.docs, "docs changed"),
        (c.cosmetic, "only formatting changed"),
    ] {
        if on {
            facts.push(what.to_string());
        }
    }
    if let Some(m) = &c.moved_from {
        facts.push(format!("moved from `{m}`"));
    }
    facts.extend(
        c.exports_added
            .iter()
            .map(|p| format!("now public as `{p}`")),
    );
    facts.extend(
        c.exports_removed
            .iter()
            .map(|p| format!("no longer public as `{p}`")),
    );
    facts.extend(
        it.info
            .sig_refs
            .iter()
            .map(|(name, r)| format!("`{name}` in the signature refers to `{}`", r.path)),
    );
    let base_start = it.old_line.unwrap_or(it.info.line);
    Snapshot {
        head: side('-', it.info.line),
        base: side('+', base_start),
        lang: "rust".into(),
        facts,
        ..Default::default()
    }
}

fn file_snapshot(f: &ChangedFile) -> Snapshot {
    let full = |text: &Option<String>| Side {
        lines: text
            .as_deref()
            .unwrap_or("")
            .lines()
            .map(str::to_string)
            .collect(),
        start: 1,
        nos: Vec::new(),
    };
    let (head, base) = if f.old_text.is_some() || f.new_text.is_some() {
        (full(&f.new_text), full(&f.old_text))
    } else {
        // Only the parts of the file that its diff shows.
        let (mut head, mut base) = (Side::default(), Side::default());
        let (mut old_no, mut new_no) = (1, 1);
        for l in &f.hunks {
            if l.0 == '@' {
                let mut it = l.1.split(' ').filter_map(|n| n.parse::<usize>().ok());
                old_no = it.next().unwrap_or(1);
                new_no = it.next().unwrap_or(1);
                continue;
            }
            if l.0 != '+' {
                base.lines.push(l.1.clone());
                base.nos.push(old_no);
                old_no += 1;
            }
            if l.0 != '-' {
                head.lines.push(l.1.clone());
                head.nos.push(new_no);
                new_no += 1;
            }
        }
        (head, base)
    };
    let mut facts = vec![format!("status: {}", f.status.css())];
    if f.too_large {
        facts.push("too large to compare line by line".into());
    }
    Snapshot {
        head,
        base,
        lang: f.lang.clone(),
        facts,
        ..Default::default()
    }
}

/// A line diff of two versions of one side, as hunks with `CONTEXT` lines
/// around each change. `@` lines hold "old_line new_line" wherever the
/// numbering jumps, as `hunks_html` expects.
pub fn interdiff(old: &Side, new: &Side) -> Vec<SourceLine> {
    let ops = capture_diff_slices(Algorithm::Myers, &old.lines, &new.lines);
    let mut out = Vec::new();
    for group in group_diff_ops(ops, CONTEXT) {
        // What `hunks_html` would number the next old and new line.
        let mut next: Option<(usize, usize)> = None;
        let mut push = |tag: char, o: Option<usize>, n: Option<usize>, text: &str| {
            let (exp_o, exp_n) = next.unwrap_or((0, 0));
            let jump =
                next.is_none() || o.is_some_and(|o| o != exp_o) || n.is_some_and(|n| n != exp_n);
            if jump {
                let (o2, n2) = (o.unwrap_or(exp_o), n.unwrap_or(exp_n));
                out.push(SourceLine('@', format!("{o2} {n2}")));
                next = Some((o2, n2));
            }
            let (no, nn) = next.unwrap_or((0, 0));
            next = Some(match tag {
                '+' => (no, nn + 1),
                '-' => (no + 1, nn),
                _ => (no + 1, nn + 1),
            });
            out.push(SourceLine(tag, text.to_string()));
        };
        for op in group {
            let (or, nr) = (op.old_range(), op.new_range());
            match op.tag() {
                DiffTag::Equal => {
                    for (a, b) in or.zip(nr) {
                        push(' ', Some(old.no(a)), Some(new.no(b)), &new.lines[b]);
                    }
                }
                tag => {
                    if tag != DiffTag::Insert {
                        for a in or {
                            push('-', Some(old.no(a)), None, &old.lines[a]);
                        }
                    }
                    if tag != DiffTag::Delete {
                        for b in nr {
                            push('+', None, Some(new.no(b)), &new.lines[b]);
                        }
                    }
                }
            }
        }
    }
    out
}

fn counts(lines: &[SourceLine]) -> (usize, usize) {
    let added = lines.iter().filter(|l| l.0 == '+').count();
    let removed = lines.iter().filter(|l| l.0 == '-').count();
    (added, removed)
}

fn stats(lines: &[SourceLine]) -> String {
    let (a, r) = counts(lines);
    format!(r#"<span class="d-add">+{a}</span> <span class="d-del">−{r}</span>"#)
}

/// Facts with backticks as code.
fn fact_html(text: &str) -> String {
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

/// The comparison of the reviewed version `saved` with the current one
/// `now`, for the dialog that a stale mark opens. `when` is the date of
/// the review, as the browser formats it.
pub fn since_html(d: &Data, saved: Option<&Snapshot>, now: &Snapshot, when: &str) -> String {
    let Some(saved) = saved else {
        return r#"<p class="since-intro">This mark was made on a page that didn't save the reviewed version yet, so it can't show what changed since. Mark it as reviewed again: the next time it changes, this shows the difference to this version.</p>"#.to_string();
    };
    let mut html = format!(
        r#"<p class="since-intro">You reviewed this on {}{}. Below: what is different now, compared with the version you reviewed.</p>"#,
        esc(when),
        if saved.rev.is_empty() || saved.rev == now.rev {
            String::new()
        } else {
            format!(
                " (at <code>{}</code>; this page shows <code>{}</code>)",
                esc(&saved.rev),
                esc(&now.rev)
            )
        }
    );
    let lang = if now.lang.is_empty() {
        &saved.lang
    } else {
        &now.lang
    };
    let section = |title: &str, old: &Side, new: &Side, same: &str| -> String {
        let diff = interdiff(old, new);
        if diff.is_empty() {
            format!(r#"<h4>{title}</h4><p class="dim since-same">{same}</p>"#)
        } else {
            format!(
                r#"<h4>{title} <span class="since-stats">{}</span></h4>{}"#,
                stats(&diff),
                hunks_html(d, &diff, lang)
            )
        }
    };
    html.push_str(&section(
        "The PR's version",
        &saved.head,
        &now.head,
        if now.head.lines.is_empty() {
            "Not in the PR's version, as when you reviewed it."
        } else {
            "Unchanged since your review."
        },
    ));
    let base_diff = interdiff(&saved.base, &now.base);
    if !base_diff.is_empty() {
        html.push_str(&section("The base version", &saved.base, &now.base, ""));
        html.push_str(r#"<p class="dim">The base changed too: the PR was rebased, or is compared with another base.</p>"#);
    }
    let old: BTreeSet<&String> = saved.facts.iter().collect();
    let new: BTreeSet<&String> = now.facts.iter().collect();
    let gone: Vec<&&String> = old.difference(&new).collect();
    let came: Vec<&&String> = new.difference(&old).collect();
    if !gone.is_empty() || !came.is_empty() {
        html.push_str("<h4>Other differences</h4><ul class=\"since-facts\">");
        for f in gone {
            html.push_str(&format!(
                r#"<li class="del"><span class="dim">was:</span> {}</li>"#,
                fact_html(f)
            ));
        }
        for f in came {
            html.push_str(&format!(
                r#"<li class="add"><span class="dim">now:</span> {}</li>"#,
                fact_html(f)
            ));
        }
        html.push_str("</ul>");
    }
    html
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(start: usize, lines: &[&str]) -> Side {
        Side {
            lines: lines.iter().map(|s| s.to_string()).collect(),
            start,
            nos: Vec::new(),
        }
    }

    fn render(lines: &[SourceLine]) -> Vec<String> {
        lines.iter().map(|l| format!("{}{}", l.0, l.1)).collect()
    }

    #[test]
    fn interdiff_numbers_lines_and_keeps_context() {
        let old = side(
            10,
            &[
                "fn f() {", "a();", "b();", "c();", "d();", "e();", "f();", "g();", "h();", "}",
            ],
        );
        let new = side(
            12,
            &[
                "fn f() {", "a();", "b();", "c();", "d();", "e();", "f();", "g();", "x();", "}",
            ],
        );
        assert_eq!(
            render(&interdiff(&old, &new)),
            ["@15 17", " e();", " f();", " g();", "-h();", "+x();", " }"]
        );
        assert!(interdiff(&old, &old).is_empty());
    }

    #[test]
    fn interdiff_restarts_numbering_where_excerpts_jump() {
        let old = Side {
            lines: vec!["a".into(), "b".into(), "z".into()],
            start: 0,
            nos: vec![1, 2, 40],
        };
        let new = Side {
            lines: vec!["a".into(), "c".into(), "z".into()],
            start: 0,
            nos: vec![1, 2, 50],
        };
        assert_eq!(
            render(&interdiff(&old, &new)),
            ["@1 1", " a", "-b", "+c", "@40 50", " z"]
        );
    }

    #[test]
    fn file_snapshots_follow_the_hunks() {
        let f = ChangedFile {
            path: "a.txt".into(),
            status: model::Status::Changed,
            lang: "text".into(),
            hunks: vec![
                SourceLine('@', "3 4".into()),
                SourceLine(' ', "keep".into()),
                SourceLine('-', "old".into()),
                SourceLine('+', "new".into()),
            ],
            ..Default::default()
        };
        let s = file_snapshot(&f);
        assert_eq!(s.head.lines, ["keep", "new"]);
        assert_eq!(s.head.nos, [4, 5]);
        assert_eq!(s.base.lines, ["keep", "old"]);
        assert_eq!(s.base.nos, [3, 4]);
    }

    #[test]
    fn item_snapshots_have_both_sides_and_facts() {
        let mut it = Item {
            status: model::Status::Changed,
            source: vec![
                SourceLine(' ', "fn f() {".into()),
                SourceLine('-', "    1".into()),
                SourceLine('+', "    2".into()),
                SourceLine(' ', "}".into()),
            ],
            old_line: Some(3),
            ..Default::default()
        };
        it.info.line = 7;
        it.changes.body = true;
        it.changes.exports_added.push("crate::f".into());
        let s = item_snapshot(&it);
        assert_eq!(s.head.lines, ["fn f() {", "    2", "}"]);
        assert_eq!(s.head.start, 7);
        assert_eq!(s.base.lines, ["fn f() {", "    1", "}"]);
        assert_eq!(s.base.start, 3);
        assert!(s.facts.contains(&"body changed".to_string()));
        assert!(s.facts.contains(&"now public as `crate::f`".to_string()));
    }

    #[test]
    fn snapshots_round_trip() {
        let s = Snapshot {
            at: 1.5e12,
            rev: "abc".into(),
            head: side(3, &["x"]),
            base: Side::default(),
            lang: "rust".into(),
            facts: vec!["status: added".into()],
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Snapshot>(&json).unwrap(), s);
    }
}
