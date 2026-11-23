//! Review marks: the reviewer marks items and files as reviewed, one by
//! one, and moves on to the next unreviewed one.
//!
//! A mark remembers what the item looked like when it was marked (a hash
//! of its diff). When the page is generated again for a newer revision of
//! the PR and the item's diff is different, the mark no longer counts: the
//! item shows as changed since the review.
//!
//! Marks are kept in the browser's local storage, per pull request (or per
//! pair of compared trees), when the browser allows that.

use crate::code::esc;
use crate::data::{Data, changed};
use crate::dom;
use model::{ChangedFile, Item, Kind};
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
}

thread_local! {
    static REVIEW: RefCell<Review> = RefCell::new(Review::default());
}

const MODE_KEY: &str = "rust-pr-explorer.review-mode";
const MARKS_KEY: &str = "rust-pr-explorer.reviewed";

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

/// FNV-1a, so that hashes are the same in every browser and every run.
fn hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
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

/// Which pull request the page is about: marks of one PR apply to every
/// page generated for it, also after new commits.
fn identity(d: &Data) -> String {
    let meta = &d.out.meta;
    match &meta.pr {
        Some(pr) => format!("{}#{}", pr.repo, pr.number),
        None => meta.url.clone().unwrap_or_else(|| meta.title.clone()),
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
    let key = format!("{MARKS_KEY}:{}", identity(d));
    let store = storage();
    let marks = store
        .as_ref()
        .and_then(|s| s.get_item(&key).ok().flatten())
        .map(|s| decode(&s))
        .unwrap_or_default();
    let on = store
        .and_then(|s| s.get_item(MODE_KEY).ok().flatten())
        .is_some_and(|v| v == "on");
    REVIEW.with(|r| {
        *r.borrow_mut() = Review {
            on,
            key,
            prints: units.iter().cloned().collect(),
            units: units.into_iter().map(|(id, _)| id).collect(),
            marks,
        }
    });
    apply_mode();
}

fn save() {
    let (key, text) = REVIEW.with(|r| {
        let r = r.borrow();
        (r.key.clone(), encode(&r.marks))
    });
    let saved = storage().is_some_and(|s| {
        if text.is_empty() {
            s.remove_item(&key).is_ok()
        } else {
            s.set_item(&key, &text).is_ok()
        }
    });
    if !saved {
        crate::toast(
            "This browser doesn't allow saving review marks: they are lost when the page is closed.",
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

/// Marks a unit as reviewed in its current state, or removes the mark.
pub fn set_mark(id: &str, done: bool) {
    let changed = REVIEW.with(|r| {
        let mut r = r.borrow_mut();
        let Some(&now) = r.prints.get(id) else {
            return false;
        };
        if done {
            r.marks.insert(id.to_string(), now);
        } else {
            r.marks.remove(id);
        }
        true
    });
    if changed {
        save();
        refresh(id);
    }
}

/// Removes every mark of this PR.
pub fn clear() {
    REVIEW.with(|r| r.borrow_mut().marks.clear());
    save();
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

/// The mark and its buttons for a unit, or nothing for things that can't
/// be marked. Outside review mode, only an existing mark is shown.
pub fn control_html(id: &str) -> String {
    let Some(m) = mark(id) else {
        return String::new();
    };
    format!(
        r#"<span class="rvc {}" data-rv="{}">{}</span>"#,
        match m {
            Mark::Open => "open",
            Mark::Done => "done",
            Mark::Stale => "stale",
        },
        esc(id),
        control_inner(m)
    )
}

fn control_inner(m: Mark) -> &'static str {
    match m {
        Mark::Open => {
            r#"<button class="rv-btn" data-rv-done title="Mark as reviewed and go to the next unreviewed item (n: next without marking)">✓ Reviewed, next ▸</button>"#
        }
        Mark::Done => {
            r#"<button class="rv-chip" data-rv-undo title="Reviewed. Click to remove the mark">✓ Reviewed</button>"#
        }
        Mark::Stale => {
            r#"<button class="rv-chip stale" data-rv-undo title="You reviewed an earlier version; the diff is different now. Click to remove the mark">⚠ Changed since review</button><button class="rv-btn" data-rv-done title="Mark as reviewed and go to the next unreviewed item">✓ Reviewed, next ▸</button>"#
        }
    }
}

/// Updates every control of a unit on the page, and the progress, without
/// rendering the views again (which would lose the scroll position).
fn refresh(id: &str) {
    let Some(m) = mark(id) else {
        return;
    };
    for el in dom::query_all(".rvc") {
        if dom::data(&el, "rv").as_deref() == Some(id) {
            el.set_class_name(match m {
                Mark::Open => "rvc open",
                Mark::Done => "rvc done",
                Mark::Stale => "rvc stale",
            });
            el.set_inner_html(control_inner(m));
        }
    }
    update_progress();
}

/// The progress and the "next" button in the header.
pub fn update_progress() {
    let (done, total) = progress();
    let html = match (done * 100).checked_div(total) {
        None => r#"<span class="dim">Nothing to review</span>"#.to_string(),
        Some(percent) => format!(
            r#"<span class="rv-count" title="Reviewed items and files">{done} / {total} reviewed</span><span class="rv-meter"><span style="width:{}%"></span></span><button id="rv-next" title="Go to the next unreviewed item (n)">Next ▸</button><button id="rv-clear" title="Remove every review mark of this pull request">Clear</button>"#,
            percent
        ),
    };
    dom::set_html("rv-bar", &html);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_round_trip() {
        let mut m = BTreeMap::new();
        m.insert("t::f".to_string(), 0x1234);
        m.insert("file:docs/a b.md".to_string(), u64::MAX);
        assert_eq!(decode(&encode(&m)), m);
        // Damaged lines are skipped.
        assert_eq!(decode("zz t::f\n\n").len(), 0);
    }

    #[test]
    fn hash_is_stable() {
        // FNV-1a test vectors.
        assert_eq!(hash(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash("a"), 0xaf63_dc4c_8601_ec8c);
    }
}
