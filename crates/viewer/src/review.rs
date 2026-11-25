//! Review marks: the reviewer goes through the items and files one by
//! one and gives each a verdict, as on GitHub: approve, request changes or
//! comment, the last two with a comment. Then it moves on to the next
//! unreviewed one. The collected review can be posted to the PR (see
//! `post`).
//!
//! A mark remembers what the item looked like when it was marked (a hash
//! of its diff). When the page is generated again for a newer revision of
//! the PR and the item's diff is different, the mark no longer counts: the
//! item shows as changed since the review.
//!
//! The mark also keeps the version itself (see `since`), so that a stale
//! mark can show what changed since the review.
//!
//! A changed item whose diff has several separate changes ("hunks", as
//! the source view shows them between folded unchanged lines) can also be
//! reviewed hunk by hunk. A hunk's mark is keyed by its changed lines, so
//! it survives new commits that leave the hunk alone: after a push, only
//! the new hunks need a review. When every hunk has a verdict, the item
//! gets one too.
//!
//! A changed file outside the Rust analysis is reviewed hunk by hunk the
//! same way. There a hunk is one box of the file's view where it has them
//! (a section of a Markdown file, a function of a Python file, a key of a
//! YAML file), and the other changed lines are hunks of its line diff (see
//! `files::review_hunks`).
//!
//! Marks are kept in the browser's local storage, per pull request (or per
//! pair of compared trees), when the browser allows that. The page of the
//! whole PR and the page of the changes since an earlier commit share
//! them. Their diffs differ, so a mark also records which change of the
//! unit was reviewed: from which version to which (see [`Edge`]). A unit
//! counts as reviewed when the reviewed changes lead from the version the
//! page compares with to the one it shows: the whole PR reviewed at an
//! earlier commit plus the changes since then make the PR's item reviewed,
//! and the whole PR reviewed covers the changes since any commit. A hunk's
//! mark is keyed by its changed lines, so it is shared when both pages show
//! the same change.

use crate::code::esc;
use crate::data::{Data, changed};
use crate::dom;
use crate::since::Snapshot;
use model::stable_hash as hash;
use model::{ChangedFile, Item, Kind, Status, UseCtx};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};

/// What the reviewer has said about a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Not reviewed yet.
    Open,
    /// Reviewed, and unchanged since.
    Done,
    /// Reviewed, but its diff is different now.
    Stale,
}

/// The reviewer's verdict on a unit, like the three choices of a GitHub
/// review.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    #[default]
    Approve,
    Changes,
    Comment,
}

impl Verdict {
    pub fn name(self) -> &'static str {
        match self {
            Verdict::Approve => "approve",
            Verdict::Changes => "changes",
            Verdict::Comment => "comment",
        }
    }

    pub fn from_name(s: &str) -> Option<Verdict> {
        [Verdict::Approve, Verdict::Changes, Verdict::Comment]
            .into_iter()
            .find(|v| v.name() == s)
    }
}

/// What the reviewer said about a marked unit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    #[serde(default)]
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

/// A reviewed change of a unit: from the version a page compared with to
/// the version it showed (see [`version_pair`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Edge {
    from: String,
    to: String,
    /// Reviewed on the page of the whole PR (else of the changes since an
    /// earlier commit).
    full: bool,
}

#[derive(Default)]
struct Review {
    /// Review mode: the controls for marking are shown.
    on: bool,
    /// Storage key of this PR's marks.
    key: String,
    /// The page shows the changes since an earlier commit, not the whole
    /// PR.
    since: bool,
    /// Everything that can be marked, in reading order: Rust items by
    /// module (as the sidebar lists them) and position, then the changed
    /// files.
    units: Vec<String>,
    /// The same units in the order that **Next** goes through them: along
    /// the calls (see [`call_order`]).
    order: Vec<String>,
    /// Hash of each unit's diff.
    prints: HashMap<String, u64>,
    /// Saved marks: unit → hash of its diff when it was marked. For
    /// items and files only from the page of the whole PR (the hashes of
    /// the other page's diffs would replace them); for hunks from both.
    marks: BTreeMap<String, u64>,
    /// The versions each unit is compared between on this page.
    pairs: HashMap<String, (String, String)>,
    /// Saved reviewed changes of each unit, from both pages.
    edges: BTreeMap<String, Vec<Edge>>,
    /// The version each marked unit had when it was marked. Missing for
    /// marks made by pages from before versions were saved.
    versions: BTreeMap<String, Snapshot>,
    /// Verdicts and comments of marked units. A mark without one is a
    /// plain approval (also every mark made before verdicts existed).
    notes: BTreeMap<String, Note>,
    /// Reviewed units the reviewer unfolded again, for this page only.
    unfolded: HashSet<String>,
    /// The hunks of items that have more than one, by item.
    hunks: HashMap<String, Vec<Hunk>>,
    /// Marks that reviewers saved in their reviews of the PR, latest
    /// first.
    saved: Vec<model::SavedMarks>,
    /// The unit that a verdict was just given to without moving on (see
    /// [`set_stayed`]), with the links to where to go next.
    stayed: Option<(String, String)>,
    /// For each item whose only change is names that refer to other items
    /// (see [`retarget_key`]), the other items with the same change.
    same: HashMap<String, Vec<String>>,
}

/// One separate change in an item's or a file's diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hunk {
    /// `item#hash of its changed lines` (for a file's box, of what the box
    /// shows).
    pub id: String,
    /// Line numbers of its added lines, in the PR's version of the file.
    pub new: Vec<usize>,
    /// Line numbers of its removed lines, in the base version.
    pub old: Vec<usize>,
    /// Whether it starts with an added line (else with a removed one).
    pub starts_added: bool,
    /// For a file: what it is, e.g. "section Usage" or "line 40", and
    /// which part of the file's view shows it (see
    /// `files::review_hunks`).
    pub name: String,
    pub key: String,
}

impl Hunk {
    /// Its first changed line (0 if it has none, as a file's box whose
    /// lines the file's line diff places elsewhere).
    pub fn first_line(&self) -> usize {
        let (first, other) = if self.starts_added {
            (&self.new, &self.old)
        } else {
            (&self.old, &self.new)
        };
        first.first().or(other.first()).copied().unwrap_or(0)
    }
}

/// Unchanged lines that separate two hunks: as many as the source view
/// folds between two changes (three lines of context on each side, and at
/// least two folded lines).
const HUNK_GAP: usize = 8;

/// The hunks of a changed item, if it has more than one.
pub fn hunks(it: &Item) -> Vec<Hunk> {
    let out = changes(it);
    if out.len() < 2 {
        return Vec::new();
    }
    out
}

/// The separate changes of a changed item, also when it has only one.
/// Reformatted lines are no part of them.
pub fn changes(it: &Item) -> Vec<Hunk> {
    if it.status != Status::Changed {
        return Vec::new();
    }
    let (o0, n0) = (it.old_line.unwrap_or(it.info.line), it.info.line);
    let (mut o, mut n) = (o0, n0);
    // Lines that were reformatted are not changes to review, whether or
    // not the source view hides them, except for the new lines with added
    // or removed tokens (removed ones are shown there).
    let rf = |side_old: bool, line: usize| {
        it.reformatted.iter().any(|g| {
            let (start, len, i) = if side_old {
                (g.old, g.old_len, line - o0)
            } else {
                (g.new, g.new_len, line - n0)
            };
            (start as usize..(start + len) as usize).contains(&i)
                && (side_old || !g.changes_new_line(i as u32))
        })
    };
    // Per hunk: its changed lines as text, and the hunk.
    let mut out: Vec<(String, Hunk)> = Vec::new();
    // Unchanged lines since the last change; a hunk starts after enough.
    let mut run = usize::MAX;
    for l in &it.source {
        let added = match l.0 {
            '+' if rf(false, n) => {
                n += 1;
                continue;
            }
            '-' if rf(true, o) => {
                o += 1;
                continue;
            }
            '+' => true,
            '-' => false,
            _ => {
                o += 1;
                n += 1;
                run = run.saturating_add(1);
                continue;
            }
        };
        if run >= HUNK_GAP {
            out.push((
                String::new(),
                Hunk {
                    starts_added: added,
                    ..Default::default()
                },
            ));
        }
        run = 0;
        let (text, h) = out.last_mut().expect("a hunk was started");
        text.push(l.0);
        text.push_str(&l.1);
        text.push('\n');
        if added {
            h.new.push(n);
            n += 1;
        } else {
            h.old.push(o);
            o += 1;
        }
    }
    with_ids(&it.info.id, out)
}

/// Gives each hunk its id: `unit#hash of its text`, with the repeats of
/// one change in a unit numbered.
pub fn with_ids(unit: &str, list: Vec<(String, Hunk)>) -> Vec<Hunk> {
    let mut seen = HashSet::new();
    list.into_iter()
        .map(|(text, mut h)| {
            let base = format!("{unit}#{:016x}", hash(&text));
            let mut id = base.clone();
            let mut k = 1;
            while !seen.insert(id.clone()) {
                k += 1;
                id = format!("{base}-{k}");
            }
            h.id = id;
            h
        })
        .collect()
}

/// The item (or file unit) a hunk belongs to. The hash comes after the
/// last `#`, so a `#` in a file's path is no problem.
pub fn hunk_item(id: &str) -> Option<String> {
    let (item, _) = id.rsplit_once('#')?;
    REVIEW.with(|r| {
        r.borrow()
            .hunks
            .contains_key(item)
            .then(|| item.to_string())
    })
}

/// The hunks of an item (none unless it has more than one).
pub fn hunks_of(item: &str) -> Vec<Hunk> {
    REVIEW.with(|r| r.borrow().hunks.get(item).cloned().unwrap_or_default())
}

/// The next hunk of the same item after `after` that has no verdict,
/// wrapping around.
pub fn next_open_hunk(after: &str) -> Option<String> {
    let list = hunks_of(&hunk_item(after)?);
    let i = list.iter().position(|h| h.id == after)?;
    (1..list.len())
        .map(|k| &list[(i + k) % list.len()])
        .find(|h| mark(&h.id) != Some(Mark::Done))
        .map(|h| h.id.clone())
}

/// The verdict that an item's hunks add up to: changes if any needs
/// them, else a comment if any has one, else an approval.
pub fn hunks_verdict(item: &str) -> Verdict {
    let verdicts: Vec<Verdict> = hunks_of(item).iter().map(|h| note(&h.id).verdict).collect();
    if verdicts.contains(&Verdict::Changes) {
        Verdict::Changes
    } else if verdicts.contains(&Verdict::Comment) {
        Verdict::Comment
    } else {
        Verdict::Approve
    }
}

/// The review controls of the hunks of `item` that start on one of the
/// given lines (removed lines by their base line number, added lines by
/// their new one): what goes at the top of one hunk of the source view.
pub fn hunk_controls(item: &str, old: &[usize], new: &[usize]) -> String {
    hunks_of(item)
        .iter()
        .filter(|h| {
            if h.starts_added {
                new.contains(&h.new[0])
            } else {
                old.contains(&h.old[0])
            }
        })
        .map(|h| {
            let all = hunks_of(item);
            let k = all.iter().position(|o| o.id == h.id).map_or(0, |i| i + 1);
            format!(
                r#"<span class="hunk-at">Change {k} of {} <span class="dim">· line {}</span></span>{}"#,
                all.len(),
                h.first_line(),
                control_html(&h.id)
            )
        })
        .collect()
}

