//! Posting a review to the pull request.
//!
//! The page is a static file without any access to GitHub, and it must not
//! hold a token: anyone who can open the page could read it. So the page
//! only writes the review down, in the form GitHub's API for creating a
//! review takes (`POST /repos/{owner}/{repo}/pulls/{number}/reviews`), and
//! the reviewer posts it with their own GitHub CLI login. The review can
//! also be copied as Markdown, to paste as an ordinary comment.
//!
//! A comment on an item goes on a line of the item that GitHub's diff
//! shows: its first added line, else its first removed line, else a line
//! of context. GitHub rejects the whole review if a comment is on a line
//! its diff doesn't show, so the lines come from the file's own diff (the
//! changed lines with 3 lines of context, as GitHub shows them). Comments
//! that fit on no line go into the review's text.

use crate::code::esc;
use crate::data::Data;
use crate::review::{self, Mark, Note, Verdict};
use model::{ChangedFile, Item, SourceLine, Status};
use serde_json::{Value, json};

/// Where a comment goes in GitHub's diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loc {
    pub path: String,
    pub line: usize,
    /// `RIGHT` for the PR's version of a line, `LEFT` for the base's.
    pub side: &'static str,
}

/// Every line of a file's diff: its tag, and its line number in the base
/// and in the PR (only one of them counts for `+` and `-` lines).
fn numbered(hunks: &[SourceLine]) -> Vec<(char, usize, usize)> {
    let (mut o, mut n) = (0, 0);
    let mut out = Vec::new();
    for l in hunks {
        match l.0 {
            '@' => {
                let mut it = l.1.split(' ').filter_map(|x| x.parse::<usize>().ok());
                o = it.next().unwrap_or(1);
                n = it.next().unwrap_or(1);
            }
            '+' => {
                out.push(('+', o, n));
                n += 1;
            }
            '-' => {
                out.push(('-', o, n));
                o += 1;
            }
            _ => {
                out.push((' ', o, n));
                o += 1;
                n += 1;
            }
        }
    }
    out
}

/// The lines of a file that GitHub's diff shows, as `numbered` gives
/// them. For an analyzed file, whose hunks only hold the changes outside
/// items, also its other changed lines (without their context).
fn diff_lines(f: &ChangedFile) -> Vec<(char, usize, usize)> {
    let mut out = numbered(&f.hunks);
    out.extend(f.added_lines.iter().map(|&n| ('+', 0, n as usize)));
    out.extend(f.removed_lines.iter().map(|&o| ('-', o as usize, 0)));
    out
}

/// The line a comment on a file outside the Rust analysis goes on: its
/// first changed line.
pub fn file_loc(f: &ChangedFile) -> Option<Loc> {
    numbered(&f.hunks)
        .into_iter()
        .find(|l| l.0 != ' ')
        .map(|(tag, o, n)| Loc {
            path: f.path.clone(),
            line: if tag == '+' { n } else { o },
            side: if tag == '+' { "RIGHT" } else { "LEFT" },
        })
}

/// The line a comment on an item goes on, from the diffs of its files.
pub fn item_loc(files: &[ChangedFile], it: &Item) -> Option<Loc> {
    let count = |tags: &[char]| it.source.iter().filter(|l| tags.contains(&l.0)).count();
    let span = |start: usize, n: usize| start..=start + n.saturating_sub(1);
    let diff = |path: &str| {
        files
            .iter()
            .find(|f| f.path == path)
            .map(diff_lines)
            .unwrap_or_default()
    };
    // The PR's version: the item's lines in its file.
    let head = (it.status != Status::Removed).then(|| {
        (
            it.info.file.as_str(),
            span(it.info.line, count(&[' ', '+'])),
        )
    });
    // The base's version, maybe in another file (a moved item).
    let base = (it.status != Status::Added).then(|| {
        (
            it.old_file.as_deref().unwrap_or(&it.info.file),
            span(it.old_line.unwrap_or(it.info.line), count(&[' ', '-'])),
        )
    });
    let find = |side: &Option<(&str, std::ops::RangeInclusive<usize>)>, tag: char| {
        let (path, range) = side.as_ref()?;
        diff(path)
            .into_iter()
            .find(|&(t, o, n)| t == tag && range.contains(if tag == '-' { &o } else { &n }))
            .map(|(_, o, n)| Loc {
                path: path.to_string(),
                line: if tag == '-' { o } else { n },
                side: if tag == '-' { "LEFT" } else { "RIGHT" },
            })
    };
    find(&head, '+')
        .or_else(|| find(&base, '-'))
        .or_else(|| find(&head, ' '))
}

