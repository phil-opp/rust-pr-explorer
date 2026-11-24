//! Posting a review to the pull request.
//!
//! The page is a static file without any access to GitHub, and it must not
//! hold a token: anyone who can open the page could read it. So the page
//! only writes the review down, in the form GitHub's API for creating a
//! review takes (`POST /repos/{owner}/{repo}/pulls/{number}/reviews`), and
//! the reviewer posts it with their own GitHub CLI login. The review can
//! also be copied as Markdown, to paste as an ordinary comment.
//!
//! A comment goes on lines of its item that GitHub's diff shows: on a
//! hunk, or an item with one change, from that change's first added
//! line to its last one in the same hunk of GitHub's diff (else its
//! removed lines); on other items, their first added line, else their
//! first removed line, else a line of context. GitHub rejects the whole
//! review if a comment is on a line its diff doesn't show, so the lines
//! come from the file's own diff (the changed lines with 3 lines of
//! context, as GitHub shows them). Comments that fit on no line go into
//! the review's text, and the command falls back to putting all of them
//! there if GitHub rejects the review anyway.

use crate::code::esc;
use crate::data::Data;
use crate::review::{self, Mark, Note, Verdict};
use model::{ChangedFile, Item, PrMeta, SourceLine, Status};
use serde_json::{Value, json};

/// Where a comment goes in GitHub's diff: one line, or the lines from
/// `start_line` to `line`, all on one side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loc {
    pub path: String,
    pub line: usize,
    /// The first line of a comment on several lines.
    pub start_line: Option<usize>,
    /// `RIGHT` for the PR's version of a line, `LEFT` for the base's.
    pub side: &'static str,
}

impl Loc {
    /// The first line it covers.
    pub fn first(&self) -> usize {
        self.start_line.unwrap_or(self.line)
    }

    /// "line 10" or "lines 10–14".
    pub fn lines(&self) -> String {
        match self.start_line {
            Some(s) => format!("lines {s}–{}", self.line),
            None => format!("line {}", self.line),
        }
    }
}

/// A line of a file's diff as GitHub shows it: its tag, its line number
/// in the base and in the PR (only one of them counts for `+` and `-`
/// lines), and which hunk of the diff it is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row {
    tag: char,
    old: usize,
    new: usize,
    hunk: usize,
}

impl Row {
    /// Its line number on the side its tag is on (the PR's for context).
    fn num(&self) -> usize {
        if self.tag == '-' { self.old } else { self.new }
    }
}

/// Lines of context around a change, as GitHub shows them.
const CONTEXT: usize = 3;

/// The rows of a diff with its `@` lines.
fn numbered(hunks: &[SourceLine]) -> Vec<Row> {
    let (mut o, mut n) = (0, 0);
    let mut hunk = 0;
    let mut out = Vec::new();
    for l in hunks {
        let tag = match l.0 {
            '@' => {
                let mut it = l.1.split(' ').filter_map(|x| x.parse::<usize>().ok());
                o = it.next().unwrap_or(1);
                n = it.next().unwrap_or(1);
                hunk += usize::from(!out.is_empty());
                continue;
            }
            '+' | '-' => l.0,
            _ => ' ',
        };
        out.push(Row {
            tag,
            old: o,
            new: n,
            hunk,
        });
        o += usize::from(tag != '+');
        n += usize::from(tag != '-');
    }
    out
}