/// Sets a file's hunks, for tests of what shows them.
#[cfg(test)]
pub fn set_hunks_for_test(path: &str, list: Vec<Hunk>) {
    REVIEW.with(|r| r.borrow_mut().hunks.insert(file_unit(path), list));
}

/// The hunk of a file that a part of its view shows (by the hunk's
/// `key`), if the file is reviewed hunk by hunk.
pub fn file_hunk(path: &str, key: &str) -> Option<Hunk> {
    hunks_of(&file_unit(path))
        .into_iter()
        .find(|h| h.key == key)
}

/// The review controls at the top of a file's hunk: which change of how
/// many it is, what it is, and the verdict buttons.
pub fn file_hunk_controls(path: &str, h: &Hunk) -> String {
    let all = hunks_of(&file_unit(path));
    let k = all.iter().position(|o| o.id == h.id).map_or(0, |i| i + 1);
    format!(
        r#"<span class="hunk-at">Change {k} of {} <span class="dim">· {}</span></span>{}"#,
        all.len(),
        esc(&h.name),
        control_html(&h.id)
    )
}

thread_local! {
    static REVIEW: RefCell<Review> = RefCell::new(Review::default());
}

// The keys keep the tool's old name, so that marks saved before the rename
// still load.
const MODE_KEY: &str = "rust-pr-explorer.review-mode";
const MARKS_KEY: &str = "rust-pr-explorer.reviewed";
const VERSIONS_KEY: &str = "rust-pr-explorer.reviewed-versions";
const NOTES_KEY: &str = "rust-pr-explorer.review-notes";
const EDGES_KEY: &str = "rust-pr-explorer.reviewed-changes";

/// The unit id of a changed file.
pub fn file_unit(path: &str) -> String {
    format!("file:{path}")
}

/// Whether an item gets its own mark: everything added, changed or
/// removed except modules, whose items are marked one by one.
pub fn is_unit_item(it: &Item) -> bool {
    changed(it) && it.info.kind != Kind::Mod
}

/// Files that get a mark: the ones the overview lists under "Changed
/// files".
pub fn is_unit_file(f: &ChangedFile) -> bool {
    !f.analyzed || f.outside_items > 0
}

/// Hash of what a reviewer looks at for an item: its signature, docs and
/// source with their old versions and marks, and what the names in its
/// signature refer to. Not its position, so a moved line count doesn't
/// count as a change.
fn item_print(it: &Item) -> u64 {
    let parts = (
        it.status.css(),
        &it.info.kind,
        &it.info.sig,
        &it.info.sig_refs,
        &it.info.docs,
        &it.old_sig,
        &it.old_docs,
        &it.source,
        &it.changes,
    );
    hash(&serde_json::to_string(&parts).unwrap_or_default())
}

fn file_print(f: &ChangedFile) -> u64 {
    let parts = (
        &f.status,
        &f.lang,
        &f.hunks,
        f.too_large,
        &f.old_text,
        &f.new_text,
    );
    hash(&serde_json::to_string(&parts).unwrap_or_default())
}

/// Which pull request (or local project) the page is about: marks of one
/// PR apply to every page generated for it, also after new commits, and
/// also to the pages of the changes since an earlier commit.
fn identity(d: &Data) -> String {
    let meta = &d.out.meta;
    match (&meta.pr, &meta.project) {
        (Some(pr), _) => format!("{}#{}", pr.repo, pr.number),
        (None, Some(project)) => format!("local:{project}"),
        // Pages from before projects were identified.
        (None, None) => meta.url.clone().unwrap_or_else(|| meta.title.clone()),
    }
}

/// A side of a unit that doesn't exist (an added unit's base).
const ABSENT: &str = "-";

/// Hash of one side of an item: its signature, docs and source lines on
/// that side. The same version of an item gets the same hash on every page,
/// whatever it is compared with.
fn item_side(it: &Item, head: bool) -> String {
    let (skip, sig, docs) = if head {
        ('-', &it.info.sig, &it.info.docs)
    } else {
        (
            '+',
            it.old_sig.as_ref().unwrap_or(&it.info.sig),
            it.old_docs.as_ref().unwrap_or(&it.info.docs),
        )
    };
    let lines: Vec<&str> = it
        .source
        .iter()
        .filter(|l| l.0 != skip)
        .map(|l| l.1.as_str())
        .collect();
    let parts = (&it.info.kind, sig, docs, lines);
    format!(
        "{:016x}",
        hash(&serde_json::to_string(&parts).unwrap_or_default())
    )
}

/// The versions a page compares a unit between: (base, head), each a hash
/// of that side's content, [`ABSENT`] for a side without the unit. `None`
/// when they are unknown (a page from an analysis before files had
/// hashes).
fn version_pair(d: &Data, unit: &str) -> Option<(String, String)> {
    if let Some(path) = unit.strip_prefix("file:") {
        let f = d.out.files.iter().find(|f| f.path == path)?;
        let side = |h: &String, present: bool| match (h.is_empty(), present) {
            (false, _) => Some(h.clone()),
            (true, false) => Some(ABSENT.to_string()),
            (true, true) => None,
        };
        return Some((
            side(&f.old_hash, f.status != Status::Added)?,
            side(&f.new_hash, f.status != Status::Removed)?,
        ));
    }
    let it = d.item(unit)?;
    let base = match it.status {
        Status::Added => ABSENT.to_string(),
        _ => item_side(it, false),
    };
    let head = match it.status {
        Status::Removed => ABSENT.to_string(),
        _ => item_side(it, true),
    };
    Some((base, head))
}

/// Every unit in reading order.
fn units_in_order(d: &Data) -> Vec<(String, u64)> {
    let mut by_module: HashMap<&str, Vec<&Item>> = HashMap::new();
    for it in d.items().filter(|it| is_unit_item(it)) {
        by_module.entry(d.mod_of(it)).or_default().push(it);
    }
    // Modules as the sidebar lists them.
    fn walk<'a>(d: &'a Data, m: &'a str, out: &mut Vec<&'a str>) {
        out.push(m);
        let mut subs: Vec<&Item> = d
            .mod_children_items(m)
            .into_iter()
            .filter(|c| c.info.kind == Kind::Mod)
            .collect();
        subs.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        for s in subs {
            walk(d, &s.info.id, out);
        }
    }
    let mut modules = Vec::new();
    for r in d.roots() {
        walk(d, r, &mut modules);
    }
    // Modules the tree doesn't reach (removed ones, for example) last.
    let mut rest: Vec<&str> = by_module
        .keys()
        .copied()
        .filter(|m| !modules.contains(m))
        .collect();
    rest.sort();
    modules.extend(rest);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for m in modules {
        if !seen.insert(m) {
            continue;
        }
        if let Some(list) = by_module.get_mut(m) {
            list.sort_by(|a, b| d.reading_order(a, b));
            out.extend(list.iter().map(|it| (it.info.id.clone(), item_print(it))));
        }
    }
    // In the order the overview's "Changed files" shows them.
    let mut files: Vec<&ChangedFile> = d.out.files.iter().filter(|f| is_unit_file(f)).collect();
    files.sort_by_key(|f| crate::files::file_rank(f));
    out.extend(files.iter().map(|f| (file_unit(&f.path), file_print(f))));
    out
}

/// The units in review order, for going through a change from the outside
/// in: each changed item that no other changed item calls or uses, in
/// reading order, and after each one the changed functions it calls,
/// depth-first, in the order of their first call in its body. A call
/// through unchanged items counts as a call too (`parse` calls the
/// unchanged `parse_expr`, which calls the changed `parse_factor`:
/// `parse_factor` comes after `parse`). An item that is reached again is
/// not repeated.
///
/// Types (and constants, statics, …) come after the functions: the changed
/// types that the functions reached from a starting item use (or construct,
/// as with `Wrapper(x)`) follow once all of those functions came, in the
/// order they were first used. A type is followed by its impls (and what a
/// trait impl contains) that no changed item calls, in reading order; what
/// those call follows them as above. So a private struct that a public
/// function's helpers build comes after the public function and its
/// helpers, not before them. Methods that no changed item calls are
/// starting items like free functions; those of one type start together,
/// so that the type comes after all of them.
///
/// Items that only call or use each other (a cycle no other changed item
/// reaches) start from the first of them in reading order. Files stay
/// last. Calls and uses of removed items don't count: a removed function no
/// longer calls anything, so the items it called are reached from their
/// callers in the PR's version.
fn call_order(d: &Data, units: &[(String, u64)]) -> Vec<String> {
    let index: HashMap<&str, usize> = units
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.as_str(), i))
        .collect();
    let item = |i: usize| d.item(&units[i].0);
    let is_impl = |i: usize| item(i).is_some_and(|it| it.info.kind == Kind::Impl);
    let callable = |i: usize| {
        item(i).is_some_and(|it| {
            matches!(
                it.info.kind,
                Kind::Fn | Kind::Method | Kind::TraitMethod | Kind::Macro
            )
        })
    };
    // Changed items that each unit calls or uses, in the order of their
    // first mention.
    let refs: Vec<Vec<usize>> = units
        .iter()
        .map(|(id, _)| {
            let me = index[id.as_str()];
            let mut list = Vec::new();
            // Unchanged items passed through, so that each is expanded once.
            let mut through = HashSet::new();
            let mut stack: Vec<&str> = refs_in_order(d, id);
            stack.reverse();
            while let Some(to) = stack.pop() {
                match index.get(to) {
                    Some(&i) if i != me && !list.contains(&i) => list.push(i),
                    Some(_) => {}
                    None if through.insert(to) => {
                        stack.extend(calls_in_order(d, to).into_iter().rev());
                    }
                    None => {}
                }
            }
            list
        })
        .collect();
    // The changed type, impl or trait that a unit belongs to.
    let owner = |i: usize| -> Option<usize> {
        let info = &item(i)?.info;
        [&info.parent, &info.self_ty]
            .into_iter()
            .flatten()
            .find_map(|o| index.get(o.as_str()).copied())
            .filter(|&o| o != i)
    };
    let mut called = vec![false; units.len()];
    for (i, list) in refs.iter().enumerate() {
        for &c in list.iter().filter(|&&c| callable(c)) {
            called[c] |= c != i;
        }
    }
    // Impls, and what trait impls contain, follow their type (or impl),
    // unless a changed item calls them. Methods that nothing calls are
    // where a review can start, like free functions.
    let attached: Vec<Option<usize>> = (0..units.len())
        .map(|i| owner(i).filter(|&o| !called[i] && !(callable(i) && !is_impl(o))))
        .collect();
    // What comes right after a unit (depth-first), and what waits until
    // the search from its starting item is done.
    let mut next: Vec<Vec<usize>> = vec![Vec::new(); units.len()];
    let mut later: Vec<Vec<usize>> = vec![Vec::new(); units.len()];
    for (i, list) in refs.iter().enumerate() {
        // An impl doesn't wait for the type it belongs to.
        let mine = |t: usize| attached[i].is_some_and(|o| o == t || owner(o) == Some(t));
        for &t in list {
            if callable(t) {
                next[i].push(t);
            } else if !mine(t) {
                later[i].push(t);
            }
        }
    }
    for (i, a) in attached.iter().enumerate() {
        if let Some(o) = *a {
            next[o].push(i);
        }
    }
    let mut reached = vec![false; units.len()];
    for (i, t) in
        (0..units.len()).flat_map(|i| next[i].iter().chain(&later[i]).map(move |&t| (i, t)))
    {
        reached[t] |= t != i;
    }
    let mut seen = vec![false; units.len()];
    let mut out = Vec::with_capacity(units.len());
    // From each of `starts`, what comes next, depth-first (preorder; the
    // stack holds it in reverse so that the first call is visited first),
    // then what waits, the same way.
    let mut visit = |starts: &[usize]| {
        let mut waiting: std::collections::VecDeque<usize> = starts.iter().copied().collect();
        let mut stack = Vec::new();
        while let Some(from) = waiting.pop_front() {
            stack.push(from);
            while let Some(i) = stack.pop() {
                if std::mem::replace(&mut seen[i], true) {
                    continue;
                }
                out.push(units[i].0.clone());
                waiting.extend(later[i].iter().filter(|&&t| !seen[t]));
                stack.extend(next[i].iter().rev().filter(|&&c| !seen[c]));
            }
        }
    };
    let is_file = |i: usize| units[i].0.starts_with("file:");
    // Methods of one type that nothing calls start together, so that the
    // type waits for all of them. Reading order keeps them next to each
    // other.
    let roots: Vec<usize> = (0..units.len())
        .filter(|&i| !reached[i] && !is_file(i))
        .collect();
    for group in roots.chunk_by(|&a, &b| owner(a).is_some() && owner(a) == owner(b)) {
        visit(group);
    }
    for i in (0..units.len()).filter(|&i| !is_file(i)) {
        visit(&[i]);
    }
    for i in (0..units.len()).filter(|&i| is_file(i)) {
        visit(&[i]);
    }
    out
}