/// The line a comment on one hunk of an item goes on: its first changed
/// line that GitHub's diff shows.
pub fn hunk_loc(files: &[ChangedFile], it: &Item, h: &review::Hunk) -> Option<Loc> {
    let find = |path: &str, tag: char, lines: &[usize]| {
        let f = files.iter().find(|f| f.path == path)?;
        diff_lines(f)
            .into_iter()
            .find(|&(t, o, n)| t == tag && lines.contains(if tag == '-' { &o } else { &n }))
            .map(|(_, o, n)| Loc {
                path: path.to_string(),
                line: if tag == '-' { o } else { n },
                side: if tag == '-' { "LEFT" } else { "RIGHT" },
            })
    };
    let added = || find(&it.info.file, '+', &h.new);
    let removed = || find(it.old_file.as_deref().unwrap_or(&it.info.file), '-', &h.old);
    if h.starts_added {
        added().or_else(removed)
    } else {
        removed().or_else(added)
    }
}

/// The address of a line in the pull request's "Files changed" view on
/// GitHub. Its anchor is `diff-` with the SHA-256 of the file's path, then
/// `L` (base) or `R` (PR) and the line number.
pub fn github_url(d: &Data, l: &Loc) -> Option<String> {
    let pr = d.out.meta.pr.as_ref()?;
    Some(format!(
        "https://github.com/{}/pull/{}/files#diff-{}{}{}",
        pr.repo,
        pr.number,
        crate::sha256::hex(l.path.as_bytes()),
        if l.side == "LEFT" { 'L' } else { 'R' },
        l.line
    ))
}

/// A link that opens `l` on GitHub, or nothing if the page isn't for a
/// pull request or there is no line to go to.
pub fn github_link(d: &Data, l: Option<Loc>, class: &str, label: &str, what: &str) -> String {
    let Some((url, l)) = l.and_then(|l| Some((github_url(d, &l)?, l))) else {
        return String::new();
    };
    format!(
        r#"<a class="{class}" href="{}" target="_blank" rel="noopener" title="Open {what} on GitHub ({} line {})">{label}</a>"#,
        esc(&url),
        esc(&l.path),
        l.line
    )
}

/// Where a unit's comment goes, if on a line.
pub fn loc(d: &Data, unit: &str) -> Option<Loc> {
    if let Some(item) = review::hunk_item(unit) {
        let h = review::hunks_of(&item).into_iter().find(|h| h.id == unit)?;
        return hunk_loc(&d.out.files, d.item(&item)?, &h);
    }
    match unit.strip_prefix("file:") {
        Some(path) => file_loc(d.out.files.iter().find(|f| f.path == path)?),
        None => item_loc(&d.out.files, d.item(unit)?),
    }
}

/// How a unit is named in a comment; a hunk by its item.
pub fn unit_name(d: &Data, unit: &str) -> String {
    if let Some(item) = review::hunk_item(unit) {
        return unit_name(d, &item);
    }
    match unit.strip_prefix("file:") {
        Some(path) => path.to_string(),
        None => d
            .item(unit)
            .map_or_else(|| unit.to_string(), |it| d.display_name(it)),
    }
}

/// For a hunk: which part of its item it is, e.g. ", change at line 41".
pub fn part_label(unit: &str) -> String {
    review::hunk_item(unit)
        .and_then(|item| review::hunks_of(&item).into_iter().find(|h| h.id == unit))
        .map_or(String::new(), |h| {
            format!(", change at line {}", h.first_line())
        })
}