/// The rows of a diff from the numbers of its added lines (in the PR) and
/// removed lines (in the base): the unchanged lines between follow from
/// them, and get cut into hunks as GitHub does, with `CONTEXT` lines
/// around each change. Changes with up to twice that between them share
/// a hunk.
fn rows_of_lines(added: &[u32], removed: &[u32]) -> Vec<Row> {
    let (mut a, mut r) = (added.to_vec(), removed.to_vec());
    a.sort_unstable();
    r.sort_unstable();
    let (a, r) = (&a[..], &r[..]);
    let (mut i, mut j) = (0, 0);
    let (mut o, mut n) = (1, 1);
    let mut hunk = 0;
    let mut seen = false;
    let mut out = Vec::new();
    let push = |out: &mut Vec<Row>, tag, o, n, hunk| {
        out.push(Row {
            tag,
            old: o,
            new: n,
            hunk,
        })
    };
    loop {
        // Skip numbers out of order (only in broken data).
        while i < r.len() && (r[i] as usize) < o {
            i += 1;
        }
        while j < a.len() && (a[j] as usize) < n {
            j += 1;
        }
        // Removed lines first, as in a unified diff.
        if i < r.len() && r[i] as usize == o {
            push(&mut out, '-', o, n, hunk);
            (o, i, seen) = (o + 1, i + 1, true);
            continue;
        }
        if j < a.len() && a[j] as usize == n {
            push(&mut out, '+', o, n, hunk);
            (n, j, seen) = (n + 1, j + 1, true);
            continue;
        }
        // Unchanged lines up to the next change.
        let to_r = r.get(i).map_or(usize::MAX, |&x| x as usize - o);
        let to_a = a.get(j).map_or(usize::MAX, |&x| x as usize - n);
        let k = to_r.min(to_a);
        let before = if seen { k.min(CONTEXT) } else { 0 };
        for _ in 0..before {
            push(&mut out, ' ', o, n, hunk);
            (o, n) = (o + 1, n + 1);
        }
        if k == usize::MAX {
            break;
        }
        let after = (k - before).min(CONTEXT);
        let skipped = k - before - after;
        if skipped > 0 && seen {
            hunk += 1;
        }
        (o, n) = (o + skipped, n + skipped);
        for _ in 0..after {
            push(&mut out, ' ', o, n, hunk);
            (o, n) = (o + 1, n + 1);
        }
    }
    out
}

/// The lines of a file that GitHub's diff shows. For an analyzed file,
/// whose hunks only hold the changes outside items, they follow from the
/// numbers of all its changed lines.
fn diff_rows(f: &ChangedFile) -> Vec<Row> {
    if f.added_lines.is_empty() && f.removed_lines.is_empty() {
        numbered(&f.hunks)
    } else {
        rows_of_lines(&f.added_lines, &f.removed_lines)
    }
}

/// The rows of the diff of `path`.
fn rows_of(files: &[ChangedFile], path: &str) -> Vec<Row> {
    files
        .iter()
        .find(|f| f.path == path)
        .map(diff_rows)
        .unwrap_or_default()
}

fn side_of(tag: char) -> &'static str {
    if tag == '-' { "LEFT" } else { "RIGHT" }
}

/// The first of `lines` (sorted) with the tag `tag` that the diff shows.
fn first_of(rows: &[Row], path: &str, tag: char, lines: &[usize]) -> Option<Loc> {
    rows.iter()
        .filter(|r| r.tag == tag && lines.binary_search(&r.num()).is_ok())
        .min_by_key(|r| r.num())
        .map(|r| Loc {
            path: path.to_string(),
            line: r.num(),
            start_line: None,
            side: side_of(tag),
        })
}

/// The lines `lines` (sorted) with the tag `tag` cover, from the first
/// one the diff shows to the last one in the same hunk of the diff:
/// GitHub takes a comment on several lines only within one hunk.
fn span_of(rows: &[Row], path: &str, tag: char, lines: &[usize]) -> Option<Loc> {
    let shown: Vec<&Row> = rows
        .iter()
        .filter(|r| r.tag == tag && lines.binary_search(&r.num()).is_ok())
        .collect();
    let first = shown.iter().min_by_key(|r| r.num())?;
    let last = shown
        .iter()
        .filter(|r| r.hunk == first.hunk)
        .map(|r| r.num())
        .max()
        .unwrap_or(first.num());
    Some(Loc {
        path: path.to_string(),
        line: last,
        start_line: (last > first.num()).then_some(first.num()),
        side: side_of(tag),
    })
}