/// What an item calls, in the order of the first call in its body (as
/// far as the positions of names tell; else by id). Nothing for removed
/// items.
fn calls_in_order<'a>(d: &'a Data, id: &str) -> Vec<&'a str> {
    let Some(it) = d.item(id).filter(|it| it.status != Status::Removed) else {
        return Vec::new();
    };
    by_first_mention(it, it.info.calls.iter().map(|c| c.to.as_str()).collect())
}

/// What an item calls or uses (not counting an impl's use of its type),
/// in the order of the first mention in its source. Nothing for removed
/// items.
fn refs_in_order<'a>(d: &'a Data, id: &str) -> Vec<&'a str> {
    let Some(it) = d.item(id).filter(|it| it.status != Status::Removed) else {
        return Vec::new();
    };
    let calls = it.info.calls.iter().map(|c| c.to.as_str());
    let uses = it
        .info
        .uses
        .iter()
        .filter(|u| u.ctx != UseCtx::Impl)
        .map(|u| u.to.as_str());
    let mut list: Vec<&str> = calls.chain(uses).collect();
    list.sort();
    list.dedup();
    by_first_mention(it, list)
}

/// Sorts `list` by where `it`'s source first names each (as far as the
/// positions of names tell; else by id).
fn by_first_mention<'a>(it: &'a Item, mut list: Vec<&'a str>) -> Vec<&'a str> {
    let mut first: HashMap<&str, (usize, usize)> = HashMap::new();
    for (at, r) in &it.info.source_refs {
        let Some(to) = r.id.as_deref() else {
            continue;
        };
        let (l, c) = at.split_once(':').unwrap_or((at, "0"));
        let pos = (l.parse().unwrap_or(usize::MAX), c.parse().unwrap_or(0));
        let e = first.entry(to).or_insert(pos);
        *e = (*e).min(pos);
    }
    list.sort_by_key(|to| (first.get(to).copied().unwrap_or((usize::MAX, 0)), *to));
    list
}

/// The hunks of the units, and the prints of units and hunks. A hunk's id
/// says what it changes, so its print is that.
fn hunks_and_prints(
    d: &Data,
    units: &[(String, u64)],
) -> (HashMap<String, Vec<Hunk>>, HashMap<String, u64>) {
    let mut prints: HashMap<String, u64> = units.iter().cloned().collect();
    let mut all = HashMap::new();
    for (id, _) in units {
        let list = match id.strip_prefix("file:") {
            Some(path) => d
                .out
                .files
                .iter()
                .find(|f| f.path == path)
                .map(crate::files::review_hunks)
                .unwrap_or_default(),
            None => d.item(id).map(hunks).unwrap_or_default(),
        };
        if !list.is_empty() {
            prints.extend(list.iter().map(|h| (h.id.clone(), hash(&h.id))));
            all.insert(id.clone(), list);
        }
    }
    (all, prints)
}

fn storage() -> Option<web_sys::Storage> {
    dom::window().local_storage().ok().flatten()
}

/// Encodes reviewed changes as lines of `f|s from to id` (`f` for the
/// page of the whole PR).
fn encode_edges(edges: &BTreeMap<String, Vec<Edge>>) -> String {
    edges
        .iter()
        .flat_map(|(id, list)| {
            list.iter().map(move |e| {
                let page = if e.full { 'f' } else { 's' };
                format!("{page} {} {} {id}\n", e.from, e.to)
            })
        })
        .collect()
}