/// The review event GitHub takes.
pub fn event_name(v: Verdict) -> &'static str {
    match v {
        Verdict::Approve => "APPROVE",
        Verdict::Changes => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    }
}

/// A comment of the review.
#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub name: String,
    /// For a hunk, its `part_label`.
    pub part: String,
    pub verdict: Verdict,
    pub text: String,
    pub loc: Option<Loc>,
}

impl Comment {
    fn lead(&self) -> String {
        self.lead_at("")
    }

    /// The comment's first line, with where it is (`at`) for Markdown.
    fn lead_at(&self, at: &str) -> String {
        match self.verdict {
            Verdict::Changes => {
                format!("**Change requested** on `{}`{}{at}:", self.name, self.part)
            }
            _ => format!("On `{}`{}{at}:", self.name, self.part),
        }
    }
}

/// What the review will hold.
pub struct Summary {
    pub comments: Vec<Comment>,
    pub approved: usize,
    pub changes: usize,
    pub commented: usize,
    /// Units without a current verdict (not reviewed, or changed since).
    pub open: usize,
    /// Comments on units that changed since: left out.
    pub stale_comments: usize,
    /// Some hunk needs changes, also of an item without a verdict yet.
    pub hunk_changes: bool,
}

impl Summary {
    /// The review's overall verdict, as GitHub would suggest it: request
    /// changes if any unit needs them, approve if every unit is approved,
    /// else comment.
    pub fn suggested(&self) -> Verdict {
        if self.changes > 0 || self.hunk_changes {
            Verdict::Changes
        } else if self.open == 0 && self.commented == 0 && self.approved > 0 {
            Verdict::Approve
        } else {
            Verdict::Comment
        }
    }
}

pub fn summary(d: &Data) -> Summary {
    let (_, total) = review::progress();
    let mut s = Summary {
        comments: Vec::new(),
        approved: 0,
        changes: 0,
        commented: 0,
        open: total,
        stale_comments: 0,
        hunk_changes: false,
    };
    for (unit, mark, Note { verdict, text }) in review::notes() {
        let has_text = !text.trim().is_empty();
        if mark == Mark::Stale {
            s.stale_comments += usize::from(has_text);
            continue;
        }
        // Hunks add their comments; their item counts.
        if review::hunk_item(&unit).is_some() {
            s.hunk_changes |= verdict == Verdict::Changes;
        } else {
            s.open -= 1;
            match verdict {
                Verdict::Approve => s.approved += 1,
                Verdict::Changes => s.changes += 1,
                Verdict::Comment => s.commented += 1,
            }
        }
        if has_text {
            s.comments.push(Comment {
                name: unit_name(d, &unit),
                part: part_label(&unit),
                verdict,
                text: text.trim().to_string(),
                loc: loc(d, &unit),
            });
        }
    }
    s
}

/// The review's text: the reviewer's own, then the comments that fit on
/// no line. GitHub requires a text for anything but an approval.
fn body(comments: &[Comment], own: &str, event: Verdict) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !own.trim().is_empty() {
        parts.push(own.trim().to_string());
    }
    for c in comments.iter().filter(|c| c.loc.is_none()) {
        parts.push(format!("{}\n\n{}", c.lead(), c.text));
    }
    if parts.is_empty() && event != Verdict::Approve {
        parts.push(if comments.is_empty() {
            "Reviewed with Rust PR Explorer.".to_string()
        } else {
            "See the comments on the code.".to_string()
        });
    }
    parts.join("\n\n")
}

/// The request body for GitHub's "create a review" API. `marks` (see
/// `carry::for_review`) goes at the end of the review's text.
pub fn review_json(
    comments: &[Comment],
    own: &str,
    event: Verdict,
    commit: &str,
    marks: &str,
) -> String {
    serde_json::to_string_pretty(&review_value(comments, own, event, commit, marks))
        .unwrap_or_default()
}

