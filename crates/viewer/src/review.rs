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
//! Marks are kept in the browser's local storage, per pull request (or per
//! pair of compared trees), when the browser allows that.

use crate::code::esc;
use crate::data::{Data, changed};
use crate::dom;
use crate::since::Snapshot;
use model::stable_hash as hash;
use model::{ChangedFile, Item, Kind, Status};
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

#[derive(Default)]
struct Review {
    /// Review mode: the controls for marking are shown.
    on: bool,
    /// Storage key of this PR's marks.
    key: String,
    /// Everything that can be marked, in reading order: Rust items by
    /// module (as the sidebar lists them) and position, then other files.
    units: Vec<String>,
    /// Hash of each unit's diff.
    prints: HashMap<String, u64>,
    /// Saved marks: unit → hash of its diff when it was marked.
    marks: BTreeMap<String, u64>,
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
}

/// One separate change in an item's diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// `item#hash of its changed lines`.
    pub id: String,
    /// Line numbers of its added lines, in the PR's version of the file.
    pub new: Vec<usize>,
    /// Line numbers of its removed lines, in the base version.
    pub old: Vec<usize>,
    /// Whether it starts with an added line (else with a removed one).
    pub starts_added: bool,
}

impl Hunk {
    /// Its first changed line.
    pub fn first_line(&self) -> usize {
        if self.starts_added {
            self.new[0]
        } else {
            self.old[0]
        }
    }
}

/// Unchanged lines that separate two hunks: as many as the source view
/// folds between two changes (three lines of context on each side, and at
/// least two folded lines).
const HUNK_GAP: usize = 8;