/// The line a comment on a file outside the Rust analysis goes on: its
/// first changed line.
pub fn file_loc(f: &ChangedFile) -> Option<Loc> {
    numbered(&f.hunks)
        .into_iter()
        .find(|l| l.tag != ' ')
        .map(|r| Loc {
            path: f.path.clone(),
            line: r.num(),
            start_line: None,
            side: side_of(r.tag),
        })
}

/// The lines an item has in the PR's version (`' '` and `'+'`) and in
/// the base (`' '` and `'-'`), by tag.
fn item_lines(it: &Item, tags: &[char]) -> Vec<usize> {
    let (mut o, mut n) = (it.old_line.unwrap_or(it.info.line), it.info.line);
    let mut out = Vec::new();
    for l in &it.source {
        let tag = if l.0 == '+' || l.0 == '-' { l.0 } else { ' ' };
        if tags.contains(&tag) {
            out.push(if tag == '-' { o } else { n });
        }
        o += usize::from(tag != '+');
        n += usize::from(tag != '-');
    }
    out
}

/// Where a comment on an item goes, from the diffs of its files. An item
/// with one change (not counting reformatting) gets it on that change's
/// lines: its added lines, or if it has none its removed lines. Others
/// get it on their first added line, else their first removed line, else
/// a line of context: an added or removed item at its start, and one
/// with several changes not on the first of them, since the comment is
/// on all of them.
pub fn item_loc(files: &[ChangedFile], it: &Item) -> Option<Loc> {
    let head = it.info.file.as_str();
    let base = it.old_file.as_deref().unwrap_or(head);
    if let [one] = &review::changes(it)[..]
        && let Some(l) = span_of(&rows_of(files, head), head, '+', &one.new)
            .or_else(|| span_of(&rows_of(files, base), base, '-', &one.old))
    {
        return Some(l);
    }
    let added = || first_of(&rows_of(files, head), head, '+', &item_lines(it, &['+']));
    let removed = || first_of(&rows_of(files, base), base, '-', &item_lines(it, &['-']));
    let context = || {
        (it.status != Status::Removed)
            .then(|| first_of(&rows_of(files, head), head, ' ', &item_lines(it, &[' '])))
            .flatten()
    };
    match it.status {
        Status::Removed => removed(),
        _ => added().or_else(removed).or_else(context),
    }
}

/// Where a comment on one hunk of an item goes: on its added lines that
/// GitHub's diff shows, as far as one hunk of that diff holds them, else
/// on its removed lines. The PR's lines come first since the comment is
/// on what the PR has (and GitHub only offers suggestions there).
pub fn hunk_loc(files: &[ChangedFile], it: &Item, h: &review::Hunk) -> Option<Loc> {
    let head = it.info.file.as_str();
    let base = it.old_file.as_deref().unwrap_or(head);
    span_of(&rows_of(files, head), head, '+', &h.new)
        .or_else(|| span_of(&rows_of(files, base), base, '-', &h.old))
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
        l.first()
    ))
}