fn review_value(
    comments: &[Comment],
    own: &str,
    event: Verdict,
    commit: &str,
    marks: &str,
) -> Value {
    let text = body(comments, own, event);
    let text = match (text.is_empty(), marks.is_empty()) {
        (_, true) => text,
        (true, false) => marks.to_string(),
        (false, false) => format!("{text}\n\n{marks}"),
    };
    let mut v = json!({
        "event": event_name(event),
        "body": text,
        "comments": comments
            .iter()
            .filter_map(|c| {
                let l = c.loc.as_ref()?;
                Some(json!({
                    "path": l.path,
                    "line": l.line,
                    "side": l.side,
                    "body": format!("{}\n\n{}", c.lead(), c.text),
                }))
            })
            .collect::<Vec<Value>>(),
    });
    if v["body"] == "" {
        // An approval without a text.
        v.as_object_mut().map(|o| o.remove("body"));
    }
    if !commit.is_empty() {
        v["commit_id"] = json!(commit);
    }
    v
}

/// The review as Markdown, for an ordinary comment.
pub fn review_markdown(comments: &[Comment], own: &str, event: Verdict) -> String {
    let mut out = format!(
        "**Review: {}**\n",
        match event {
            Verdict::Approve => "Approved",
            Verdict::Changes => "Changes requested",
            Verdict::Comment => "Comments",
        }
    );
    if !own.trim().is_empty() {
        out.push_str(&format!("\n{}\n", own.trim()));
    }
    for c in comments {
        let at = c.loc.as_ref().map_or(String::new(), |l| {
            format!(
                " (`{}` line {}{})",
                l.path,
                l.line,
                if l.side == "LEFT" { ", before" } else { "" }
            )
        });
        out.push_str(&format!("\n{}\n\n{}\n", c.lead_at(&at), c.text));
    }
    out
}

/// The review as JSON that is safe inside single quotes in bash, zsh and
/// fish: no `'`, and no backslash followed by a backslash or `'` (fish
/// unescapes those two even in single quotes). JSON allows `\u0027` and
/// `\u005c` in strings instead, and has no other place for these.
fn shell_safe_json(v: &Value) -> String {
    let json = serde_json::to_string(v).unwrap_or_default();
    let mut out = String::with_capacity(json.len());
    let mut chars = json.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => out.push_str("\\u0027"),
            '\\' => match chars.next() {
                Some('\\') => out.push_str("\\u005c"),
                Some(next) => {
                    out.push('\\');
                    out.push(next);
                }
                None => out.push('\\'),
            },
            c => out.push(c),
        }
    }
    out
}

/// One command that posts the review with the GitHub CLI. The JSON is
/// piped in, so nothing needs to be saved first; `printf` because zsh's
/// `echo` would turn `\n` into line breaks.
///
/// GitHub refuses an approval of one's own PR. So an approval falls back
/// (`||`, which bash, zsh and fish all have) to a comment review whose
/// text starts with "Approved.": any other error would make that fail
/// too, and the page can't know who runs the command.
pub fn command(
    d: &Data,
    comments: &[Comment],
    own: &str,
    event: Verdict,
    marks: &str,
) -> Option<String> {
    let pr = d.out.meta.pr.as_ref()?;
    let post = |v: &Value| {
        format!(
            "printf '%s' '{}' | gh api --method POST repos/{}/pulls/{}/reviews --input -",
            shell_safe_json(v),
            pr.repo,
            pr.number,
        )
    };
    let v = review_value(comments, own, event, &pr.head_commit, marks);
    if event != Verdict::Approve {
        return Some(post(&v));
    }
    Some(format!("{} || {}", post(&v), post(&approved_comment(v))))
}

/// An approval as a comment review, for the author's own PR.
fn approved_comment(mut v: Value) -> Value {
    let body = match v["body"].as_str() {
        Some(b) if !b.is_empty() => format!("Approved.\n\n{b}"),
        _ => "Approved.".to_string(),
    };
    v["event"] = json!("COMMENT");
    v["body"] = json!(body);
    v
}