/// The hunks of a changed item, if it has more than one.
pub fn hunks(it: &Item) -> Vec<Hunk> {
    if it.status != Status::Changed {
        return Vec::new();
    }
    let (mut o, mut n) = (it.old_line.unwrap_or(it.info.line), it.info.line);
    // Per hunk: its changed lines as text, and the hunk.
    let mut out: Vec<(String, Hunk)> = Vec::new();
    // Unchanged lines since the last change; a hunk starts after enough.
    let mut run = usize::MAX;
    for l in &it.source {
        let added = match l.0 {
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
                    id: String::new(),
                    new: Vec::new(),
                    old: Vec::new(),
                    starts_added: added,
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
    if out.len() < 2 {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    out.into_iter()
        .map(|(text, mut h)| {
            let base = format!("{}#{:016x}", it.info.id, hash(&text));
            // The same change twice in one item: number the repeats.
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

/// The item a hunk belongs to.
pub fn hunk_item(id: &str) -> Option<String> {
    let (item, _) = id.split_once('#')?;
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
            format!(
                r#"<span class="hunk-at">Change at line {}</span>{}"#,
                h.first_line(),
                control_html(&h.id)
            )
        })
        .collect()
}

thread_local! {
    static REVIEW: RefCell<Review> = RefCell::new(Review::default());
}

const MODE_KEY: &str = "rust-pr-explorer.review-mode";
const MARKS_KEY: &str = "rust-pr-explorer.reviewed";
const VERSIONS_KEY: &str = "rust-pr-explorer.reviewed-versions";
const NOTES_KEY: &str = "rust-pr-explorer.review-notes";

/// The unit id of a changed file.
pub fn file_unit(path: &str) -> String {
    format!("file:{path}")
}

/// Whether an item gets its own mark: everything added, changed or
/// removed except modules, whose items are marked one by one.
pub fn is_unit_item(it: &Item) -> bool {
    changed(it) && it.info.kind != Kind::Mod
}

/// Files that get a mark: the ones the overview lists under "Other
/// changed files".
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
/// PR apply to every page generated for it, also after new commits.
fn identity(d: &Data) -> String {
    let meta = &d.out.meta;
    match (&meta.pr, &meta.project) {
        (Some(pr), _) => format!("{}#{}", pr.repo, pr.number),
        (None, Some(project)) => format!("local:{project}"),
        // Pages from before projects were identified.
        (None, None) => meta.url.clone().unwrap_or_else(|| meta.title.clone()),
    }
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
            list.sort_by(|a, b| {
                (&a.info.file, a.info.line, &a.info.id).cmp(&(
                    &b.info.file,
                    b.info.line,
                    &b.info.id,
                ))
            });
            out.extend(list.iter().map(|it| (it.info.id.clone(), item_print(it))));
        }
    }
    out.extend(
        d.out
            .files
            .iter()
            .filter(|f| is_unit_file(f))
            .map(|f| (file_unit(&f.path), file_print(f))),
    );
    out
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
        let list = d.item(id).map(hunks).unwrap_or_default();
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

/// Reads the saved marks for this page's PR.
pub fn load(d: &Data) {
    let units = units_in_order(d);
    let key = identity(d);
    let store = storage();
    let get = |prefix: &str| {
        store
            .as_ref()
            .and_then(|s| s.get_item(&format!("{prefix}:{key}")).ok().flatten())
    };
    let marks = get(MARKS_KEY).map(|s| decode(&s)).unwrap_or_default();
    let mut versions: BTreeMap<String, Snapshot> = get(VERSIONS_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    versions.retain(|id, _| marks.contains_key(id));
    let mut notes: BTreeMap<String, Note> = get(NOTES_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    notes.retain(|id, _| marks.contains_key(id));
    let on = store
        .and_then(|s| s.get_item(MODE_KEY).ok().flatten())
        .is_some_and(|v| v == "on");
    let (hunks, prints) = hunks_and_prints(d, &units);
    REVIEW.with(|r| {
        *r.borrow_mut() = Review {
            on,
            key,
            prints,
            units: units.into_iter().map(|(id, _)| id).collect(),
            marks,
            versions,
            notes,
            unfolded: HashSet::new(),
            hunks,
        }
    });
    apply_mode();
}

fn save() {
    let (key, marks, versions, notes) = REVIEW.with(|r| {
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
        (r.key.clone(), encode(&r.marks), versions, notes)
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
        Some(match r.marks.get(id) {
            None => Mark::Open,
            Some(h) if h == now => Mark::Done,
            Some(_) => Mark::Stale,
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
    let changed = REVIEW.with(|r| {
        let mut r = r.borrow_mut();
        let Some(&now) = r.prints.get(id) else {
            return false;
        };
        if done {
            r.marks.insert(id.to_string(), now);
        } else {
            r.marks.remove(id);
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

/// Removes every mark of this PR.
pub fn clear() {
    REVIEW.with(|r| {
        let mut r = r.borrow_mut();
        r.marks.clear();
        r.versions.clear();
        r.notes.clear();
        r.unfolded.clear();
    });
    save();
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

/// The first unit after `after` (or from the start) that still needs a
/// review, wrapping around.
pub fn next_open(after: Option<&str>) -> Option<String> {
    let units = REVIEW.with(|r| r.borrow().units.clone());
    let start = after
        .and_then(|a| units.iter().position(|u| u == a))
        .map_or(0, |i| i + 1);
    let n = units.len();
    (0..n)
        .map(|k| &units[(start + k) % n])
        .find(|u| mark(u) != Some(Mark::Done))
        .cloned()
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

/// The verdict buttons, as on GitHub. Approving moves on right away; the
/// other two ask for a comment first.
const ACTIONS: &str = r#"<button class="rv-btn approve" data-rv-act="approve" title="Approve this item and go to the next unreviewed one (n: next without a verdict)">✓ Approve</button><button class="rv-btn changes" data-rv-act="changes" title="Write what needs to change, then go to the next unreviewed item">✗ Request changes</button><button class="rv-btn comment" data-rv-act="comment" title="Write a comment, then go to the next unreviewed item">✎ Comment</button>"#;

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
        Mark::Open => ACTIONS.to_string(),
        Mark::Done => {
            let (class, label, what) = match n.verdict {
                Verdict::Approve => ("approved", "✓ Approved", "Approved"),
                Verdict::Changes => ("changes", "✗ Changes requested", "You requested changes"),
                Verdict::Comment => ("commented", "✎ Commented", "You commented"),
            };
            let unfolded = REVIEW.with(|r| r.borrow().unfolded.contains(id));
            format!(
                r#"<button class="rv-chip {class}" data-rv-undo title="{what}. Click to remove the mark">{label}</button>{text}<button class="rv-edit" data-rv-edit title="Change the verdict or the comment">Edit</button><button class="rv-fold" data-rv-fold title="{}">{}</button>"#,
                if unfolded {
                    "Fold the item back to one line"
                } else {
                    "Show the whole item again"
                },
                if unfolded { "▴ Fold" } else { "▾ Expand" }
            )
        }
        Mark::Stale => format!(
            r#"<button class="rv-chip stale" data-rv-undo title="You reviewed an earlier version; the diff is different now. Click to remove the mark">⚠ Changed since review</button><button class="rv-since" data-rv-since title="Show what is different from the version you reviewed">Show changes</button>{text}{ACTIONS}"#
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

/// The progress and the "next" button in the header.
pub fn update_progress() {
    update_groups();
    let (done, total) = progress();
    let html = match (done * 100).checked_div(total) {
        None => r#"<span class="dim">Nothing to review</span>"#.to_string(),
        Some(percent) => format!(
            r#"<span class="rv-count" title="Reviewed items and files">{done} / {total} reviewed</span><span class="rv-meter"><span style="width:{}%"></span></span><button id="rv-next" title="Go to the next unreviewed item (n)">Next ▸</button><button id="rv-finish" title="Sum up your verdicts and comments, to post them as a review on GitHub">Finish review…</button><button id="rv-clear" title="Remove every review mark of this pull request">Clear</button>"#,
            percent
        ),
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
        })
    }

    fn set(d: &Data, marks: &[(&str, u64)]) {
        let units = units_in_order(d);
        let (hunks, prints) = hunks_and_prints(d, &units);
        REVIEW.with(|r| {
            *r.borrow_mut() = Review {
                on: true,
                key: String::new(),
                prints,
                units: units.into_iter().map(|(id, _)| id).collect(),
                marks: marks.iter().map(|(id, h)| (id.to_string(), *h)).collect(),
                versions: BTreeMap::new(),
                notes: BTreeMap::new(),
                unfolded: HashSet::new(),
                hunks,
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
    fn hash_is_stable() {
        // FNV-1a test vectors.
        assert_eq!(hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