fn decode_edges(s: &str) -> BTreeMap<String, Vec<Edge>> {
    let mut out: BTreeMap<String, Vec<Edge>> = BTreeMap::new();
    for l in s.lines() {
        let mut parts = l.splitn(4, ' ');
        let (Some(page), Some(from), Some(to), Some(id)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let full = match page {
            "f" => true,
            "s" => false,
            _ => continue,
        };
        out.entry(id.to_string()).or_default().push(Edge {
            from: from.to_string(),
            to: to.to_string(),
            full,
        });
    }
    out
}

/// Whether the reviewed changes of a unit cover what a page shows of it:
/// they lead from the version it compares with to the one it shows. A
/// review of the whole PR also covers every page of the changes since an
/// earlier commit that ends at the same version.
fn covered(edges: &[Edge], (from, to): (&str, &str), since: bool) -> bool {
    if since && edges.iter().any(|e| e.full && e.to == to) {
        return true;
    }
    // At least one reviewed change: a unit can change in ways the versions
    // leave out (e.g. only its position), so `from` can equal `to`.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut todo = vec![from];
    while let Some(v) = todo.pop() {
        for e in edges.iter().filter(|e| e.from == v) {
            if e.to == to {
                return true;
            }
            if seen.insert(&e.to) {
                todo.push(&e.to);
            }
        }
    }
    false
}

/// Encodes marks as lines of `hash id`.
fn encode(marks: &BTreeMap<String, u64>) -> String {
    marks
        .iter()
        .map(|(id, h)| format!("{h:016x} {id}\n"))
        .collect()
}

fn decode(s: &str) -> BTreeMap<String, u64> {
    s.lines()
        .filter_map(|l| {
            let (h, id) = l.split_once(' ')?;
            Some((id.to_string(), u64::from_str_radix(h, 16).ok()?))
        })
        .collect()
}

/// The marks saved for a PR.
#[derive(Default)]
struct Stored {
    marks: BTreeMap<String, u64>,
    edges: BTreeMap<String, Vec<Edge>>,
    versions: BTreeMap<String, Snapshot>,
    notes: BTreeMap<String, Note>,
}

/// Reads the marks saved under a PR's key.
fn read_stored(key: &str) -> Stored {
    let store = storage();
    let get = |prefix: &str| {
        store
            .as_ref()
            .and_then(|s| s.get_item(&format!("{prefix}:{key}")).ok().flatten())
    };
    let marks = get(MARKS_KEY).map(|s| decode(&s)).unwrap_or_default();
    let edges = get(EDGES_KEY).map(|s| decode_edges(&s)).unwrap_or_default();
    let marked = |id: &String| marks.contains_key(id) || edges.contains_key(id);
    let mut versions: BTreeMap<String, Snapshot> = get(VERSIONS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    versions.retain(|id, _| marked(id));
    let mut notes: BTreeMap<String, Note> = get(NOTES_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    notes.retain(|id, _| marked(id));
    Stored {
        marks,
        edges,
        versions,
        notes,
    }
}

/// Reads the saved marks again, before changing them: the other page of
/// the PR, open in another tab, may have saved marks since this page read
/// them, and saving this page's copy would lose those.
fn reread() {
    let key = REVIEW.with(|r| r.borrow().key.clone());
    let s = read_stored(&key);
    REVIEW.with(|r| {
        let mut r = r.borrow_mut();
        r.marks = s.marks;
        r.edges = s.edges;
        r.versions = s.versions;
        r.notes = s.notes;
    });
}

/// Whether a key of the browser's storage that changed holds marks of this
/// page's PR. `None` (the whole storage was cleared) may have.
pub fn is_marks_key(key: Option<&str>) -> bool {
    let Some(key) = key else {
        return true;
    };
    let own = REVIEW.with(|r| r.borrow().key.clone());
    key.split_once(':').is_some_and(|(prefix, k)| {
        k == own && [MARKS_KEY, EDGES_KEY, NOTES_KEY, VERSIONS_KEY].contains(&prefix)
    })
}

/// Another tab (of this page or of the PR's other page) saved marks: reads
/// them and updates the controls of the units whose mark or verdict
/// changed. Returns whether any did.
pub fn sync_from_storage() -> bool {
    let ids: Vec<String> = REVIEW.with(|r| r.borrow().prints.keys().cloned().collect());
    let state = |id: &String| (mark(id), note(id));
    let before: Vec<_> = ids.iter().map(state).collect();
    reread();
    let changed: Vec<&String> = ids
        .iter()
        .zip(before)
        .filter(|(id, b)| state(id) != *b)
        .map(|(id, _)| id)
        .collect();
    for id in &changed {
        refresh(id);
    }
    !changed.is_empty()
}

/// Reads the saved marks for this page's PR.
pub fn load(d: &Data) {
    let units = units_in_order(d);
    let key = identity(d);
    let Stored {
        marks,
        edges,
        versions,
        notes,
    } = read_stored(&key);
    // On unless the reader turned it off before.
    let on = storage()
        .and_then(|s| s.get_item(MODE_KEY).ok().flatten())
        .is_none_or(|v| v != "off");
    let (hunks, prints) = hunks_and_prints(d, &units);
    let pairs = units
        .iter()
        .filter_map(|(id, _)| Some((id.clone(), version_pair(d, id)?)))
        .collect();
    let order = call_order(d, &units);
    let since = d.out.meta.pr.as_ref().is_some_and(|pr| pr.since.is_some());
    let same = same_changes(
        units
            .iter()
            .filter_map(|(id, _)| d.item(id))
            .filter(|it| is_unit_item(it)),
    );
    crate::line_notes::load(&key);
    REVIEW.with(|r| {
        *r.borrow_mut() = Review {
            on,
            key,
            since,
            prints,
            units: units.into_iter().map(|(id, _)| id).collect(),
            order,
            marks,
            pairs,
            edges,
            versions,
            notes,
            unfolded: HashSet::new(),
            hunks,
            saved: d
                .out
                .meta
                .pr
                .as_ref()
                .map(|pr| pr.saved_marks.clone())
                .unwrap_or_default(),
            stayed: None,
            same,
        }
    });
    apply_mode();
}

/// What an item's change is, if it is only names that now refer to other
/// items (see `Item::retargeted`): each name's old and new target, sorted
/// and without repeats. `None` if anything else changed, such as a line's
/// text, the signature, the docs or its `unsafe` code.
pub fn retarget_key(it: &Item) -> Option<Vec<(String, String)>> {
    let c = &it.changes;
    if it.status != Status::Changed
        || it.retargeted.is_empty()
        || c.signature
        || c.docs
        || c.cosmetic
        || c.moved_from.is_some()
        || !c.exports_added.is_empty()
        || !c.exports_removed.is_empty()
        || !it.unsafe_code.is_empty()
    {
        return None;
    }
    // Every changed line is a retargeted one, shown removed and added.
    let lines = |tag: char| it.source.iter().filter(|l| l.0 == tag).count();
    let (old, new): (HashSet<u32>, HashSet<u32>) =
        it.retargeted.iter().map(|r| (r.old_line, r.line)).unzip();
    if lines('-') != old.len() || lines('+') != new.len() {
        return None;
    }
    let path = |r: &Option<model::Ref>| r.as_ref().map_or(String::new(), |r| r.path.clone());
    let mut key: Vec<(String, String)> = it
        .retargeted
        .iter()
        .map(|r| (path(&r.old), path(&r.new)))
        .collect();
    key.sort();
    key.dedup();
    Some(key)
}

/// Groups the items whose only change is the same set of retargeted
/// names, by item: each with the others of its group.
fn same_changes<'a>(items: impl IntoIterator<Item = &'a Item>) -> HashMap<String, Vec<String>> {
    let mut groups: BTreeMap<Vec<(String, String)>, Vec<String>> = BTreeMap::new();
    for it in items {
        if let Some(key) = retarget_key(it) {
            groups.entry(key).or_default().push(it.info.id.clone());
        }
    }
    let mut out = HashMap::new();
    for ids in groups.into_values().filter(|g| g.len() > 1) {
        for id in &ids {
            let others = ids.iter().filter(|o| *o != id).cloned().collect();
            out.insert(id.clone(), others);
        }
    }
    out
}

/// The other items with the same change as `id` (see [`same_changes`])
/// that still need a review.
pub fn open_same_change(id: &str) -> Vec<String> {
    let all = REVIEW.with(|r| r.borrow().same.get(id).cloned().unwrap_or_default());
    all.into_iter()
        .filter(|o| mark(o) != Some(Mark::Done))
        .collect()
}

/// The marks of a PR as one text, to take them to another page: in a
/// posted review, or copied and pasted.
#[derive(Serialize, Deserialize)]
struct Payload {
    v: u32,
    /// The PR's storage key, so that pasted marks don't go to another PR.
    /// Not in a review, which belongs to its PR.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    marks: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    edges: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    notes: BTreeMap<String, Note>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    versions: BTreeMap<String, Snapshot>,
    /// Comments on lines (see `line_notes`); only in copied marks, since a
    /// posted review has them as comments.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    lines: BTreeMap<String, crate::line_notes::LineNote>,
}

/// Whether anything of this PR is marked, on this page or not.
pub fn has_marks() -> bool {
    REVIEW.with(|r| {
        let r = r.borrow();
        !r.marks.is_empty() || !r.edges.is_empty()
    })
}

/// Where an [`export`] goes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Export {
    /// Into a posted review, as readable lines (see [`review_lines`]).
    Review,
    /// To copy and paste on another page: everything, as JSON.
    Copy,
}

/// The saved marks of this PR, `None` when there are none.
pub fn export(to: Export) -> Option<String> {
    reread();
    REVIEW.with(|r| {
        let r = r.borrow();
        if r.marks.is_empty() && r.edges.is_empty() {
            return None;
        }
        match to {
            Export::Copy => serde_json::to_string(&Payload {
                v: 1,
                key: r.key.clone(),
                marks: encode(&r.marks),
                edges: encode_edges(&r.edges),
                notes: r.notes.clone(),
                versions: r.versions.clone(),
                lines: crate::line_notes::all(),
            })
            .ok(),
            Export::Review => Some(review_lines(&r.marks, &r.edges, &r.notes)),
        }
    })
}

/// The marks for a posted review, one per line:
///
/// - `mark HASH ID`: a unit or hunk, with the hash of the diff it had.
/// - `change full|since FROM TO ID`: a reviewed change of a unit, on the
///   page of the whole PR or of the changes since an earlier commit (see
///   [`Edge`]); `-` for a side without the unit.
/// - `verdict changes|comment ID`: a verdict other than an approval.
///
/// Without what the review itself holds: the PR, and the comments (which a
/// later review must not post again). Without the reviewed versions (for
/// **Show changes**) too: they are the whole source of every reviewed item
/// and would make the command that posts the review long.
fn review_lines(
    marks: &BTreeMap<String, u64>,
    edges: &BTreeMap<String, Vec<Edge>>,
    notes: &BTreeMap<String, Note>,
) -> String {
    let mut out = Vec::new();
    for (id, h) in marks {
        out.push(format!("mark {h:016x} {id}"));
    }
    for (id, list) in edges {
        for e in list {
            let page = if e.full { "full" } else { "since" };
            out.push(format!("change {page} {} {} {id}", e.from, e.to));
        }
    }
    for (id, n) in notes.iter().filter(|(_, n)| n.verdict != Verdict::Approve) {
        out.push(format!("verdict {} {id}", n.verdict.name()));
    }
    // An id with a line break would break the format.
    out.retain(|l| !l.contains(['\n', '\r']));
    out.join("\n")
}

/// Reads the lines of [`review_lines`]; unknown lines are skipped.
fn parse_review_lines(text: &str) -> Payload {
    let mut p = Payload {
        v: 1,
        key: String::new(),
        marks: String::new(),
        edges: String::new(),
        notes: BTreeMap::new(),
        versions: BTreeMap::new(),
        lines: BTreeMap::new(),
    };
    for l in text.lines().map(str::trim) {
        let Some((kind, rest)) = l.split_once(' ') else {
            continue;
        };
        match kind {
            "mark" => {
                p.marks.push_str(rest);
                p.marks.push('\n');
            }
            "change" => {
                let Some((page, rest)) = rest.split_once(' ') else {
                    continue;
                };
                let page = match page {
                    "full" => 'f',
                    "since" => 's',
                    _ => continue,
                };
                p.edges.push_str(&format!("{page} {rest}\n"));
            }
            "verdict" => {
                let Some((v, id)) = rest.split_once(' ') else {
                    continue;
                };
                if let Some(verdict) = Verdict::from_name(v) {
                    p.notes.insert(
                        id.to_string(),
                        Note {
                            verdict,
                            text: String::new(),
                        },
                    );
                }
            }
            _ => {}
        }
    }
    p
}

/// Adds the marks of a posted review (see [`review_lines`]) like
/// [`import`].
pub fn import_review(text: &str) -> Result<usize, String> {
    merge(parse_review_lines(text))
}

/// Adds the marks of an [`export`] to the saved ones, where this page has
/// none of its own for the same unit, and saves them. Returns how many
/// units and hunks got a mark they didn't have. Call [`load`] afterwards
/// to show them.
pub fn import(json: &str) -> Result<usize, String> {
    let p: Payload =
        serde_json::from_str(json).map_err(|_| "These aren't review marks.".to_string())?;
    if p.v != 1 {
        return Err("These marks are from a newer version of Rust Semantic Review.".to_string());
    }
    merge(p)
}

fn merge(mut p: Payload) -> Result<usize, String> {
    reread();
    let added = REVIEW.with(|r| {
        let mut r = r.borrow_mut();
        if !p.key.is_empty() && p.key != r.key {
            return Err(format!(
                "These marks are for {}, not for this page's {}.",
                p.key, r.key
            ));
        }
        let before: HashSet<String> = r.marks.keys().chain(r.edges.keys()).cloned().collect();
        for (id, h) in decode(&p.marks) {
            r.marks.entry(id).or_insert(h);
        }
        for (id, list) in decode_edges(&p.edges) {
            let mine = r.edges.entry(id).or_default();
            for e in list {
                if !mine.contains(&e) {
                    mine.push(e);
                }
            }
        }
        for (id, n) in p.notes {
            r.notes.entry(id).or_insert(n);
        }
        for (id, v) in p.versions {
            r.versions.entry(id).or_insert(v);
        }
        crate::line_notes::merge(std::mem::take(&mut p.lines));
        let after: HashSet<&String> = r.marks.keys().chain(r.edges.keys()).collect();
        Ok(after.iter().filter(|id| !before.contains(**id)).count())
    })?;
    save();
    Ok(added)
}

/// Marks that reviewers saved in their reviews of the PR.
pub fn saved_marks() -> Vec<model::SavedMarks> {
    REVIEW.with(|r| r.borrow().saved.clone())
}

fn save() {
    let (key, marks, edges, versions, notes) = REVIEW.with(|r| {
        let r = r.borrow();
        let versions = if r.versions.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&r.versions).unwrap_or_default()
        };
        let notes = if r.notes.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&r.notes).unwrap_or_default()
        };
        (
            r.key.clone(),
            encode(&r.marks),
            encode_edges(&r.edges),
            versions,
            notes,
        )
    });
    let put = |s: &web_sys::Storage, prefix: &str, text: &str| {
        let k = format!("{prefix}:{key}");
        if text.is_empty() {
            s.remove_item(&k).is_ok()
        } else {
            s.set_item(&k, text).is_ok()
        }
    };
    let Some(store) = storage().filter(|s| put(s, MARKS_KEY, &marks)) else {
        crate::toast(
            "This browser doesn't allow saving review marks: they are lost when the page is closed.",
        );
        return;
    };
    if !put(&store, EDGES_KEY, &edges) {
        crate::toast(
            "The browser's storage is full: the marks of the whole PR are saved, but not which changes you reviewed, so the PR's other page can't use them.",
        );
    }
    if !put(&store, NOTES_KEY, &notes) {
        crate::toast(
            "The browser's storage is full: your verdicts and comments are lost when the page is closed.",
        );
    }
    if !put(&store, VERSIONS_KEY, &versions) {
        crate::toast(
            "The browser's storage is full: the marks are saved, but not the reviewed versions, so a later page can't show what changed since.",
        );
    }
}

pub fn is_on() -> bool {
    REVIEW.with(|r| r.borrow().on)
}

pub fn set_on(on: bool) {
    REVIEW.with(|r| r.borrow_mut().on = on);
    if let Some(s) = storage() {
        let _ = s.set_item(MODE_KEY, if on { "on" } else { "off" });
    }
    apply_mode();
}

/// Shows or hides the controls (a class on the body) and the progress.
fn apply_mode() {
    let on = is_on();
    if let Some(body) = dom::document().body() {
        dom::toggle_class(&body, "reviewing", on);
    }
    if let Some(b) = dom::by_id("rv-mode") {
        dom::toggle_class(&b, "on", on);
    }
    update_progress();
}

/// The mark of a unit, `None` for things that can't be marked.
pub fn mark(id: &str) -> Option<Mark> {
    REVIEW.with(|r| {
        let r = r.borrow();
        let now = r.prints.get(id)?;
        let edges = r.edges.get(id).map(Vec::as_slice).unwrap_or_default();
        let pair = r.pairs.get(id);
        let done = r.marks.get(id) == Some(now)
            || pair.is_some_and(|(f, t)| covered(edges, (f, t), r.since));
        Some(if done {
            Mark::Done
        } else if r.marks.contains_key(id) || !edges.is_empty() {
            Mark::Stale
        } else {
            Mark::Open
        })
    })
}

/// The verdict and comment of a marked unit (a plain approval if it has
/// none), also of a stale mark.
pub fn note(id: &str) -> Note {
    REVIEW.with(|r| r.borrow().notes.get(id).cloned().unwrap_or_default())
}

/// Every marked unit's mark and note, in reading order.
pub fn notes() -> Vec<(String, Mark, Note)> {
    let units = REVIEW.with(|r| r.borrow().units.clone());
    units
        .into_iter()
        .flat_map(|u| {
            let hunks = hunks_of(&u).into_iter().map(|h| h.id);
            std::iter::once(u).chain(hunks)
        })
        .filter_map(|u| {
            let m = mark(&u)?;
            (m != Mark::Open).then(|| {
                let n = note(&u);
                (u, m, n)
            })
        })
        .collect()
}