/// What the posted review consists of, shown above the copy buttons.
pub fn posts_html(comments: &[Comment], own: &str, event: Verdict) -> String {
    let mut parts = vec![
        match event {
            Verdict::Approve => "an <b>approval</b>",
            Verdict::Changes => "a <b>request for changes</b>",
            Verdict::Comment => "a <b>comment review</b>",
        }
        .to_string(),
    ];
    if !own.trim().is_empty() {
        parts.push("your summary".to_string());
    }
    let placed = comments.iter().filter(|c| c.loc.is_some()).count();
    let unplaced = comments.len() - placed;
    let n = |k: usize, what: &str| format!("{k} {what}{}", if k == 1 { "" } else { "s" });
    if placed > 0 {
        parts.push(n(placed, "inline comment"));
    }
    if unplaced > 0 {
        parts.push(format!("{} in the review's text", n(unplaced, "comment")));
    }
    let last = parts.pop().unwrap_or_default();
    let list = if parts.is_empty() {
        last
    } else {
        format!("{} and {last}", parts.join(", "))
    };
    let own_pr = if event == Verdict::Approve {
        " On your own PR, which GitHub doesn't let you approve, it posts a comment review starting with “Approved.” instead, after GitHub's refusal."
    } else {
        ""
    };
    format!("The command posts {list}.{own_pr}")
}