/// A link that opens `l` on GitHub, or nothing if the page isn't for a
/// pull request or there is no line to go to.
pub fn github_link(d: &Data, l: Option<Loc>, class: &str, label: &str, what: &str) -> String {
    let Some((url, l)) = l.and_then(|l| Some((github_url(d, &l)?, l))) else {
        return String::new();
    };
    format!(
        r#"<a class="{class}" href="{}" target="_blank" rel="noopener" title="Open {what} on GitHub ({} {})">{label}</a>"#,
        esc(&url),
        esc(&l.path),
        l.lines()
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

/// The address of lines of a file on GitHub, in the compared version:
/// the PR's commit for `RIGHT`, the base's for `LEFT`. No lines for the
/// whole file.
fn blob_url(pr: &PrMeta, path: &str, side: &str, lines: Option<(usize, usize)>) -> String {
    let rev = if side == "LEFT" {
        if pr.base_commit.is_empty() {
            &pr.base_rev
        } else {
            &pr.base_commit
        }
    } else if pr.head_commit.is_empty() {
        &pr.head_rev
    } else {
        &pr.head_commit
    };
    let anchor = match lines {
        Some((a, b)) if b > a => format!("#L{a}-L{b}"),
        Some((a, _)) => format!("#L{a}"),
        None => String::new(),
    };
    // Spaces and the like in the path.
    let path: String = path
        .chars()
        .map(|c| match c {
            ' ' => "%20".to_string(),
            '#' => "%23".to_string(),
            '?' => "%3F".to_string(),
            '%' => "%25".to_string(),
            c => c.to_string(),
        })
        .collect();
    format!("https://github.com/{}/blob/{rev}/{path}{anchor}", pr.repo)
}

/// Where a unit is, as a Markdown link to its lines on GitHub, for a
/// comment in the review's text: the lines its comment goes on, else all
/// of an item's lines, else the whole file. `None` if the page isn't for
/// a pull request.
pub fn place_link(d: &Data, unit: &str, l: Option<&Loc>) -> Option<String> {
    let pr = d.out.meta.pr.as_ref()?;
    let link = |path: &str, side: &str, lines: Option<(usize, usize)>, label: String| {
        format!("[`{path}` {label}]({})", blob_url(pr, path, side, lines))
    };
    if let Some(l) = l {
        return Some(link(&l.path, l.side, Some((l.first(), l.line)), l.lines()));
    }
    if let Some(path) = unit.strip_prefix("file:") {
        let removed = d
            .out
            .files
            .iter()
            .any(|f| f.path == path && f.status == Status::Removed);
        let side = if removed { "LEFT" } else { "RIGHT" };
        return Some(format!("[`{path}`]({})", blob_url(pr, path, side, None)));
    }
    let it = d.item(&review::hunk_item(unit).unwrap_or_else(|| unit.to_string()))?;
    let (path, side, start, tags) = if it.status == Status::Removed {
        (
            it.old_file.as_deref().unwrap_or(&it.info.file),
            "LEFT",
            it.old_line.unwrap_or(it.info.line),
            [' ', '-'],
        )
    } else {
        (it.info.file.as_str(), "RIGHT", it.info.line, [' ', '+'])
    };
    let n = it
        .source
        .iter()
        .filter(|l| tags.contains(&l.0))
        .count()
        .max(1);
    let end = start + n - 1;
    let label = if end > start {
        format!("lines {start}–{end}")
    } else {
        format!("line {start}")
    };
    Some(link(path, side, Some((start, end)), label))
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
    /// Where it is, as a Markdown link to the lines on GitHub, for the
    /// review's text.
    pub link: Option<String>,
}

impl Comment {
    /// The first line of an inline comment.
    fn lead(&self) -> String {
        self.lead_at("")
    }

    /// The first line of the comment in the review's text: with where it
    /// is, since it's on no line there.
    fn lead_in_text(&self) -> String {
        self.lead_at(
            &self
                .link
                .as_ref()
                .map_or(String::new(), |l| format!(" ({l})")),
        )
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
            let loc = loc(d, &unit);
            s.comments.push(Comment {
                name: unit_name(d, &unit),
                part: part_label(&unit),
                verdict,
                text: text.trim().to_string(),
                link: place_link(d, &unit, loc.as_ref()),
                loc,
            });
        }
    }
    s
}

/// The review's text: the reviewer's own, then the comments that fit on
/// no line (all of them unless `inline`). GitHub requires a text for
/// anything but an approval.
fn body(comments: &[Comment], own: &str, event: Verdict, inline: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !own.trim().is_empty() {
        parts.push(own.trim().to_string());
    }
    for c in comments.iter().filter(|c| !inline || c.loc.is_none()) {
        parts.push(format!("{}\n\n{}", c.lead_in_text(), c.text));
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

/// The request body for GitHub's "create a review" API.
pub fn review_json(comments: &[Comment], own: &str, event: Verdict, commit: &str) -> String {
    serde_json::to_string_pretty(&review_value(comments, own, event, commit, true))
        .unwrap_or_default()
}

/// The review as GitHub's API takes it; with `inline`, comments that fit
/// on a line go there, else all go into the review's text.
fn review_value(
    comments: &[Comment],
    own: &str,
    event: Verdict,
    commit: &str,
    inline: bool,
) -> Value {
    let mut v = json!({
        "event": event_name(event),
        "body": body(comments, own, event, inline),
        "comments": comments
            .iter()
            .filter(|_| inline)
            .filter_map(|c| {
                let l = c.loc.as_ref()?;
                let mut o = json!({
                    "path": l.path,
                    "line": l.line,
                    "side": l.side,
                    "body": format!("{}\n\n{}", c.lead(), c.text),
                });
                if let Some(start) = l.start_line {
                    o["start_line"] = json!(start);
                    o["start_side"] = json!(l.side);
                }
                Some(o)
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
        let at = match (&c.link, &c.loc) {
            (Some(link), _) => format!(" ({link})"),
            (None, Some(l)) => format!(
                " (`{}` {}{})",
                l.path,
                l.lines(),
                if l.side == "LEFT" { ", before" } else { "" }
            ),
            (None, None) => String::new(),
        };
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
/// Each attempt runs only if the one before failed (`||`, which bash, zsh
/// and fish all have), and GitHub creates a review whole or not at all.
/// GitHub refuses the whole review if one comment is on a line its diff
/// doesn't show, which can happen where its diff differs from the page's
/// (a renamed file, another base, a tie the two diff algorithms break
/// differently). So the review is tried again with every comment in its
/// text, with a link to its lines. And GitHub refuses an approval of
/// one's own PR, so an approval is tried again as a comment review whose
/// text starts with "Approved.", before giving up the inline comments.
/// The page can't tell these errors apart, or know who runs the command.
pub fn command(d: &Data, comments: &[Comment], own: &str, event: Verdict) -> Option<String> {
    let pr = d.out.meta.pr.as_ref()?;
    let post = |v: &Value| {
        format!(
            "printf '%s' '{}' | gh api --method POST repos/{}/pulls/{}/reviews --input -",
            shell_safe_json(v),
            pr.repo,
            pr.number,
        )
    };
    let tries = attempts(comments, own, event, &pr.head_commit);
    Some(tries.iter().map(post).collect::<Vec<_>>().join(" || "))
}

/// The reviews `command` tries to post, in order.
fn attempts(comments: &[Comment], own: &str, event: Verdict, commit: &str) -> Vec<Value> {
    let v = |inline| review_value(comments, own, event, commit, inline);
    let mut tries = vec![v(true)];
    if event == Verdict::Approve {
        tries.push(approved_comment(v(true)));
    }
    tries.push(v(false));
    if event == Verdict::Approve {
        tries.push(approved_comment(v(false)));
    }
    // Without inline comments, the second half is the first again.
    let mut out: Vec<Value> = Vec::new();
    for t in tries {
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out
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
    let fallback = if placed > 0 {
        " If GitHub refuses a comment's line, it posts the review again with every comment in its text, linked to its lines."
    } else {
        ""
    };
    format!("The command posts {list}.{own_pr}{fallback}")
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
                        "{} {}{}",
                        l.path,
                        l.lines(),
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
<p class="sub">The comments go on the commit this page shows ({rev}). If the PR got new commits, GitHub shows them where those lines are now, or as outdated.{custom}</p>"#,
            what = posts_html(&s.comments, "", suggested),
            rev = esc(&pr.head_rev),
            custom = if pr.since.is_some() {
                " This page compares with an earlier commit of the PR, not with its base branch, so some comments may not fit GitHub's diff; they then go into the review's text."
            } else if pr.custom_base {
                " This page compares against a base given with <code>--base</code>, so its diff may differ from GitHub's; comments that don't fit it then go into the review's text."
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
                "Posted to GitHub, it goes on {} of <code>{}</code>{}.",
                l.lines(),
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
    use model::{Kind, Output};

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

    fn at(path: &str, start: Option<usize>, line: usize, side: &'static str) -> Option<Loc> {
        Some(Loc {
            path: path.into(),
            line,
            start_line: start,
            side,
        })
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
    fn comments_go_on_the_change() {
        let files = [lib()];
        // fn f at line 8 in both versions: on its added lines.
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
            at("src/lib.rs", Some(10), 11, "RIGHT")
        );
        // Only a removed line: on the base side.
        let mut g = item(
            Status::Changed,
            39,
            vec![sl(' ', "fn g() {"), sl('-', "    old();"), sl(' ', "}")],
        );
        g.old_line = Some(38);
        assert_eq!(item_loc(&files, &g), at("src/lib.rs", None, 39, "LEFT"));
    }

    #[test]
    fn added_items_take_their_first_line() {
        let f = file(
            "src/lib.rs",
            vec![
                sl('@', "4 4"),
                sl(' ', "}"),
                sl('+', "fn n() {"),
                sl('+', "    x();"),
                sl('+', "}"),
            ],
        );
        let n = item(
            Status::Added,
            5,
            vec![sl('+', "fn n() {"), sl('+', "    x();"), sl('+', "}")],
        );
        assert_eq!(item_loc(&[f], &n), at("src/lib.rs", None, 5, "RIGHT"));
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
            item_loc(std::slice::from_ref(&f), &it),
            at("src/lib.rs", None, 9, "RIGHT")
        );
        // Without the lists, it fits on no line.
        f.added_lines.clear();
        f.removed_lines.clear();
        assert_eq!(item_loc(&[f], &it), None);
    }

    #[test]
    fn rows_follow_from_the_changed_lines() {
        // Base line 5 became PR lines 5 and 6, and a line was added after
        // base line 15 (PR line 17): 10 unchanged lines apart, so two
        // hunks, with 3 lines of context each.
        let rows = rows_of_lines(&[5, 6, 17], &[5]);
        let r = |tag, old, new, hunk| Row {
            tag,
            old,
            new,
            hunk,
        };
        assert_eq!(
            rows,
            vec![
                r(' ', 2, 2, 0),
                r(' ', 3, 3, 0),
                r(' ', 4, 4, 0),
                r('-', 5, 5, 0),
                r('+', 6, 5, 0),
                r('+', 6, 6, 0),
                r(' ', 6, 7, 0),
                r(' ', 7, 8, 0),
                r(' ', 8, 9, 0),
                r(' ', 13, 14, 1),
                r(' ', 14, 15, 1),
                r(' ', 15, 16, 1),
                r('+', 16, 17, 1),
                r(' ', 16, 18, 1),
                r(' ', 17, 19, 1),
                r(' ', 18, 20, 1),
            ]
        );
        // The same as the diff with its context lines gives.
        let hunks = vec![
            sl('@', "2 2"),
            sl(' ', "2"),
            sl(' ', "3"),
            sl(' ', "4"),
            sl('-', "5"),
            sl('+', "5a"),
            sl('+', "5b"),
            sl(' ', "6"),
            sl(' ', "7"),
            sl(' ', "8"),
            sl('@', "13 14"),
            sl(' ', "13"),
            sl(' ', "14"),
            sl(' ', "15"),
            sl('+', "new"),
            sl(' ', "16"),
            sl(' ', "17"),
            sl(' ', "18"),
        ];
        assert_eq!(numbered(&hunks), rows);
        // Six unchanged lines apart: one hunk.
        let rows = rows_of_lines(&[1, 8], &[]);
        assert!(rows.iter().all(|r| r.hunk == 0));
        assert_eq!(rows.len(), 11);
    }

    #[test]
    fn ranges_stay_in_one_hunk_of_the_diff() {
        let hunk = |new: Vec<usize>| review::Hunk {
            id: "c::f#1".into(),
            new,
            old: Vec::new(),
            starts_added: true,
        };
        let mut f = file("src/lib.rs", Vec::new());
        let it = item(Status::Changed, 1, Vec::new());
        // Lines 10 and 18 added, 7 unchanged lines apart: GitHub shows
        // them in two hunks, so the comment goes on the first.
        f.added_lines = vec![10, 18];
        let files = [f.clone()];
        assert_eq!(
            hunk_loc(&files, &it, &hunk(vec![10, 18])),
            at("src/lib.rs", None, 10, "RIGHT")
        );
        // 6 apart: one hunk, one range over both.
        f.added_lines = vec![10, 17];
        assert_eq!(
            hunk_loc(&[f], &it, &hunk(vec![10, 17])),
            at("src/lib.rs", Some(10), 17, "RIGHT")
        );
    }

    #[test]
    fn hunks_prefer_their_added_lines() {
        // A change that starts with its removed line still goes on the
        // added ones: the PR's code.
        let h = review::Hunk {
            id: "c::f#1".into(),
            new: vec![10, 11],
            old: vec![10],
            starts_added: false,
        };
        let it = item(Status::Changed, 8, Vec::new());
        assert_eq!(
            hunk_loc(&[lib()], &it, &h),
            at("src/lib.rs", Some(10), 11, "RIGHT")
        );
        // Only removed lines: the base side.
        let h = review::Hunk {
            id: "c::f#2".into(),
            new: Vec::new(),
            old: vec![39],
            starts_added: false,
        };
        assert_eq!(
            hunk_loc(&[lib()], &it, &h),
            at("src/lib.rs", None, 39, "LEFT")
        );
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
        assert_eq!(file_loc(&f), at("README.md", None, 4, "LEFT"));
        assert_eq!(file_loc(&file("logo.png", Vec::new())), None);
    }

    fn comment(text: &str, loc: Option<Loc>) -> Comment {
        Comment {
            name: "f".into(),
            part: String::new(),
            verdict: Verdict::Comment,
            text: text.into(),
            link: Some(format!("[{text}](url)")),
            loc,
        }
    }

    #[test]
    fn json_has_the_api_fields() {
        let comments = vec![
            Comment {
                verdict: Verdict::Changes,
                ..comment("Please rename.", at("src/lib.rs", None, 10, "RIGHT"))
            },
            comment("Split this.", at("src/lib.rs", Some(20), 24, "LEFT")),
            comment("Too large.", None),
        ];
        let v: Value =
            serde_json::from_str(&review_json(&comments, "", Verdict::Changes, "abc123")).unwrap();
        assert_eq!(v["event"], "REQUEST_CHANGES");
        assert_eq!(v["commit_id"], "abc123");
        let c = v["comments"].as_array().unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["line"], 10);
        assert_eq!(c[0]["side"], "RIGHT");
        assert_eq!(c[0]["path"], "src/lib.rs");
        assert!(c[0].get("start_line").is_none());
        assert_eq!(c[1]["start_line"], 20);
        assert_eq!(c[1]["start_side"], "LEFT");
        assert_eq!(c[1]["line"], 24);
        // The comment that fits on no line is in the text, with its link.
        let body = v["body"].as_str().unwrap();
        assert!(body.contains("On `f` ([Too large.](url)):\n\nToo large."));
        assert!(!body.contains("Split this."));

        // A comment review needs a text even without any.
        let v: Value = serde_json::from_str(&review_json(&[], "", Verdict::Comment, "")).unwrap();
        assert!(!v["body"].as_str().unwrap().is_empty());
        assert!(v.get("commit_id").is_none());
        // An approval doesn't.
        let v: Value = serde_json::from_str(&review_json(&[], "", Verdict::Approve, "")).unwrap();
        assert!(v.get("body").is_none());
    }

    #[test]
    fn falls_back_to_comments_in_the_text() {
        let comments = vec![
            comment("a", at("src/lib.rs", None, 10, "RIGHT")),
            comment("b", None),
        ];
        let t = attempts(&comments, "", Verdict::Comment, "abc");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0]["comments"].as_array().unwrap().len(), 1);
        assert_eq!(t[1]["comments"].as_array().unwrap().len(), 0);
        let body = t[1]["body"].as_str().unwrap();
        assert!(body.contains("([a](url))") && body.contains("([b](url))"));
        assert_eq!(t[1]["commit_id"], "abc");

        // An approval: as an approval, then as a comment review, each with
        // inline comments first.
        let t = attempts(&comments, "", Verdict::Approve, "abc");
        let ev: Vec<_> = t
            .iter()
            .map(|v| (v["event"].clone(), v["comments"].as_array().unwrap().len()))
            .collect();
        assert_eq!(
            ev,
            [
                (json!("APPROVE"), 1),
                (json!("COMMENT"), 1),
                (json!("APPROVE"), 0),
                (json!("COMMENT"), 0)
            ]
        );
        // Without inline comments, nothing to fall back to.
        assert_eq!(
            attempts(&[comment("b", None)], "", Verdict::Comment, "").len(),
            1
        );
        assert_eq!(attempts(&[], "", Verdict::Approve, "").len(), 2);
    }

    #[test]
    fn links_to_lines() {
        let pr = PrMeta {
            repo: "o/r".into(),
            base_rev: "base1".into(),
            base_commit: "base1full".into(),
            head_rev: "head1".into(),
            head_commit: "head1full".into(),
            ..Default::default()
        };
        assert_eq!(
            blob_url(&pr, "src/a b.rs", "RIGHT", Some((3, 5))),
            "https://github.com/o/r/blob/head1full/src/a%20b.rs#L3-L5"
        );
        assert_eq!(
            blob_url(&pr, "x.rs", "LEFT", Some((7, 7))),
            "https://github.com/o/r/blob/base1full/x.rs#L7"
        );
        let d = Data::new(Output {
            meta: model::Meta {
                pr: Some(pr),
                ..Default::default()
            },
            crates: Vec::new(),
            items: Default::default(),
            files: vec![ChangedFile {
                status: Status::Removed,
                ..file("old.png", Vec::new())
            }],
            externals: Default::default(),
            warnings: Vec::new(),
            semver_checks: None,
        });
        assert_eq!(
            place_link(&d, "file:old.png", None).unwrap(),
            "[`old.png`](https://github.com/o/r/blob/base1full/old.png)"
        );
        assert_eq!(
            place_link(&d, "x", at("a.rs", Some(2), 4, "RIGHT").as_ref()).unwrap(),
            "[`a.rs` lines 2–4](https://github.com/o/r/blob/head1full/a.rs#L2-L4)"
        );
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
        let c = |loc: bool| comment("x", loc.then(|| at("a.rs", None, 1, "RIGHT")).flatten());
        assert!(
            posts_html(&[], "", Verdict::Approve)
                .starts_with("The command posts an <b>approval</b>. On your own PR")
        );
        assert_eq!(
            posts_html(&[c(true), c(true), c(false)], "Nice", Verdict::Changes),
            "The command posts a <b>request for changes</b>, your summary, 2 inline comments and 1 comment in the review's text. If GitHub refuses a comment's line, it posts the review again with every comment in its text, linked to its lines."
        );
    }

    #[test]
    fn own_pr_approval() {
        // Without a text, an approval has no body; the fallback has one.
        let v = approved_comment(review_value(&[], "", Verdict::Approve, "abc", true));
        assert_eq!(v["event"], "COMMENT");
        assert_eq!(v["body"], "Approved.");
        assert_eq!(v["commit_id"], "abc");
        let v = approved_comment(review_value(&[], "Looks good", Verdict::Approve, "", true));
        assert_eq!(v["body"], "Approved.\n\nLooks good");
    }
}