/// Marks a unit as reviewed in its current state with a verdict, or
/// removes the mark (`None`).
pub fn set_mark(d: &Data, id: &str, note: Option<Note>) {
    let done = note.is_some();
    let version = done
        .then(|| crate::since::snapshot(d, id))
        .flatten()
        .map(|mut s| {
            s.at = js_sys::Date::now();
            s
        });
    reread();
    let changed = REVIEW.with(|r| {
        let mut r = r.borrow_mut();
        let Some(&now) = r.prints.get(id) else {
            return false;
        };
        let pair = r.pairs.get(id).cloned();
        let is_hunk = r.hunks.values().flatten().any(|h| h.id == id);
        if done {
            // The other page's diff of an item has another hash: its mark
            // is only the reviewed change.
            if !r.since || is_hunk {
                r.marks.insert(id.to_string(), now);
            }
            if let Some((from, to)) = pair {
                let e = Edge {
                    from,
                    to,
                    full: !r.since,
                };
                let list = r.edges.entry(id.to_string()).or_default();
                if !list.contains(&e) {
                    list.push(e);
                }
            }
        } else {
            // Every review that ends at the version shown, so that the
            // unit is open on both pages.
            r.marks.remove(id);
            if let Some((_, to)) = pair
                && let Some(list) = r.edges.get_mut(id)
            {
                list.retain(|e| e.to != to);
                if list.is_empty() {
                    r.edges.remove(id);
                }
            }
            r.unfolded.remove(id);
        }
        match note.filter(|n| *n != Note::default()) {
            Some(n) => r.notes.insert(id.to_string(), n),
            None => r.notes.remove(id),
        };
        match version {
            Some(v) => r.versions.insert(id.to_string(), v),
            None => r.versions.remove(id),
        };
        true
    });
    if changed {
        save();
        refresh(id);
    }
}

/// Approves the other items with the same change as `id` (see
/// [`open_same_change`]), without a comment: one on `id` stays there,
/// rather than being repeated once per item in a posted review.
pub fn mark_same_change(d: &Data, id: &str) {
    for other in open_same_change(id) {
        set_mark(d, &other, Some(Note::default()));
    }
}

/// The storage key of this PR's marks: which pull request (or local
/// project) the page is about.
pub fn key() -> String {
    REVIEW.with(|r| r.borrow().key.clone())
}

/// Removes every mark of this PR.
pub fn clear() {
    REVIEW.with(|r| {
        let mut r = r.borrow_mut();
        r.marks.clear();
        r.edges.clear();
        r.versions.clear();
        r.notes.clear();
        r.unfolded.clear();
    });
    save();
    crate::line_notes::clear();
    crate::drafts::clear();
}

/// Where a unit is in reading order (after every unit if it isn't one).
pub fn unit_index(id: &str) -> usize {
    REVIEW.with(|r| {
        let r = r.borrow();
        r.units
            .iter()
            .position(|u| u == id)
            .unwrap_or(r.units.len())
    })
}

/// The version a unit had when it was marked, if it was saved.
pub fn reviewed_version(id: &str) -> Option<Snapshot> {
    REVIEW.with(|r| r.borrow().versions.get(id).cloned())
}

/// (reviewed, total) units.
pub fn progress() -> (usize, usize) {
    let units = REVIEW.with(|r| r.borrow().units.clone());
    let done = units.iter().filter(|u| mark(u) == Some(Mark::Done)).count();
    (done, units.len())
}

/// Units of the given list that are not reviewed (or changed since).
pub fn open_count<'a>(ids: impl IntoIterator<Item = &'a str>) -> usize {
    ids.into_iter()
        .filter(|id| matches!(mark(id), Some(Mark::Open | Mark::Stale)))
        .count()
}

/// The first unit after `after` (or from the start) in review order (see
/// [`call_order`]) that still needs a review, wrapping around.
pub fn next_open(after: Option<&str>) -> Option<String> {
    next_open_where(after, |_| true)
}

/// Like [`next_open`], among the units that `keep` accepts.
pub fn next_open_where(after: Option<&str>, keep: impl Fn(&str) -> bool) -> Option<String> {
    let units = REVIEW.with(|r| r.borrow().order.clone());
    let start = after
        .and_then(|a| units.iter().position(|u| u == a))
        .map_or(0, |i| i + 1);
    let n = units.len();
    (0..n)
        .map(|k| &units[(start + k) % n])
        .find(|u| keep(u) && mark(u) != Some(Mark::Done))
        .cloned()
}

/// The unit before `of` in review order, reviewed or not: where `k` goes.
/// None at the start of the order, or for a unit that isn't in it.
pub fn prev_unit(of: &str) -> Option<String> {
    REVIEW.with(|r| {
        let r = r.borrow();
        let i = r.order.iter().position(|u| u == of)?;
        i.checked_sub(1).map(|i| r.order[i].clone())
    })
}

/// The first item that the source of item `of` tags as new or changed
/// (see `code::ref_tag`) and that still needs a review, in the order the
/// source names them: where `Enter` goes in review mode.
pub fn next_open_call(d: &Data, of: &str) -> Option<String> {
    let it = d.item(of)?;
    let mut refs: Vec<((usize, usize), &str)> = it
        .info
        .source_refs
        .iter()
        .filter_map(|(at, r)| {
            let (l, c) = at.split_once(':')?;
            Some(((l.parse().ok()?, c.parse().ok()?), r.id.as_deref()?))
        })
        .collect();
    refs.sort();
    refs.into_iter()
        .map(|(_, id)| id)
        .find(|id| crate::code::ref_tag(d, id, of).is_some() && mark(id) != Some(Mark::Done))
        .map(str::to_string)
}

/// Shows `next`, links to where to go next, in the control of the unit
/// that was just reviewed without moving on, until another unit gets one
/// or the review moves on (`None`).
pub fn set_stayed(stayed: Option<(String, String)>) {
    let old = REVIEW.with(|r| std::mem::replace(&mut r.borrow_mut().stayed, stayed.clone()));
    for id in [old.map(|(id, _)| id), stayed.map(|(id, _)| id)]
        .into_iter()
        .flatten()
    {
        refresh(&id);
    }
}

/// The unit that [`set_stayed`] last stayed on.
pub fn stayed() -> Option<String> {
    REVIEW.with(|r| r.borrow().stayed.as_ref().map(|(id, _)| id.clone()))
}

/// Whether a reviewed unit is shown whole again.
pub fn is_unfolded(id: &str) -> bool {
    REVIEW.with(|r| r.borrow().unfolded.contains(id))
}

/// Shows a reviewed unit whole again, or folds it back to one line.
pub fn toggle_unfolded(id: &str) {
    REVIEW.with(|r| {
        let mut r = r.borrow_mut();
        if !r.unfolded.remove(id) {
            r.unfolded.insert(id.to_string());
        }
    });
    refresh(id);
}

/// The mark and its buttons for a unit, or nothing for things that can't
/// be marked. Outside review mode, only an existing mark is shown.
pub fn control_html(id: &str) -> String {
    let Some(m) = mark(id) else {
        return String::new();
    };
    format!(
        r#"<span class="{}" data-rv="{}">{}</span>"#,
        control_class(id, m),
        esc(id),
        control_inner(id, m)
    )
}

fn control_class(id: &str, m: Mark) -> String {
    let unfolded = REVIEW.with(|r| r.borrow().unfolded.contains(id));
    format!(
        "rvc {}{}",
        match m {
            Mark::Open => "open",
            Mark::Done => "done",
            Mark::Stale => "stale",
        },
        if unfolded { " unfold" } else { "" }
    )
}

/// The verdict buttons, as on GitHub. Approving needs no comment; the
/// other two ask for a comment first.
const ACTIONS: &str = r#"<button class="rv-btn approve" data-rv-act="approve" title="Approve, and go on to the next item if Next ▸ led here, except on the canvas (a)">✓ Approve</button><button class="rv-btn changes" data-rv-act="changes" title="Say what to change (r)">✗ Request changes</button><button class="rv-btn comment" data-rv-act="comment" title="Add a comment (c)">✎ Comment</button>"#;

/// One line of a comment, shortened, for the button that edits it.
fn snippet(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let short: String = line.trim().chars().take(36).collect();
    if short.len() < line.trim().len() || text.trim().lines().count() > 1 {
        format!("{short}…")
    } else {
        short
    }
}

/// The verdict buttons of a unit, labelled by what they apply to: one
/// change of an item (a hunk), the whole item when it has several, or
/// just the unit.
fn actions(id: &str) -> String {
    if hunk_item(id).is_some() {
        return r#"<button class="rv-btn approve" data-rv-act="approve" title="Approve only this change (a, after n/p)">✓ Approve change</button><button class="rv-btn changes" data-rv-act="changes" title="Say what to change here (r, after n/p)">✗ Request changes</button><button class="rv-btn comment" data-rv-act="comment" title="Comment on this change (c, after n/p)">✎ Comment</button>"#.to_string();
    }
    let same = open_same_change(id).len();
    if same > 0 {
        let others = if same == 1 {
            "1 other item".to_string()
        } else {
            format!("{same} other items")
        };
        return format!(
            r#"<span class="rv-same" title="Their only change is the same: the same names now refer to the same other items.">Same change as {others}.</span><button class="rv-btn approve" data-rv-act="approve" data-rv-all title="Approve this item and the {same} others, and go on to the next item if Next ▸ led here, except on the canvas (a)">✓ Approve all {all}</button><button class="rv-btn approve" data-rv-act="approve" title="Approve only this item">✓ Only this</button><button class="rv-btn changes" data-rv-act="changes" title="Say what to change in this item (r)">✗ Request changes</button><button class="rv-btn comment" data-rv-act="comment" title="Comment on this item (c)">✎ Comment</button>"#,
            all = same + 1
        );
    }
    let n = hunks_of(id).len();
    if n > 1 {
        let what = if id.starts_with("file:") {
            "file"
        } else {
            "item"
        };
        return format!(
            r#"<span class="rv-scope" title="These buttons give the whole {what} one verdict. Each change also has its own buttons.">Whole {what} ({n} changes):</span><button class="rv-btn approve" data-rv-act="approve" title="Approve the whole {what}, and go on to the next one if Next ▸ led here, except on the canvas (a)">✓ Approve {what}</button><button class="rv-btn changes" data-rv-act="changes" title="Say what to change in this {what} (r)">✗ Request changes</button><button class="rv-btn comment" data-rv-act="comment" title="Comment on the whole {what} (c)">✎ Comment</button>"#
        );
    }
    ACTIONS.to_string()
}