/// The dialog that sums up the review and offers ways to post it.
pub fn finish_html(d: &Data) -> String {
    let s = summary(d);
    let suggested = s.suggested();
    let count =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut facts = vec![
        count(s.approved, "approved", "approved"),
        count(
            s.changes,
            "with changes requested",
            "with changes requested",
        ),
        count(s.commented, "commented", "commented"),
    ];
    if s.open > 0 {
        facts.push(count(s.open, "not reviewed yet", "not reviewed yet"));
    }
    let placed = s.comments.iter().filter(|c| c.loc.is_some()).count();
    let unplaced = s.comments.len() - placed;
    let mut notes = Vec::new();
    if s.stale_comments > 0 {
        notes.push(format!(
            "{} on items that changed since you reviewed them {} left out. Review them again to include them.",
            count(s.stale_comments, "comment", "comments"),
            if s.stale_comments == 1 { "is" } else { "are" }
        ));
    }
    if unplaced > 0 {
        notes.push(format!(
            "{} {} on no line of GitHub's diff (a binary file, for example), so {} into the review's text.",
            count(unplaced, "comment", "comments"),
            if unplaced == 1 { "fits" } else { "fit" },
            if unplaced == 1 { "it goes" } else { "they go" }
        ));
    }
    let radio = |v: Verdict, label: &str, help: &str| {
        format!(
            r#"<label class="rv-ev"><input type="radio" name="rv-event" value="{}"{}> <b>{label}</b> <span class="dim">{help}</span></label>"#,
            v.name(),
            if v == suggested { " checked" } else { "" }
        )
    };
    let list: String = s
        .comments
        .iter()
        .map(|c| {
            format!(
                r#"<li><span class="rv-k {}">{}</span> <code>{}</code>{} <span class="dim">{}</span><div class="rv-q">{}</div></li>"#,
                c.verdict.name(),
                if c.verdict == Verdict::Changes {
                    "✗ change"
                } else {
                    "✎ comment"
                },
                esc(&c.name),
                esc(&c.part),
                c.loc.as_ref().map_or("in the review's text".to_string(), |l| esc(
                    &format!(
                        "{} line {}{}",
                        l.path,
                        l.line,
                        if l.side == "LEFT" { " (base)" } else { "" }
                    )
                )),
                esc(&c.text)
            )
        })
        .collect();
    let post = match &d.out.meta.pr {
        Some(pr) => format!(
            r#"<h4>Post it to the pull request</h4>
<p class="sub">This page has no access to GitHub and holds no token. Copy the command and run it in a terminal: it posts the review with the <a href="https://cli.github.com" target="_blank" rel="noopener">GitHub CLI</a>, using your own login. It works in bash, zsh and fish.</p>
<p id="rv-what">{what}</p>
<div class="rv-row"><button data-rv-out="cmd" class="primary">Copy command</button><button data-rv-out="json">Copy JSON</button></div>
<p class="sub">The review also carries your marks, hidden, so that the page of a later explorer comment can load them.</p>
<p class="sub">The comments go on the commit this page shows ({rev}). If the PR got new commits, GitHub shows them where those lines are now, or as outdated.{custom}</p>"#,
            what = posts_html(&s.comments, "", suggested),
            rev = esc(&pr.head_rev),
            custom = if pr.since.is_some() {
                " This page compares with an earlier commit of the PR, not with its base branch, so comments on removed lines may not fit GitHub's diff; GitHub then rejects the review."
            } else if pr.custom_base {
                " This page compares against a base given with <code>--base</code>, so its diff may differ from GitHub's; GitHub then rejects the review."
            } else {
                ""
            },
        ),
        None => r#"<p class="sub">This page doesn't show a GitHub pull request, so the review can only be copied as Markdown.</p>"#.to_string(),
    };
    format!(
        r#"<div class="rv-finish">
<p>{facts}.</p>{notes}
<h4>Overall verdict</h4>
{r_comment}{r_approve}{r_changes}
<h4>Summary</h4>
<textarea id="rv-body" rows="4" placeholder="Leave a comment for the whole review (optional)"></textarea>
{comments}
{post}
<h4>Or copy it as a comment</h4>
<div class="rv-row"><button data-rv-out="md">Copy as Markdown</button></div>
<textarea id="rv-out" class="hidden" rows="8" readonly></textarea>
</div>"#,
        facts = facts.join(", "),
        notes = notes
            .iter()
            .map(|n| format!(r#"<p class="note">{}</p>"#, esc(n)))
            .collect::<String>(),
        r_comment = radio(
            Verdict::Comment,
            "Comment",
            "Feedback without approving or requesting changes."
        ),
        r_approve = radio(Verdict::Approve, "Approve", "The PR can be merged."),
        r_changes = radio(
            Verdict::Changes,
            "Request changes",
            "Feedback that must be addressed before merging."
        ),
        comments = if list.is_empty() {
            String::new()
        } else {
            format!(
                r#"<h4>{}</h4><ul class="rv-list">{list}</ul>"#,
                count(s.comments.len(), "comment", "comments")
            )
        },
    )
}

/// The editor for a unit's comment.
pub fn editor_html(d: &Data, unit: &str, verdict: Verdict) -> String {
    let n = review::note(unit);
    let at = if d.out.meta.pr.is_some() {
        match loc(d, unit) {
            Some(l) => format!(
                "Posted to GitHub, it goes on line {} of <code>{}</code>{}.",
                l.line,
                esc(&l.path),
                if l.side == "LEFT" {
                    " (the base version)"
                } else {
                    ""
                }
            ),
            None => "It fits on no line of GitHub's diff, so posted to GitHub, it goes into the review's text.".to_string(),
        }
    } else {
        String::new()
    };
    let button = |v: Verdict, label: &str| {
        format!(
            r#"<button data-rv-save="{}"{}>{label}</button>"#,
            v.name(),
            if v == verdict {
                r#" class="primary""#
            } else {
                ""
            }
        )
    };
    format!(
        r#"<div class="rv-editor" data-rv-for="{}" data-rv-verdict="{}">
<textarea id="rv-text" rows="6" placeholder="{}">{}</textarea>
<p class="sub">{at} Markdown works. Ctrl+Enter saves.</p>
<div class="rv-row">{}{}{}<span class="grow"></span><button data-modal-close>Cancel</button></div>
</div>"#,
        esc(unit),
        verdict.name(),
        if verdict == Verdict::Changes {
            "What needs to change?"
        } else {
            "Leave a comment"
        },
        esc(&n.text),
        button(Verdict::Comment, "✎ Comment"),
        button(Verdict::Changes, "✗ Request changes"),
        button(Verdict::Approve, "✓ Approve"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::Kind;

    fn sl(tag: char, text: &str) -> SourceLine {
        SourceLine(tag, text.into())
    }

    fn file(path: &str, hunks: Vec<SourceLine>) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            status: Status::Changed,
            lang: "rust".into(),
            hunks,
            ..Default::default()
        }
    }

    fn item(status: Status, line: usize, source: Vec<SourceLine>) -> Item {
        let mut it = Item {
            status,
            source,
            ..Default::default()
        };
        it.info.id = "c::f".into();
        it.info.kind = Kind::Fn;
        it.info.file = "src/lib.rs".into();
        it.info.line = line;
        it
    }

    /// `src/lib.rs`: line 10 of the base became lines 10 and 11 of the
    /// PR; a hunk further down removed line 40 of the base (38 in the PR).
    fn lib() -> ChangedFile {
        file(
            "src/lib.rs",
            vec![
                sl('@', "8 8"),
                sl(' ', "fn f() {"),
                sl(' ', "    a();"),
                sl('-', "    b();"),
                sl('+', "    b(1);"),
                sl('+', "    c();"),
                sl(' ', "}"),
                sl('@', "38 39"),
                sl(' ', "fn g() {"),
                sl('-', "    old();"),
                sl(' ', "}"),
            ],
        )
    }

    #[test]
    fn comments_go_on_the_first_changed_line() {
        let files = [lib()];
        // fn f at line 8 in both versions.
        let src = vec![
            sl(' ', "fn f() {"),
            sl(' ', "    a();"),
            sl('-', "    b();"),
            sl('+', "    b(1);"),
            sl('+', "    c();"),
            sl(' ', "}"),
        ];
        let f = item(Status::Changed, 8, src);
        assert_eq!(
            item_loc(&files, &f),
            Some(Loc {
                path: "src/lib.rs".into(),
                line: 10,
                side: "RIGHT"
            })
        );
        // Only a removed line: on the base side.
        let mut g = item(
            Status::Changed,
            39,
            vec![sl(' ', "fn g() {"), sl('-', "    old();"), sl(' ', "}")],
        );
        g.old_line = Some(38);
        assert_eq!(
            item_loc(&files, &g),
            Some(Loc {
                path: "src/lib.rs".into(),
                line: 39,
                side: "LEFT"
            })
        );
    }

    #[test]
    fn analyzed_files_list_their_changed_lines() {
        // The hunks only hold a change outside items; the item's change is
        // in the line lists.
        let mut f = file("src/lib.rs", vec![sl('@', "40 40"), sl('+', "mod x;")]);
        f.added_lines = vec![9, 40];
        f.removed_lines = vec![9];
        let mut it = item(Status::Changed, 8, vec![sl(' ', "fn f() {")]);
        it.source
            .extend([sl('-', "    a();"), sl('+', "    b();"), sl(' ', "}")]);
        assert_eq!(
            item_loc(std::slice::from_ref(&f), &it).map(|l| (l.line, l.side)),
            Some((9, "RIGHT"))
        );
        // Without the lists, it fits on no line.
        f.added_lines.clear();
        f.removed_lines.clear();
        assert_eq!(item_loc(&[f], &it), None);
    }

    #[test]
    fn items_outside_the_diff_fit_on_no_line() {
        let files = [lib()];
        // Lines 20 to 22: no hunk covers them.
        let h = item(Status::Changed, 20, vec![sl(' ', "fn h() {}")]);
        assert_eq!(item_loc(&files, &h), None);
        // Another file without a diff.
        let mut moved = item(Status::Added, 1, vec![sl('+', "fn f() {}")]);
        moved.info.file = "src/other.rs".into();
        assert_eq!(item_loc(&files, &moved), None);
    }

    #[test]
    fn files_take_their_first_changed_line() {
        let f = file(
            "README.md",
            vec![sl('@', "3 3"), sl(' ', "x"), sl('-', "old"), sl('+', "new")],
        );
        assert_eq!(file_loc(&f).map(|l| (l.line, l.side)), Some((4, "LEFT")));
        assert_eq!(file_loc(&file("logo.png", Vec::new())), None);
    }

    #[test]
    fn json_has_the_api_fields() {
        let comments = vec![
            Comment {
                name: "f".into(),
                part: String::new(),
                verdict: Verdict::Changes,
                text: "Please rename.".into(),
                loc: Some(Loc {
                    path: "src/lib.rs".into(),
                    line: 10,
                    side: "RIGHT",
                }),
            },
            Comment {
                name: "logo.png".into(),
                part: String::new(),
                verdict: Verdict::Comment,
                text: "Too large.".into(),
                loc: None,
            },
        ];
        let v: Value =
            serde_json::from_str(&review_json(&comments, "", Verdict::Changes, "abc123", ""))
                .unwrap();
        assert_eq!(v["event"], "REQUEST_CHANGES");
        assert_eq!(v["commit_id"], "abc123");
        assert_eq!(v["comments"].as_array().unwrap().len(), 1);
        assert_eq!(v["comments"][0]["line"], 10);
        assert_eq!(v["comments"][0]["side"], "RIGHT");
        assert_eq!(v["comments"][0]["path"], "src/lib.rs");
        // The comment that fits on no line is in the text.
        assert!(v["body"].as_str().unwrap().contains("Too large."));

        // A comment review needs a text even without any.
        let v: Value =
            serde_json::from_str(&review_json(&[], "", Verdict::Comment, "", "")).unwrap();
        assert!(!v["body"].as_str().unwrap().is_empty());
        assert!(v.get("commit_id").is_none());
        // An approval doesn't.
        let v: Value =
            serde_json::from_str(&review_json(&[], "", Verdict::Approve, "", "")).unwrap();
        assert!(v.get("body").is_none());
    }

    #[test]
    fn shell_safe() {
        let v = json!({"body": "it's a \\ and \\' and \"q\"\nnext"});
        let s = shell_safe_json(&v);
        assert!(!s.contains('\''));
        assert!(!s.contains("\\\\"));
        assert!(!s.contains("\\'"));
        assert_eq!(serde_json::from_str::<Value>(&s).unwrap(), v);
    }

    #[test]
    fn posts() {
        let c = |loc: bool| Comment {
            name: "f".into(),
            part: String::new(),
            verdict: Verdict::Comment,
            text: "x".into(),
            loc: loc.then(|| Loc {
                path: "a.rs".into(),
                line: 1,
                side: "RIGHT",
            }),
        };
        assert!(
            posts_html(&[], "", Verdict::Approve)
                .starts_with("The command posts an <b>approval</b>. On your own PR")
        );
        assert_eq!(
            posts_html(&[c(true), c(true), c(false)], "Nice", Verdict::Changes),
            "The command posts a <b>request for changes</b>, your summary, 2 inline comments and 1 comment in the review's text."
        );
    }

    #[test]
    fn own_pr_approval() {
        // Without a text, an approval has no body; the fallback has one.
        let v = approved_comment(review_value(&[], "", Verdict::Approve, "abc", ""));
        assert_eq!(v["event"], "COMMENT");
        assert_eq!(v["body"], "Approved.");
        assert_eq!(v["commit_id"], "abc");
        let v = approved_comment(review_value(&[], "Looks good", Verdict::Approve, "", ""));
        assert_eq!(v["body"], "Approved.\n\nLooks good");
    }

    #[test]
    fn marks_go_at_the_end_of_the_text() {
        let m = "<!-- rust-pr-explorer-marks v1 TWFu -->";
        let v = review_value(&[], "", Verdict::Approve, "", m);
        assert_eq!(v["body"], m);
        let v = review_value(&[], "Looks good", Verdict::Comment, "", m);
        assert_eq!(v["body"], format!("Looks good\n\n{m}"));
        let v = approved_comment(review_value(&[], "", Verdict::Approve, "", m));
        assert_eq!(v["body"], format!("Approved.\n\n{m}"));
    }
}