fn control_inner(id: &str, m: Mark) -> String {
    let n = note(id);
    let text = if n.text.trim().is_empty() {
        String::new()
    } else {
        format!(
            r#"<button class="rv-note" data-rv-edit title="{}">“{}”</button>"#,
            esc(&n.text),
            esc(&snippet(&n.text))
        )
    };
    match m {
        Mark::Open => actions(id),
        Mark::Done => {
            let (class, label, what) = match n.verdict {
                Verdict::Approve => ("approved", "✓ Approved", "Approved"),
                Verdict::Changes => ("changes", "✗ Changes requested", "You requested changes"),
                Verdict::Comment => ("commented", "✎ Commented", "You commented"),
            };
            let (unfolded, next) = REVIEW.with(|r| {
                let r = r.borrow();
                let next = match &r.stayed {
                    Some((s, next)) if s == id => next.clone(),
                    _ => String::new(),
                };
                (r.unfolded.contains(id), next)
            });
            format!(
                r#"<button class="rv-chip {class}" data-rv-undo title="{what}. Click to remove the mark">{label}</button>{text}<button class="rv-edit" data-rv-edit title="Change the verdict or the comment">Edit</button><button class="rv-fold" data-rv-fold title="{}">{}</button>{next}"#,
                if unfolded {
                    "Fold the item back to one line"
                } else {
                    "Show the whole item again"
                },
                if unfolded { "▴ Fold" } else { "▾ Expand" }
            )
        }
        Mark::Stale => format!(
            r#"<button class="rv-chip stale" data-rv-undo title="Changed since you reviewed it. Click to unmark.">⚠ Changed since review</button><button class="rv-since" data-rv-since title="Diff to the reviewed version">Show changes</button>{text}{}"#,
            actions(id)
        ),
    }
}

/// Updates every control of a unit on the page, and the progress, without
/// rendering the views again (which would lose the scroll position).
fn refresh(id: &str) {
    let Some(m) = mark(id) else {
        return;
    };
    let (class, inner) = (control_class(id, m), control_inner(id, m));
    for el in dom::query_all(".rvc") {
        if dom::data(&el, "rv").as_deref() == Some(id) {
            el.set_class_name(&class);
            el.set_inner_html(&inner);
        }
    }
    // References to the unit in source views get or lose their ✓.
    for el in dom::query_all("a.ref.new, a.ref.chg") {
        if dom::data(&el, "id").as_deref() == Some(id) {
            dom::toggle_class(&el, "rv-ok", m == Mark::Done);
        }
    }
    update_progress();
}

/// An item's name in a list of changes (the overview's internal changes),
/// with a ✓ in review mode once the item is reviewed.
pub fn item_tag(id: &str, name: &str) -> String {
    format!(
        r#"<span class="rv-it{}" data-rv-it="{}">{}</span>"#,
        if mark(id) == Some(Mark::Done) {
            " done"
        } else {
            ""
        },
        esc(id),
        esc(name)
    )
}

/// The review state of a group of items, like in the sidebar: a ✓ once
/// all are reviewed, else how many are left.
pub fn group_html<'a>(ids: impl IntoIterator<Item = &'a str>) -> String {
    let ids: Vec<&str> = ids.into_iter().collect();
    match open_count(ids.iter().copied()) {
        0 => r#"<span class="rv-all" title="All reviewed">✓ reviewed</span>"#.to_string(),
        left => format!(
            r#"<span class="rv-left" title="{left} of {} not reviewed yet">{left} not reviewed</span>"#,
            ids.len()
        ),
    }
}

/// Updates the item names and group states of the lists of changes.
fn update_groups() {
    for el in dom::query_all(".rv-it") {
        let done = dom::data(&el, "rv-it").and_then(|id| mark(&id)) == Some(Mark::Done);
        dom::toggle_class(&el, "done", done);
    }
    for el in dom::query_all(".rv-group") {
        let Some(row) = dom::closest(&el, ".irow") else {
            continue;
        };
        let ids: Vec<String> = dom::query_all_in(&row, "[data-rv-it]")
            .iter()
            .filter_map(|it| dom::data(it, "rv-it"))
            .collect();
        el.set_inner_html(&group_html(ids.iter().map(String::as_str)));
    }
}

/// Leaves review mode; the marks stay.
const EXIT: &str = r#"<button id="rv-keys" title="Keyboard shortcuts (?)">?</button><button id="rv-exit" title="Hide the review controls; your marks stay">Exit<span class="btn-label"> review</span></button>"#;

/// A button that loads the marks saved in a review, while this page has
/// no marks of the PR: what a page of a later Rust Semantic Review comment starts with,
/// since it can't see the marks of the earlier one's page.
fn load_offer() -> String {
    if has_marks() {
        return String::new();
    }
    let saved = saved_marks();
    match saved.as_slice() {
        [] => String::new(),
        [one] => format!(
            r#"<button id="rv-load" class="primary" title="Load the marks you saved with your review of {}">Load marks<span class="btn-label"> from {}'s review</span></button>"#,
            esc(&one.at),
            esc(&one.login)
        ),
        _ => r#"<button id="rv-load" class="primary" title="Load the marks saved with a review of this PR">Load marks<span class="btn-label"> from a review</span>…</button>"#.to_string(),
    }
}

/// The progress and the review actions in the header: "Next" while
/// something is left, then "Finish review…" as the main action. "Clear"
/// and "Exit review" sit apart.
pub fn update_progress() {
    update_groups();
    let (done, total) = progress();
    let html = match (done * 100).checked_div(total) {
        None => format!(r#"<span class="dim">Nothing to review</span>{EXIT}"#),
        Some(percent) => {
            let all = done == total;
            let count = if all {
                format!("✓ All {total} reviewed")
            } else {
                format!("{done} / {total} reviewed")
            };
            let next = if all {
                String::new()
            } else {
                r#"<button id="rv-next" title="Go to the next unreviewed item (j)">Next ▸</button>"#
                    .to_string()
            };
            format!(
                r#"<span class="rv-progress{}" title="Reviewed items and files"><span class="rv-count">{count}</span><span class="rv-meter"><span style="width:{percent}%"></span></span></span>{next}<button id="rv-finish" class="{}" title="Collect your verdicts and comments for GitHub">Finish<span class="btn-label"> review</span>…</button>{load}<button id="rv-marks" title="Take your marks to another page, or clear them">Marks…</button>{EXIT}"#,
                if all { " all" } else { "" },
                if all { "primary" } else { "" },
                load = load_offer(),
            )
        }
    };
    dom::set_html("rv-bar", &html);
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Meta, Output, Status};

    fn item(id: &str, kind: Kind, module: &str, file: &str, line: usize, status: Status) -> Item {
        let mut it = Item {
            status,
            ..Default::default()
        };
        it.info.id = id.into();
        it.info.name = id.rsplit("::").next().unwrap().into();
        it.info.kind = kind;
        it.info.module = module.into();
        it.info.file = file.into();
        it.info.line = line;
        it.info.sig = format!("fn {id}()");
        it
    }

    /// A test whose only change is that `helper` now calls `to`.
    fn retargeted(id: &str, to: &str) -> Item {
        let mut it = item(id, Kind::Fn, "c::tests", "src/lib.rs", 10, Status::Changed);
        it.changes.body = true;
        it.source = vec![
            model::SourceLine(' ', "fn t() {".into()),
            model::SourceLine('-', "    helper();".into()),
            model::SourceLine('+', "    helper();".into()),
            model::SourceLine(' ', "}".into()),
        ];
        let r = |p: &str| model::Ref {
            id: Some(p.into()),
            path: p.into(),
        };
        it.retargeted = vec![model::Retarget {
            old_line: 1,
            line: 1,
            col: 4,
            name: "helper".into(),
            old: Some(r("c::helper")),
            new: Some(r(to)),
        }];
        it
    }

    #[test]
    fn items_with_the_same_retarget_only_are_one_group() {
        let a = retargeted("c::tests::a", "c::tests::helper");
        let b = retargeted("c::tests::b", "c::tests::helper");
        // Calls another new helper: another change.
        let other = retargeted("c::tests::c", "c::tests::other");
        // The same retarget, plus a changed line.
        let mut more = retargeted("c::tests::d", "c::tests::helper");
        more.source
            .insert(3, model::SourceLine('+', "    extra();".into()));
        // The same retarget, plus new docs.
        let mut docs = retargeted("c::tests::e", "c::tests::helper");
        docs.changes.docs = true;
        assert!(retarget_key(&more).is_none());
        assert!(retarget_key(&docs).is_none());
        let same = same_changes([&a, &b, &other, &more, &docs]);
        assert_eq!(same["c::tests::a"], ["c::tests::b"]);
        assert_eq!(same["c::tests::b"], ["c::tests::a"]);
        assert_eq!(same.len(), 2);
        // The buttons say that the verdict applies to the other one too,
        // until it is reviewed.
        REVIEW.with(|r| {
            *r.borrow_mut() = Review {
                same,
                prints: [("c::tests::b".to_string(), 1)].into(),
                ..Default::default()
            }
        });
        assert!(actions("c::tests::a").contains("Same change as 1 other item."));
        // Approving can cover both or only this one.
        let html = actions("c::tests::a");
        assert!(html.contains(r#"data-rv-all title="Approve this item and the 1 others"#));
        assert!(html.contains("✓ Approve all 2</button>"));
        assert!(html.contains("✓ Only this</button>"));
        REVIEW.with(|r| r.borrow_mut().marks.insert("c::tests::b".into(), 1));
        assert!(!actions("c::tests::a").contains("Same change"));
        REVIEW.with(|r| *r.borrow_mut() = Review::default());
    }

    /// Crate `c` with modules `b` and `a` (listed in that order in the
    /// data), changed items out of position order, an unchanged item and
    /// a changed file outside the analysis.
    fn data() -> Data {
        let items = [
            item("c", Kind::Mod, "c", "src/lib.rs", 1, Status::Changed),
            item("c::b", Kind::Mod, "c", "src/b.rs", 1, Status::Changed),
            item("c::a", Kind::Mod, "c", "src/a.rs", 1, Status::Added),
            item(
                "c::root_fn",
                Kind::Fn,
                "c",
                "src/lib.rs",
                9,
                Status::Changed,
            ),
            item(
                "c::b::late",
                Kind::Fn,
                "c::b",
                "src/b.rs",
                20,
                Status::Changed,
            ),
            item(
                "c::b::early",
                Kind::Fn,
                "c::b",
                "src/b.rs",
                3,
                Status::Removed,
            ),
            item(
                "c::a::new_fn",
                Kind::Fn,
                "c::a",
                "src/a.rs",
                2,
                Status::Added,
            ),
            item(
                "c::a::same",
                Kind::Fn,
                "c::a",
                "src/a.rs",
                5,
                Status::Unchanged,
            ),
            // In a module the tree doesn't reach.
            item(
                "c::gone::f",
                Kind::Fn,
                "c::gone",
                "src/gone.rs",
                1,
                Status::Removed,
            ),
        ];
        let file = ChangedFile {
            path: "README.md".into(),
            status: Status::Changed,
            lang: "markdown".into(),
            ..Default::default()
        };
        let covered = ChangedFile {
            path: "src/b.rs".into(),
            status: Status::Changed,
            lang: "rust".into(),
            analyzed: true,
            ..Default::default()
        };
        Data::new(Output {
            meta: Meta::default(),
            crates: vec!["c".into()],
            items: items
                .into_iter()
                .map(|it| (it.info.id.clone(), it))
                .collect(),
            externals: BTreeMap::new(),
            files: vec![file, covered],
            warnings: Vec::new(),
            semver_checks: None,
        })
    }

    #[test]
    fn types_come_before_their_impls_and_methods() {
        // In the file, the impl and its method come first; a free function
        // sits between them and the type.
        let mut imp = item(
            "c::<impl Display for S>",
            Kind::Impl,
            "c",
            "src/lib.rs",
            2,
            Status::Added,
        );
        imp.info.self_ty = Some("c::S".into());
        let mut method = item(
            "c::S::new",
            Kind::Method,
            "c",
            "src/lib.rs",
            8,
            Status::Changed,
        );
        method.info.self_ty = Some("c::S".into());
        let items = [
            item("c", Kind::Mod, "c", "src/lib.rs", 1, Status::Changed),
            imp,
            method,
            item("c::free", Kind::Fn, "c", "src/lib.rs", 12, Status::Changed),
            item("c::S", Kind::Struct, "c", "src/lib.rs", 20, Status::Changed),
            item("c::last", Kind::Fn, "c", "src/lib.rs", 30, Status::Changed),
        ];
        let d = Data::new(Output {
            meta: Meta::default(),
            crates: vec!["c".into()],
            items: items
                .into_iter()
                .map(|it| (it.info.id.clone(), it))
                .collect(),
            externals: BTreeMap::new(),
            files: Vec::new(),
            warnings: Vec::new(),
            semver_checks: None,
        });
        let order: Vec<String> = units_in_order(&d).into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            order,
            [
                "c::free",
                "c::S",
                "c::<impl Display for S>",
                "c::S::new",
                "c::last"
            ]
        );
    }

    fn set(d: &Data, marks: &[(&str, u64)]) {
        let units = units_in_order(d);
        let (hunks, prints) = hunks_and_prints(d, &units);
        let order = call_order(d, &units);
        REVIEW.with(|r| {
            *r.borrow_mut() = Review {
                on: true,
                key: String::new(),
                since: false,
                prints,
                units: units.into_iter().map(|(id, _)| id).collect(),
                order,
                marks: marks.iter().map(|(id, h)| (id.to_string(), *h)).collect(),
                pairs: HashMap::new(),
                edges: BTreeMap::new(),
                versions: BTreeMap::new(),
                notes: BTreeMap::new(),
                unfolded: HashSet::new(),
                hunks,
                saved: Vec::new(),
                stayed: None,
                same: HashMap::new(),
            }
        });
    }

    fn print_of(id: &str) -> u64 {
        REVIEW.with(|r| r.borrow().prints[id])
    }

    #[test]
    fn units_follow_the_sidebar_then_positions_then_files() {
        let d = data();
        let order: Vec<String> = units_in_order(&d).into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            order,
            [
                // The crate root, then its modules by name, items by position.
                "c::root_fn",
                "c::a::new_fn",
                "c::b::early",
                "c::b::late",
                // Modules the tree doesn't reach.
                "c::gone::f",
                // Files outside the analysis; analyzed ones are covered by
                // their items.
                "file:README.md",
            ]
        );
    }

    /// Makes `from` call `to`, first named at `at` (`line:column`).
    fn call(out: &mut Output, from: &str, to: &str, at: &str) {
        let info = &mut out.items.get_mut(from).unwrap().info;
        info.calls.insert(model::Call { to: to.into() });
        info.source_refs.insert(
            at.into(),
            model::Ref {
                id: Some(to.into()),
                path: to.into(),
            },
        );
    }

    fn order_of(d: &Data) -> Vec<String> {
        call_order(d, &units_in_order(d))
    }

    #[test]
    fn review_order_follows_the_calls() {
        let mut out = data().out;
        // `root_fn` calls `new_fn` on line 10, and before that, on line 9,
        // the unchanged `same`, which calls `late` (lines compare as
        // numbers: as text, "10" would come first).
        call(&mut out, "c::root_fn", "c::a::new_fn", "10:30");
        call(&mut out, "c::root_fn", "c::a::same", "9:4");
        call(&mut out, "c::a::same", "c::b::late", "6:4");
        // `late` calls `new_fn` too: it is reached once, under `late`.
        call(&mut out, "c::b::late", "c::a::new_fn", "21:8");
        // Calls of a removed item don't count.
        call(&mut out, "c::b::early", "c::gone::f", "4:4");
        let d = Data::new(out);
        assert_eq!(
            order_of(&d),
            [
                "c::root_fn",
                "c::b::late",
                "c::a::new_fn",
                "c::b::early",
                "c::gone::f",
                "file:README.md",
            ]
        );
        set(&d, &[]);
        assert_eq!(next_open(Some("c::root_fn")).as_deref(), Some("c::b::late"));
        // The summary of **Finish review** keeps reading order.
        let units = REVIEW.with(|r| r.borrow().units.clone());
        assert_eq!(units[1], "c::a::new_fn");
    }

    #[test]
    fn next_on_the_canvas_skips_reviewed_cards() {
        use crate::CanvasNext;
        let mut out = data().out;
        call(&mut out, "c::root_fn", "c::a::new_fn", "10:30");
        let d = Data::new(out);
        set(&d, &[]);
        fn done<'a>(ids: &[&'a str]) -> Vec<(&'a str, u64)> {
            ids.iter().map(|id| (*id, print_of(id))).collect()
        }
        let cards =
            |ids: &[&str]| -> HashSet<String> { ids.iter().map(|id| id.to_string()).collect() };
        let next = |on: &[&str], from: &str| crate::canvas_next(&d, &cards(on), Some(from));
        // Another card still needs a review: Next goes there.
        assert_eq!(
            next(&["c::root_fn", "c::b::late"], "c::root_fn"),
            CanvasNext::Card("c::b::late".into())
        );
        // `root_fn` is approved and the only card. Next must not go back
        // to it, but on to `new_fn`, which it calls.
        set(&d, &done(&["c::root_fn"]));
        assert_eq!(
            next(&["c::root_fn"], "c::root_fn"),
            CanvasNext::Add("c::a::new_fn".into())
        );
        // Once `new_fn` is approved too, the next item isn't connected to
        // the cards: a new canvas.
        set(&d, &done(&["c::root_fn", "c::a::new_fn"]));
        assert_eq!(
            next(&["c::root_fn", "c::a::new_fn"], "c::a::new_fn"),
            CanvasNext::NewCanvas("c::b::early".into())
        );
        let all: Vec<String> = REVIEW.with(|r| r.borrow().order.clone());
        let all: Vec<&str> = all.iter().map(String::as_str).collect();
        set(&d, &done(&all));
        assert_eq!(next(&["c::root_fn"], "c::root_fn"), CanvasNext::Done);
        REVIEW.with(|r| *r.borrow_mut() = Review::default());
    }

    #[test]
    fn types_come_after_the_functions_that_use_them() {
        // In the file, the types and their impls come before `run`, which
        // calls `S::new`, which uses `S` and constructs `Inner`.
        let mut imp = item(
            "c::S::<impl Display for S>",
            Kind::Impl,
            "c",
            "src/lib.rs",
            8,
            Status::Added,
        );
        imp.info.parent = Some("c::S".into());
        imp.info.self_ty = Some("c::S".into());
        imp.info.uses.insert(model::Use {
            to: "c::S".into(),
            ctx: UseCtx::Impl,
        });
        let mut new = item(
            "c::S::new",
            Kind::Method,
            "c",
            "src/lib.rs",
            4,
            Status::Added,
        );
        new.info.parent = Some("c::S".into());
        new.info.self_ty = Some("c::S".into());
        new.info.uses.insert(model::Use {
            to: "c::S".into(),
            ctx: UseCtx::Signature,
        });
        // `-> S`, before the body constructs `Inner`.
        new.info.source_refs.insert(
            "4:20".into(),
            model::Ref {
                id: Some("c::S".into()),
                path: "c::S".into(),
            },
        );
        let mut s = item("c::S", Kind::Struct, "c", "src/lib.rs", 1, Status::Added);
        s.info.uses.insert(model::Use {
            to: "c::Inner".into(),
            ctx: UseCtx::Field,
        });
        let items = [
            item("c", Kind::Mod, "c", "src/lib.rs", 1, Status::Changed),
            s,
            new,
            imp,
            item(
                "c::Inner",
                Kind::Struct,
                "c",
                "src/lib.rs",
                12,
                Status::Added,
            ),
            item("c::run", Kind::Fn, "c", "src/lib.rs", 20, Status::Changed),
        ];
        let mut out = Output {
            meta: Meta::default(),
            crates: vec!["c".into()],
            items: items
                .into_iter()
                .map(|it| (it.info.id.clone(), it))
                .collect(),
            externals: BTreeMap::new(),
            files: Vec::new(),
            warnings: Vec::new(),
            semver_checks: None,
        };
        call(&mut out, "c::run", "c::S::new", "21:4");
        call(&mut out, "c::S::new", "c::Inner", "5:8");
        let d = Data::new(out);
        assert_eq!(
            order_of(&d),
            [
                "c::run",
                "c::S::new",
                // After all the functions, with the impl that nothing calls.
                "c::S",
                "c::S::<impl Display for S>",
                "c::Inner",
            ]
        );
    }

    #[test]
    fn methods_that_nothing_calls_come_before_their_type() {
        // `P::new` and `P::run` are the entry points: nothing calls them.
        let method = |id: &str, line| {
            let mut m = item(id, Kind::Method, "c", "src/lib.rs", line, Status::Added);
            m.info.parent = Some("c::P".into());
            m.info.self_ty = Some("c::P".into());
            m.info.uses.insert(model::Use {
                to: "c::P".into(),
                ctx: UseCtx::Signature,
            });
            m
        };
        let items = [
            item("c", Kind::Mod, "c", "src/lib.rs", 1, Status::Changed),
            item("c::P", Kind::Struct, "c", "src/lib.rs", 1, Status::Added),
            method("c::P::new", 4),
            method("c::P::run", 8),
            item("c::later", Kind::Fn, "c", "src/lib.rs", 20, Status::Added),
        ];
        let d = Data::new(Output {
            meta: Meta::default(),
            crates: vec!["c".into()],
            items: items
                .into_iter()
                .map(|it| (it.info.id.clone(), it))
                .collect(),
            externals: BTreeMap::new(),
            files: Vec::new(),
            warnings: Vec::new(),
            semver_checks: None,
        });
        assert_eq!(order_of(&d), ["c::P::new", "c::P::run", "c::P", "c::later"]);
    }

    #[test]
    fn items_that_only_call_each_other_start_from_the_first() {
        let mut out = data().out;
        call(&mut out, "c::b::late", "c::a::new_fn", "21:8");
        call(&mut out, "c::a::new_fn", "c::b::late", "3:4");
        let d = Data::new(out);
        assert_eq!(
            order_of(&d),
            [
                "c::root_fn",
                "c::b::early",
                "c::gone::f",
                // The cycle, from its first item in reading order.
                "c::a::new_fn",
                "c::b::late",
                "file:README.md",
            ]
        );
    }

    #[test]
    fn next_skips_only_reviewed_units_and_wraps() {
        let d = data();
        set(&d, &[]);
        let done = print_of("c::a::new_fn");
        set(&d, &[("c::a::new_fn", done), ("c::b::early", 1)]);
        assert_eq!(mark("c::a::new_fn"), Some(Mark::Done));
        // Marked with another diff: changed since the review.
        assert_eq!(mark("c::b::early"), Some(Mark::Stale));
        assert_eq!(mark("c::b::late"), Some(Mark::Open));
        // Not a unit: modules and unchanged items.
        assert_eq!(mark("c::a"), None);
        assert_eq!(mark("c::a::same"), None);

        assert_eq!(next_open(None).as_deref(), Some("c::root_fn"));
        // Skips the reviewed one, stops at the stale one.
        assert_eq!(
            next_open(Some("c::root_fn")).as_deref(),
            Some("c::b::early")
        );
        // Wraps around at the end.
        assert_eq!(
            next_open(Some("file:README.md")).as_deref(),
            Some("c::root_fn")
        );
        assert_eq!(progress(), (1, 6));
        assert_eq!(open_count(["c::a::new_fn", "c::b::early", "c::b::late"]), 2);

        // Everything reviewed: nothing next.
        let all: Vec<(String, u64)> = units_in_order(&d);
        let marks: Vec<(&str, u64)> = all.iter().map(|(id, h)| (id.as_str(), *h)).collect();
        set(&d, &marks);
        assert_eq!(next_open(None), None);
        assert_eq!(progress(), (6, 6));
    }

    #[test]
    fn prints_ignore_positions_but_not_diffs() {
        let a = item("c::f", Kind::Fn, "c", "src/lib.rs", 3, Status::Changed);
        let mut moved = a.clone();
        moved.info.line = 40;
        assert_eq!(item_print(&a), item_print(&moved));
        let mut edited = a.clone();
        edited.source.push(model::SourceLine('+', "x += 1;".into()));
        assert_ne!(item_print(&a), item_print(&edited));
    }

    #[test]
    fn local_projects_have_their_own_marks() {
        let mut d = data();
        d.out.meta.title = "main → HEAD".into();
        d.out.meta.project = Some("0123".into());
        let one = identity(&d);
        d.out.meta.project = Some("4567".into());
        assert_ne!(identity(&d), one);
    }

    fn edge(from: &str, to: &str, full: bool) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            full,
        }
    }

    #[test]
    fn reviewed_changes_round_trip() {
        let mut m = BTreeMap::new();
        m.insert(
            "file:docs/a b.md".to_string(),
            vec![edge("-", "00ff", true), edge("00ff", "1234", false)],
        );
        m.insert("t::f".to_string(), vec![edge("aa", "bb", true)]);
        assert_eq!(decode_edges(&encode_edges(&m)), m);
        // Damaged lines are skipped.
        assert_eq!(decode_edges("x a b t::f\nf a\n").len(), 0);
    }

    #[test]
    fn review_lines_round_trip() {
        let mut marks = BTreeMap::new();
        marks.insert("t::f".to_string(), 0x1234);
        marks.insert("file:docs/a b.md".to_string(), u64::MAX);
        marks.insert("bad\nid".to_string(), 1);
        let mut edges = BTreeMap::new();
        edges.insert(
            "t::f".to_string(),
            vec![edge("-", "00ff", true), edge("00ff", "1234", false)],
        );
        let mut notes = BTreeMap::new();
        let note = |verdict, text: &str| Note {
            verdict,
            text: text.into(),
        };
        notes.insert("t::f".to_string(), note(Verdict::Changes, "Rename it."));
        notes.insert("t::g".to_string(), note(Verdict::Approve, ""));
        let text = review_lines(&marks, &edges, &notes);
        assert_eq!(
            text,
            "mark ffffffffffffffff file:docs/a b.md\n\
             mark 0000000000001234 t::f\n\
             change full - 00ff t::f\n\
             change since 00ff 1234 t::f\n\
             verdict changes t::f"
        );
        let p = parse_review_lines(&format!("{text}\nsomething else\n"));
        marks.remove("bad\nid");
        assert_eq!(decode(&p.marks), marks);
        assert_eq!(decode_edges(&p.edges), edges);
        // The verdict, without the comment, which the review has.
        assert_eq!(p.notes.len(), 1);
        assert_eq!(p.notes["t::f"], note(Verdict::Changes, ""));
        assert!(p.key.is_empty());
    }

    #[test]
    fn reviewed_changes_chain_up() {
        // The whole PR reviewed at commit A, then the changes since A.
        let both = [edge("base", "a", true), edge("a", "b", false)];
        assert!(covered(&both, ("base", "b"), false));
        assert!(!covered(&both[..1], ("base", "b"), false));
        assert!(!covered(&both[1..], ("base", "b"), false));
        // The page of the changes since A.
        assert!(covered(&both[1..], ("a", "b"), true));
        // The whole PR reviewed at B covers the changes since any commit.
        let whole = [edge("base", "b", true)];
        assert!(covered(&whole, ("a", "b"), true));
        assert!(!covered(&whole, ("a", "c"), true));
        // Changes since another commit don't.
        assert!(!covered(&[edge("x", "b", false)], ("a", "b"), true));
        // Nor does nothing, also when both versions are the same.
        assert!(!covered(&[], ("a", "a"), false));
    }

    #[test]
    fn an_items_version_is_the_same_on_both_pages() {
        // The whole PR at commit A: `f` changed from base.
        let mut at_a = item("c::f", Kind::Fn, "c", "src/lib.rs", 1, Status::Changed);
        at_a.old_sig = Some("fn f(x: u8)".into());
        at_a.source = vec![
            sl('-', "fn f(x: u8) {"),
            sl('+', "fn c::f() {"),
            sl(' ', "}"),
        ];
        // The changes since A, at B: `f`'s body changed.
        let mut since = item("c::f", Kind::Fn, "c", "src/lib.rs", 1, Status::Changed);
        since.source = vec![sl(' ', "fn c::f() {"), sl('+', "    g();"), sl(' ', "}")];
        // The whole PR at B.
        let mut at_b = since.clone();
        at_b.old_sig = at_a.old_sig.clone();
        at_b.source = vec![
            sl('-', "fn f(x: u8) {"),
            sl('+', "fn c::f() {"),
            sl('+', "    g();"),
            sl(' ', "}"),
        ];
        assert_eq!(item_side(&at_a, true), item_side(&since, false));
        assert_eq!(item_side(&since, true), item_side(&at_b, true));
        assert_eq!(item_side(&at_a, false), item_side(&at_b, false));
        assert_ne!(item_side(&at_a, true), item_side(&at_b, true));
    }

    #[test]
    fn marks_round_trip() {
        let mut m = BTreeMap::new();
        m.insert("t::f".to_string(), 0x1234);
        m.insert("file:docs/a b.md".to_string(), u64::MAX);
        assert_eq!(decode(&encode(&m)), m);
        // Damaged lines are skipped.
        assert_eq!(decode("zz t::f\n\n").len(), 0);
    }

    fn sl(tag: char, text: &str) -> model::SourceLine {
        model::SourceLine(tag, text.into())
    }

    /// A changed function at line 10 (12 in the base) with a change at
    /// its start and one 9 unchanged lines further down.
    fn two_hunks() -> Item {
        let mut it = item("c::f", Kind::Fn, "c", "src/lib.rs", 10, Status::Changed);
        it.old_line = Some(12);
        it.source = vec![
            sl(' ', "fn f() {"),
            sl('-', "    a();"),
            sl('+', "    a(1);"),
        ];
        it.source
            .extend((0..9).map(|i| sl(' ', &format!("    x{i}();"))));
        it.source.extend([sl('-', "    b();"), sl(' ', "}")]);
        it
    }

    #[test]
    fn hunks_split_where_the_view_folds() {
        let it = two_hunks();
        let h = hunks(&it);
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].old.clone(), h[0].new.clone()), (vec![13], vec![11]));
        assert!(!h[0].starts_added);
        // 12 + 1 + 1 + 9 lines further in the base; nothing added.
        assert_eq!((h[1].old.clone(), h[1].new.clone()), (vec![23], vec![]));
        assert_eq!(h[1].first_line(), 23);
        // Ids say what changed, not where.
        let mut moved = it.clone();
        moved.info.line = 50;
        moved.old_line = Some(60);
        assert_eq!(
            hunks(&moved).iter().map(|h| &h.id).collect::<Vec<_>>(),
            h.iter().map(|h| &h.id).collect::<Vec<_>>()
        );
        // 7 unchanged lines apart, which the view doesn't fold, the two
        // changes are one hunk: no hunks then.
        let mut close = it.clone();
        close.source.drain(5..7);
        assert!(hunks(&close).is_empty());
        // Added and removed items have no hunks.
        let mut added = it;
        added.status = Status::Added;
        assert!(hunks(&added).is_empty());
    }

    #[test]
    fn hunks_have_their_own_marks() {
        let mut d = data();
        d.out.items.insert("c::root_fn".into(), {
            let mut it = two_hunks();
            it.info.id = "c::root_fn".into();
            it.info.module = "c".into();
            it
        });
        let d = Data::new(d.out);
        set(&d, &[]);
        let h = hunks_of("c::root_fn");
        assert_eq!(h.len(), 2);
        assert_eq!(mark(&h[0].id), Some(Mark::Open));
        assert_eq!(hunk_item(&h[1].id).as_deref(), Some("c::root_fn"));
        assert_eq!(hunk_item("c::root_fn"), None);
        let first = print_of(&h[0].id);
        set(&d, &[(&h[0].id, first)]);
        assert_eq!(mark(&h[0].id), Some(Mark::Done));
        // The next hunk without a verdict, wrapping around.
        assert_eq!(next_open_hunk(&h[0].id).as_deref(), Some(h[1].id.as_str()));
        assert_eq!(next_open_hunk(&h[1].id), None);
        // Hunks don't count as units of their own.
        assert_eq!(progress(), (0, 6));
        // Each hunk's controls go at its first changed line.
        assert!(hunk_controls("c::root_fn", &[13], &[]).contains(&esc(&h[0].id)));
        assert!(!hunk_controls("c::root_fn", &[13], &[]).contains(&esc(&h[1].id)));
        assert!(hunk_controls("c::root_fn", &[23], &[11]).contains(&esc(&h[1].id)));
    }

    #[test]
    fn references_to_reviewed_items_get_a_check() {
        let mut out = data().out;
        let f = out.items.get_mut("c::root_fn").unwrap();
        f.changes.body = true;
        f.source = vec![sl('+', "    late();"), sl('+', "    new_fn();")];
        for (at, to) in [("9:4", "c::b::late"), ("10:4", "c::a::new_fn")] {
            f.info.source_refs.insert(
                at.into(),
                model::Ref {
                    id: Some(to.into()),
                    path: to.into(),
                },
            );
        }
        let d = Data::new(out);
        let late = print_of_in(&d, "c::b::late");
        set(&d, &[("c::b::late", late)]);
        let html = d.source_html(d.item("c::root_fn").unwrap(), Some(true), false);
        assert!(
            html.contains(r#"<a class="ref chg rv-ok" data-id="c::b::late""#),
            "{html}"
        );
        assert!(
            html.contains(r#"<a class="ref new" data-id="c::a::new_fn""#),
            "{html}"
        );
    }

    #[test]
    fn enter_follows_the_first_unreviewed_tagged_call() {
        let mut out = data().out;
        let f = out.items.get_mut("c::root_fn").unwrap();
        // In source order: `late` (changed), `same` (unchanged), `new_fn`
        // (added), and `root_fn` itself. Positions compare as numbers.
        for (at, to) in [
            ("10:4", "c::a::new_fn"),
            ("9:30", "c::a::same"),
            ("9:12", "c::root_fn"),
            ("9:4", "c::b::late"),
        ] {
            f.info.source_refs.insert(
                at.into(),
                model::Ref {
                    id: Some(to.into()),
                    path: to.into(),
                },
            );
        }
        let d = Data::new(out);
        set(&d, &[]);
        assert_eq!(
            next_open_call(&d, "c::root_fn").as_deref(),
            Some("c::b::late")
        );
        let late = print_of("c::b::late");
        set(&d, &[("c::b::late", late)]);
        assert_eq!(
            next_open_call(&d, "c::root_fn").as_deref(),
            Some("c::a::new_fn")
        );
        let new_fn = print_of("c::a::new_fn");
        set(&d, &[("c::b::late", late), ("c::a::new_fn", new_fn)]);
        assert_eq!(next_open_call(&d, "c::root_fn"), None);
    }

    fn print_of_in(d: &Data, id: &str) -> u64 {
        set(d, &[]);
        print_of(id)
    }

    #[test]
    fn hash_is_stable() {
        // FNV-1a test vectors.
        assert_eq!(hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
