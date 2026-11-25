//! Review comments on chosen lines of an item's source diff, or of the
//! diff of a file outside the Rust analysis, as on GitHub: in review mode,
//! a click on a line number selects that line,
//! Shift+click on another one selects the lines between, and the bar under
//! the selection opens the comment editor.
//!
//! A line comment is not a review unit: it gives no verdict to its item
//! or hunk, which still get their own marks. It goes into the posted
//! review as an inline comment on its lines (see `post::lines_loc`).
//!
//! The comments are kept in the browser's local storage next to the review
//! marks, per pull request. Each remembers the text of its lines, on each
//! side, so that it follows its lines when a later revision of the PR moves
//! them. When the lines changed, or their text is found in more than one
//! place, the comment is stale: it is shown at the end of its item's diff
//! and left out of the review.

use crate::code::{esc, markdown_file_html};
use crate::data::Data;
use crate::dom;
use crate::review::Verdict;
use model::stable_hash as hash;
use model::{ChangedFile, Item, SourceLine, Status};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeMap;
use wasm_bindgen::JsCast;
use web_sys::Element;

/// Lines of one side of an item, first to last, with a hash of their text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub first: usize,
    pub last: usize,
    pub print: u64,
}

impl Span {
    fn len(&self) -> usize {
        self.last + 1 - self.first
    }

    fn shifted(&self, to: usize) -> Span {
        Span {
            first: to,
            last: to + self.len() - 1,
            print: self.print,
        }
    }

    pub fn range(&self) -> (usize, usize) {
        (self.first, self.last)
    }
}

/// A comment on lines of an item (or of a file: `file:PATH`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineNote {
    pub item: String,
    /// The selected lines of the base version (removed lines), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<Span>,
    /// The selected lines of the PR's version (added and unchanged lines),
    /// if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<Span>,
    /// The comment is shown under its last line, a base line if this is
    /// set.
    #[serde(default)]
    pub end_old: bool,
    #[serde(default)]
    pub verdict: Verdict,
    pub text: String,
}

/// Where a comment's lines are now: its spans moved to their current
/// place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub old: Option<Span>,
    pub new: Option<Span>,
    pub end_old: bool,
}

impl Placed {
    /// The line the comment is shown under: (base side, number).
    pub fn end(&self) -> Option<(bool, usize)> {
        match (self.end_old, self.old, self.new) {
            (true, Some(o), _) => Some((true, o.last)),
            (_, _, Some(n)) => Some((false, n.last)),
            (_, Some(o), None) => Some((true, o.last)),
            _ => None,
        }
    }

    /// Its first line on either side, roughly where the diff shows it.
    pub fn first_line(&self) -> usize {
        [self.old, self.new]
            .into_iter()
            .flatten()
            .map(|s| s.first)
            .min()
            .unwrap_or(0)
    }

    /// Whether it covers a line.
    pub fn covers(&self, old: bool, line: usize) -> bool {
        let s = if old { self.old } else { self.new };
        s.is_some_and(|s| (s.first..=s.last).contains(&line))
    }

    /// "line 12", "lines 12–14", "base lines 3–4", or both.
    pub fn label(&self) -> String {
        label(self.old.map(|s| s.range()), self.new.map(|s| s.range()))
    }
}

/// How lines are named: "line 12", "lines 12–14", "base line 3", or
/// "base line 3 and lines 4–5" for both sides.
pub fn label(old: Option<(usize, usize)>, new: Option<(usize, usize)>) -> String {
    let one = |(a, b): (usize, usize)| {
        if b > a {
            format!("lines {a}–{b}")
        } else {
            format!("line {a}")
        }
    };
    match (old, new) {
        (Some(o), Some(n)) => format!("base {} and {}", one(o), one(n)),
        (Some(o), None) => format!("base {}", one(o)),
        (None, Some(n)) => one(n),
        (None, None) => String::new(),
    }
}

/// Where an item's lines start in the base and the PR's version of their
/// files, as its source diff numbers them.
pub fn starts(it: &Item) -> (usize, usize) {
    match it.status {
        Status::Removed => (it.info.line, it.info.line),
        _ => (it.old_line.unwrap_or(it.info.line), it.info.line),
    }
}

/// The lines of a unit that its diffs show, on each side, by number: the
/// base's (unchanged and removed lines) and the PR's (unchanged and added
/// lines). For an item all of its lines; for a file the lines of its line
/// diff and of its boxes' diffs, and all of a Markdown file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sides {
    pub old: BTreeMap<usize, String>,
    pub new: BTreeMap<usize, String>,
}

impl Sides {
    pub fn of_item(it: &Item) -> Sides {
        let (o, n) = starts(it);
        let mut s = Sides::default();
        s.add(o, n, &it.source);
        s
    }

    pub fn of_file(f: &ChangedFile) -> Sides {
        let mut s = Sides::default();
        s.add(1, 1, &f.hunks);
        for p in &f.parts {
            s.add(1, 1, &p.lines);
        }
        let whole = |text: &Option<String>| -> BTreeMap<usize, String> {
            text.iter()
                .flat_map(|t| t.split('\n').enumerate())
                .map(|(i, l)| (i + 1, l.to_string()))
                .collect()
        };
        s.old.extend(whole(&f.old_text));
        s.new.extend(whole(&f.new_text));
        s
    }

    /// The lines of an item or a file (`file:PATH`).
    pub fn of_unit(d: &Data, unit: &str) -> Option<Sides> {
        match unit.strip_prefix("file:") {
            Some(path) => d
                .out
                .files
                .iter()
                .find(|f| f.path == path)
                .map(Sides::of_file),
            None => d.item(unit).map(Sides::of_item),
        }
    }

    /// Adds the lines of a diff whose first lines have the numbers `o` and
    /// `n`, or as its `@` lines say.
    fn add(&mut self, mut o: usize, mut n: usize, lines: &[SourceLine]) {
        for l in lines {
            match l.0 {
                '@' => {
                    let mut it = l.1.split(' ').filter_map(|x| x.parse::<usize>().ok());
                    o = it.next().unwrap_or(1);
                    n = it.next().unwrap_or(1);
                }
                '-' => {
                    self.old.insert(o, l.1.clone());
                    o += 1;
                }
                '+' => {
                    self.new.insert(n, l.1.clone());
                    n += 1;
                }
                _ => {
                    self.old.insert(o, l.1.clone());
                    self.new.insert(n, l.1.clone());
                    o += 1;
                    n += 1;
                }
            }
        }
    }

    fn side(&self, old: bool) -> &BTreeMap<usize, String> {
        if old { &self.old } else { &self.new }
    }
}

/// The hash of lines `first..=last` of a side, `None` if it doesn't have
/// them all.
fn print_of(side: &BTreeMap<usize, String>, first: usize, last: usize) -> Option<u64> {
    let window = (first..=last)
        .map(|l| side.get(&l).map(String::as_str))
        .collect::<Option<Vec<&str>>>()?;
    Some(hash(&window.join("\n")))
}

/// A span of lines `first..=last` of one side, with their hash.
pub fn span(s: &Sides, old: bool, first: usize, last: usize) -> Option<Span> {
    Some(Span {
        first,
        last,
        print: print_of(s.side(old), first, last)?,
    })
}

/// Where a span's lines are now: at the same place if their text is still
/// there, else at the only other place with the same text.
fn relocate(s: &Sides, old: bool, sp: Span) -> Option<Span> {
    let side = s.side(old);
    if print_of(side, sp.first, sp.last) == Some(sp.print) {
        return Some(sp);
    }
    let mut found = side
        .keys()
        .copied()
        .filter(|&f| print_of(side, f, f + sp.len() - 1) == Some(sp.print));
    match (found.next(), found.next()) {
        (Some(f), None) => Some(sp.shifted(f)),
        _ => None,
    }
}

/// Where a comment's lines are now, `None` if they changed.
pub fn place(s: &Sides, n: &LineNote) -> Option<Placed> {
    if n.old.is_none() && n.new.is_none() {
        return None;
    }
    let old = match n.old {
        Some(sp) => Some(relocate(s, true, sp)?),
        None => None,
    };
    let new = match n.new {
        Some(sp) => Some(relocate(s, false, sp)?),
        None => None,
    };
    Some(Placed {
        old,
        new,
        end_old: n.end_old,
    })
}

/// The key of a line in the source diff's `data-lc`: `o` and the base
/// line number for a removed line, else `n` and the PR's.
pub fn key(old: bool, line: usize) -> String {
    format!("{}{line}", if old { 'o' } else { 'n' })
}

fn parse_key(k: &str) -> Option<(bool, usize)> {
    let old = match k.as_bytes().first()? {
        b'o' => true,
        b'n' => false,
        _ => return None,
    };
    Some((old, k[1..].parse().ok()?))
}

/// The first and last of some lines.
pub type Lines = (usize, usize);

/// The lines that a selection of keys (in the order the diff shows them)
/// covers on each side, and whether the last one is a base line.
pub fn spans_of_keys(keys: &[String]) -> (Option<Lines>, Option<Lines>, bool) {
    let mut old: Option<Lines> = None;
    let mut new: Option<Lines> = None;
    let mut end_old = false;
    for (o, l) in keys.iter().filter_map(|k| parse_key(k)) {
        let s = if o { &mut old } else { &mut new };
        *s = Some(s.map_or((l, l), |(a, b)| (a.min(l), b.max(l))));
        end_old = o;
    }
    (old, new, end_old)
}

#[derive(Default)]
struct State {
    /// Storage key of this PR's comments (the review marks' key).
    key: String,
    notes: BTreeMap<String, LineNote>,
    /// The selected lines: the diff they are in, and the first and last
    /// clicked line's key.
    sel: Option<(Element, String, String)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

const NOTES_KEY: &str = "rust-pr-explorer.line-comments";

fn storage() -> Option<web_sys::Storage> {
    dom::window().local_storage().ok().flatten()
}

fn read(key: &str) -> BTreeMap<String, LineNote> {
    storage()
        .and_then(|s| s.get_item(&format!("{NOTES_KEY}:{key}")).ok().flatten())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Reads the comments saved for a PR (by the review marks' key).
pub fn load(key: &str) {
    let notes = read(key);
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.key = key.to_string();
        s.notes = notes;
        s.sel = None;
    });
}

/// Changes the comments as `f` says and saves them. Reads them again
/// first: another tab of the PR may have saved some since.
fn update(f: impl FnOnce(&mut BTreeMap<String, LineNote>)) {
    let key = STATE.with(|s| s.borrow().key.clone());
    let mut notes = read(&key);
    f(&mut notes);
    let text = serde_json::to_string(&notes).unwrap_or_default();
    STATE.with(|s| s.borrow_mut().notes = notes.clone());
    let k = format!("{NOTES_KEY}:{key}");
    let saved = storage().is_some_and(|s| {
        if notes.is_empty() {
            s.remove_item(&k).is_ok()
        } else {
            s.set_item(&k, &text).is_ok()
        }
    });
    if !saved {
        crate::toast(
            "This browser doesn't allow saving your line comments: they are lost when the page is closed.",
        );
    }
}

/// Every comment, by id.
pub fn all() -> BTreeMap<String, LineNote> {
    STATE.with(|s| s.borrow().notes.clone())
}

/// Adds comments from elsewhere (copied marks) where there is none with
/// the same id.
pub fn merge(other: BTreeMap<String, LineNote>) {
    if other.is_empty() {
        return;
    }
    update(|notes| {
        for (id, n) in other {
            notes.entry(id).or_insert(n);
        }
    });
}

/// Removes every comment.
pub fn clear() {
    update(BTreeMap::clear);
}

/// The comments on an item (or file) with where they are now (`None` if
/// their lines changed), in the order of their lines.
pub fn of_unit(unit: &str, sides: &Sides) -> Vec<(String, LineNote, Option<Placed>)> {
    let mut out: Vec<(String, LineNote, Option<Placed>)> = STATE.with(|s| {
        s.borrow()
            .notes
            .iter()
            .filter(|(id, n)| n.item == unit && id.as_str() != NEW)
            .map(|(id, n)| (id.clone(), n.clone(), place(sides, n)))
            .collect()
    });
    out.sort_by_key(|(id, _, p)| (p.map(|p| p.first_line()), id.clone()));
    out
}

/// The box that shows a comment under its last line.
pub fn box_html(d: &Data, id: &str, n: &LineNote, p: Option<&Placed>) -> String {
    let (what, class) = match n.verdict {
        Verdict::Changes => ("✗ Change requested", "changes"),
        _ => ("✎ Your comment", "comment"),
    };
    let at = match p {
        Some(p) => format!("on {}", p.label()),
        None => format!(
            "on {} · ⚠ these lines changed since, so it is left out of the review",
            label(n.old.map(|s| s.range()), n.new.map(|s| s.range()))
        ),
    };
    format!(
        r#"<span class="rt-row lc-box {class}{}" data-lc-id="{id}"><span class="lc-head"><b>{what}</b> <span class="dim">{at}</span><span class="grow"></span><button class="mini" data-lc-edit="{id}">Edit</button><button class="mini" data-lc-remove="{id}">Remove</button></span><span class="docs">{}</span></span>"#,
        if p.is_none() { " lc-stale" } else { "" },
        markdown_file_html(d, &n.text),
        id = esc(id),
    )
}

/// What the diffs of an item or file add for line comments: whether a
/// line is commented, and the boxes of the comments that end on a line.
pub struct ForUnit {
    list: Vec<(String, LineNote, Option<Placed>)>,
}

impl ForUnit {
    pub fn new(unit: &str, sides: &Sides) -> ForUnit {
        ForUnit {
            list: of_unit(unit, sides),
        }
    }

    pub fn of(d: &Data, unit: &str) -> ForUnit {
        let sides = Sides::of_unit(d, unit).unwrap_or_default();
        ForUnit::new(unit, &sides)
    }

    pub fn covers(&self, old: bool, line: usize) -> bool {
        self.list
            .iter()
            .any(|(_, _, p)| p.is_some_and(|p| p.covers(old, line)))
    }

    /// Whether a comment is shown under the line.
    pub fn ends_at(&self, old: bool, line: usize) -> bool {
        self.list
            .iter()
            .any(|(_, _, p)| p.and_then(|p| p.end()) == Some((old, line)))
    }

    /// The boxes of the comments shown under one of the lines.
    pub fn boxes_at(&self, d: &Data, lines: &[(bool, usize)]) -> String {
        self.list
            .iter()
            .filter(|(_, _, p)| p.and_then(|p| p.end()).is_some_and(|e| lines.contains(&e)))
            .map(|(id, n, p)| box_html(d, id, n, p.as_ref()))
            .collect()
    }

    /// The boxes of the comments whose lines changed, for the end of the
    /// diff.
    pub fn stale_html(&self, d: &Data) -> String {
        self.list
            .iter()
            .filter(|(_, _, p)| p.is_none())
            .map(|(id, n, _)| box_html(d, id, n, None))
            .collect()
    }
}

/// The comments for a posted review, and how many were left out since
/// their lines changed.
pub fn comments(d: &Data) -> (Vec<crate::post::Comment>, usize) {
    let mut out = Vec::new();
    let mut stale = 0;
    let list: Vec<(String, LineNote)> = all().into_iter().collect();
    // In reading order of their items, then by line.
    let mut sides: BTreeMap<String, Option<Sides>> = BTreeMap::new();
    let mut placed = |n: &LineNote| {
        sides
            .entry(n.item.clone())
            .or_insert_with(|| Sides::of_unit(d, &n.item))
            .as_ref()
            .and_then(|s| place(s, n))
    };
    let mut list: Vec<(String, LineNote, Option<Placed>)> = list
        .into_iter()
        .map(|(id, n)| {
            let p = placed(&n);
            (id, n, p)
        })
        .collect();
    list.sort_by_key(|(id, n, p)| {
        (
            crate::review::unit_index(&n.item),
            p.map(|p| p.first_line()),
            id.clone(),
        )
    });
    for (_, n, p) in list {
        if n.text.trim().is_empty() {
            continue;
        }
        let (Some(t), Some(p)) = (Target::of(d, &n.item), p) else {
            stale += 1;
            continue;
        };
        let (old, new) = (p.old.map(|s| s.range()), p.new.map(|s| s.range()));
        out.push(crate::post::Comment {
            name: t.name(d),
            part: format!(", {}", p.label()),
            verdict: n.verdict,
            text: n.text.trim().to_string(),
            link: t.lines_link(d, old, new),
            loc: t.lines_loc(d, old, new),
        });
    }
    (out, stale)
}

/// What a line comment is on: an item, or a file outside the Rust
/// analysis.
#[derive(Clone, Copy)]
pub enum Target<'a> {
    Item(&'a Item),
    File(&'a ChangedFile),
}

impl<'a> Target<'a> {
    pub fn of(d: &'a Data, unit: &str) -> Option<Target<'a>> {
        match unit.strip_prefix("file:") {
            Some(path) => d
                .out
                .files
                .iter()
                .find(|f| f.path == path)
                .map(Target::File),
            None => d.item(unit).map(Target::Item),
        }
    }

    fn name(&self, d: &Data) -> String {
        match self {
            Target::Item(it) => d.display_name(it),
            Target::File(f) => f.path.clone(),
        }
    }

    fn lines_loc(
        &self,
        d: &Data,
        old: Option<Lines>,
        new: Option<Lines>,
    ) -> Option<crate::post::Loc> {
        match self {
            Target::Item(it) => crate::post::lines_loc(&d.out.files, it, old, new),
            Target::File(f) => crate::post::file_lines_loc(&d.out.files, &f.path, old, new),
        }
    }

    fn lines_link(&self, d: &Data, old: Option<Lines>, new: Option<Lines>) -> Option<String> {
        match self {
            Target::Item(it) => crate::post::lines_link(d, it, old, new),
            Target::File(f) => crate::post::file_lines_link(d, &f.path, old, new),
        }
    }
}

/// The source diffs that show an item.
fn containers(item: &str) -> Vec<Element> {
    dom::query_all(".src-code[data-item]")
        .into_iter()
        .filter(|c| dom::data(c, "item").as_deref() == Some(item))
        .collect()
}

/// The last element of the row of a line's number (`[data-lc]`): the
/// line itself inline; side by side, the code cell after its number, and
/// for a base cell the PR's cell after that. Then past the review threads
/// and comments under the row.
fn row_end(el: &Element) -> Element {
    let next = |e: &Element| e.next_element_sibling();
    let mut end = el.clone();
    if el.class_list().contains("no") {
        if let Some(code) = next(el) {
            end = code;
        }
        if el.class_list().contains("o")
            && let Some(no) = next(&end).filter(|e| e.class_list().contains("no"))
            && let Some(code) = next(&no)
        {
            end = code;
        }
    }
    while let Some(n) = next(&end).filter(|e| e.class_list().contains("rt-row")) {
        end = n;
    }
    end
}

/// Shows an item's comments again in its diffs on the page, after they
/// changed, without rendering the views again.
pub fn refresh(d: &Data, item: &str) {
    let Some(sides) = Sides::of_unit(d, item) else {
        return;
    };
    let f = ForUnit::new(item, &sides);
    for c in containers(item) {
        for b in dom::query_all_in(&c, ".lc-box") {
            b.remove();
        }
        for el in dom::query_all_in(&c, "[data-lc]") {
            let covered = dom::data(&el, "lc")
                .and_then(|k| parse_key(&k))
                .is_some_and(|(o, l)| f.covers(o, l));
            let _ = if covered {
                el.set_attribute("data-lc-on", "")
            } else {
                el.remove_attribute("data-lc-on")
            };
        }
        let mut done: Vec<String> = Vec::new();
        for el in dom::query_all_in(&c, "[data-lc]") {
            let Some(k) = dom::data(&el, "lc") else {
                continue;
            };
            let Some((o, l)) = parse_key(&k) else {
                continue;
            };
            if done.contains(&k) || !f.ends_at(o, l) {
                continue;
            }
            done.push(k);
            let _ = row_end(&el).insert_adjacent_html("afterend", &f.boxes_at(d, &[(o, l)]));
        }
        let stale = f.stale_html(d);
        if !stale.is_empty()
            && let Some(code) = c.first_element_child()
        {
            let _ = code.insert_adjacent_html("beforeend", &stale);
        }
    }
    show_selection();
}

/// The keys of the selected lines in the order the diff shows them, and
/// the elements of their numbers.
fn selected() -> Option<(String, Vec<String>, Vec<Element>)> {
    let (c, a, b) = STATE.with(|s| s.borrow().sel.clone())?;
    let item = dom::data(&c, "item")?;
    let all = dom::query_all_in(&c, "[data-lc]");
    let keys: Vec<String> = all
        .iter()
        .map(|e| dom::data(e, "lc").unwrap_or_default())
        .collect();
    let ia = keys.iter().position(|k| *k == a)?;
    let ib = keys.iter().position(|k| *k == b)?;
    let (lo, mut hi) = (ia.min(ib), ia.max(ib));
    // Side by side, an unchanged line has the same key on both sides.
    while hi + 1 < keys.len() && keys[hi + 1] == keys[hi] {
        hi += 1;
    }
    let mut sel_keys: Vec<String> = Vec::new();
    for k in &keys[lo..=hi] {
        if sel_keys.last() != Some(k) {
            sel_keys.push(k.clone());
        }
    }
    Some((item, sel_keys, all[lo..=hi].to_vec()))
}

/// Marks the selected lines and puts the bar with the comment buttons
/// under them.
fn show_selection() {
    for e in dom::query_all(".lc-sel") {
        dom::toggle_class(&e, "lc-sel", false);
    }
    for e in dom::query_all(".lc-bar") {
        e.remove();
    }
    let Some((_, keys, els)) = selected() else {
        return;
    };
    for e in &els {
        dom::toggle_class(e, "lc-sel", true);
    }
    let (old, new, _) = spans_of_keys(&keys);
    let what = label(old, new);
    let what = format!("{}{}", what[..1].to_uppercase(), &what[1..]);
    let bar = format!(
        r#"<span class="rt-row lc-bar"><b>{what}</b><button class="rv-btn comment" data-lc-act="comment" title="Comment on these lines">✎ Comment</button><button class="rv-btn changes" data-lc-act="changes" title="Say what to change on these lines">✗ Request changes</button><button class="mini" data-lc-cancel title="Unselect the lines (Esc)">Cancel</button><span class="dim lc-hint">Shift+click another line number to select the lines between.</span></span>"#,
        what = esc(&what)
    );
    if let Some(last) = els.last() {
        let _ = row_end(last).insert_adjacent_html("afterend", &bar);
    }
}

/// Unselects the lines, if any are; whether some were.
pub fn cancel() -> bool {
    let had = STATE.with(|s| s.borrow_mut().sel.take().is_some());
    if had {
        show_selection();
    }
    had
}

/// A click on a line number in review mode, on a selection's bar or on a
/// comment's buttons. Returns whether it was one of them.
pub fn on_click(app: &mut crate::App, el: &Element, ev: &web_sys::Event) -> bool {
    if dom::closest(el, "[data-lc-cancel]").is_some() {
        cancel();
        return true;
    }
    if let Some(b) = dom::closest(el, "[data-lc-act]") {
        let v = dom::data(&b, "lc-act").and_then(|v| Verdict::from_name(&v));
        open_new(app, v.unwrap_or(Verdict::Comment));
        return true;
    }
    if let Some(b) = dom::closest(el, "[data-lc-edit]") {
        open_editor(app, &dom::data(&b, "lc-edit").unwrap_or_default(), None);
        return true;
    }
    if let Some(b) = dom::closest(el, "[data-lc-remove]") {
        let id = dom::data(&b, "lc-remove").unwrap_or_default();
        let sure = dom::window()
            .confirm_with_message("Remove this comment?")
            .unwrap_or(false);
        if sure {
            remove(app, &id);
        }
        return true;
    }
    if !crate::review::is_on() {
        return false;
    }
    // The line numbers: inline, the gutter of a line; side by side, the
    // cell before the code.
    let num = if dom::closest(el, ".lno, .sg").is_some() {
        dom::closest(el, ".ln[data-lc]")
    } else {
        dom::closest(el, ".no[data-lc]")
    };
    let Some(num) = num else {
        return false;
    };
    let (Some(c), Some(k)) = (
        dom::closest(&num, ".src-code[data-item]"),
        dom::data(&num, "lc"),
    ) else {
        return false;
    };
    let shift = ev
        .dyn_ref::<web_sys::MouseEvent>()
        .is_some_and(|m| m.shift_key());
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.sel = match s.sel.take() {
            // Shift+click extends the selection in the same diff.
            Some((sc, a, _)) if shift && sc == c => Some((sc, a, k)),
            // A click on the only selected line unselects it.
            Some((sc, a, b)) if !shift && sc == c && a == k && b == k => None,
            _ => Some((c, k.clone(), k)),
        };
    });
    show_selection();
    true
}

/// Opens the editor for a comment on the selected lines.
fn open_new(app: &crate::App, verdict: Verdict) {
    let Some((item, keys, _)) = selected() else {
        return;
    };
    let Some(sides) = Sides::of_unit(&app.d, &item) else {
        return;
    };
    let (old, new, end_old) = spans_of_keys(&keys);
    let n = LineNote {
        item,
        old: old.and_then(|(a, b)| span(&sides, true, a, b)),
        new: new.and_then(|(a, b)| span(&sides, false, a, b)),
        end_old,
        verdict,
        text: String::new(),
    };
    if n.old.is_none() && n.new.is_none() {
        return;
    }
    STATE.with(|s| s.borrow_mut().notes.insert(NEW.to_string(), n));
    open_editor(app, NEW, Some(verdict));
}

/// The id of a comment that is being written, until it is saved.
const NEW: &str = "lc:new";

/// The dialog for writing a line comment.
fn open_editor(app: &crate::App, id: &str, verdict: Option<Verdict>) {
    let Some(n) = STATE.with(|s| s.borrow().notes.get(id).cloned()) else {
        return;
    };
    let (Some(t), Some(sides)) = (Target::of(&app.d, &n.item), Sides::of_unit(&app.d, &n.item))
    else {
        return;
    };
    let verdict = verdict.unwrap_or(n.verdict);
    let p = place(&sides, &n);
    let lines = p.map_or_else(
        || label(n.old.map(|s| s.range()), n.new.map(|s| s.range())),
        |p| p.label(),
    );
    let at = if app.d.out.meta.pr.is_none() {
        String::new()
    } else {
        match p.and_then(|p| t.lines_loc(&app.d, p.old.map(|s| s.range()), p.new.map(|s| s.range())))
        {
            Some(l) => format!(
                "Posted to GitHub, it goes on {} of <code>{}</code>.",
                l.lines(),
                esc(&l.path)
            ),
            None if p.is_none() => {
                "These lines changed since, so the comment is left out of the review.".to_string()
            }
            None => "GitHub's diff doesn't show all of these lines in one piece, so posted to GitHub, it goes into the review's text, with a link to them.".to_string(),
        }
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
    let remove = if id == NEW {
        String::new()
    } else {
        format!(r#"<button data-lc-remove="{}">Remove</button>"#, esc(id))
    };
    let title = match verdict {
        Verdict::Changes => "Request changes on",
        _ => "Comment on",
    };
    crate::open_modal(
        &format!(
            "{title} <code>{}</code>, {}",
            esc(&t.name(&app.d)),
            esc(&lines)
        ),
        &format!(
            r#"<div class="rv-editor" data-rv-for="lc:{}" data-rv-verdict="{}">
<textarea id="rv-text" rows="6" placeholder="{}">{}</textarea>
<p class="sub">{at} Markdown works. Ctrl+Enter saves.</p>
<div class="rv-row">{}{}{remove}<span class="grow"></span><button data-modal-close>Cancel</button></div>
</div>"#,
            esc(id),
            verdict.name(),
            if verdict == Verdict::Changes {
                "What needs to change here?"
            } else {
                "Leave a comment"
            },
            esc(&n.text),
            button(Verdict::Comment, "✎ Comment"),
            button(Verdict::Changes, "✗ Request changes"),
        ),
    );
    if let Some(t) = dom::by_id("rv-text").and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
        let _ = t.focus();
    }
}

/// Whether an editor's id (`data-rv-for`) is of a line comment; then the
/// comment's id.
pub fn editor_id(for_: &str) -> Option<&str> {
    for_.strip_prefix("lc:")
}

thread_local! {
    static COUNTER: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Saves the comment of the open editor.
pub fn save(app: &mut crate::App, id: &str, verdict: Verdict, text: String) {
    let Some(mut n) = STATE.with(|s| s.borrow().notes.get(id).cloned()) else {
        return;
    };
    n.verdict = if verdict == Verdict::Changes {
        Verdict::Changes
    } else {
        Verdict::Comment
    };
    n.text = text;
    let item = n.item.clone();
    let id = if id == NEW {
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            s.notes.remove(NEW);
            s.sel = None;
        });
        let k = COUNTER.with(|c| {
            c.set(c.get() + 1);
            c.get()
        });
        format!("{:x}-{k}", js_sys::Date::now() as u64)
    } else {
        id.to_string()
    };
    update(|notes| {
        notes.insert(id, n);
    });
    refresh(&app.d, &item);
    crate::review::update_progress();
}

fn remove(app: &mut crate::App, id: &str) {
    let Some(item) = STATE.with(|s| s.borrow().notes.get(id).map(|n| n.item.clone())) else {
        return;
    };
    if id == NEW {
        STATE.with(|s| s.borrow_mut().notes.remove(NEW));
    } else {
        update(|notes| {
            notes.remove(id);
        });
    }
    crate::close_modal();
    refresh(&app.d, &item);
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Kind, SourceLine};

    fn item(source: &[(char, &str)]) -> Item {
        let mut it = Item {
            status: Status::Changed,
            source: source
                .iter()
                .map(|(t, s)| SourceLine(*t, s.to_string()))
                .collect(),
            ..Default::default()
        };
        it.info.id = "c::f".into();
        it.info.kind = Kind::Fn;
        it.info.file = "src/lib.rs".into();
        it.info.line = 10;
        it.old_line = Some(8);
        it
    }

    fn sides(source: &[(char, &str)]) -> Sides {
        Sides::of_item(&item(source))
    }

    #[test]
    fn labels() {
        assert_eq!(label(None, Some((4, 4))), "line 4");
        assert_eq!(label(None, Some((4, 6))), "lines 4–6");
        assert_eq!(label(Some((3, 3)), None), "base line 3");
        assert_eq!(
            label(Some((3, 4)), Some((5, 5))),
            "base lines 3–4 and line 5"
        );
    }

    #[test]
    fn keys_make_spans() {
        let keys: Vec<String> = ["n10", "o9", "o10", "n11", "n12"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(spans_of_keys(&keys), (Some((9, 10)), Some((10, 12)), false));
        assert_eq!(
            spans_of_keys(&keys[..3]),
            (Some((9, 10)), Some((10, 10)), true)
        );
    }

    #[test]
    fn a_comment_follows_its_lines() {
        let it = sides(&[(' ', "fn f() {"), ('-', "a();"), ('+', "b();"), (' ', "}")]);
        // `b();` is line 11 of the PR, `a();` line 9 of the base.
        let n = LineNote {
            item: "c::f".into(),
            old: span(&it, true, 9, 9),
            new: span(&it, false, 11, 11),
            end_old: false,
            verdict: Verdict::Comment,
            text: "x".into(),
        };
        let p = place(&it, &n).unwrap();
        assert_eq!(p.end(), Some((false, 11)));
        assert!(p.covers(true, 9) && p.covers(false, 11) && !p.covers(false, 10));
        // Two lines added above in a later revision: the comment moves.
        let later = sides(&[
            ('+', "// one"),
            ('+', "// two"),
            (' ', "fn f() {"),
            ('-', "a();"),
            ('+', "b();"),
            (' ', "}"),
        ]);
        let p = place(&later, &n).unwrap();
        assert_eq!(p.new.map(|s| s.range()), Some((13, 13)));
        assert_eq!(p.old.map(|s| s.range()), Some((9, 9)));
        // The line changed: the comment is stale.
        let changed = sides(&[(' ', "fn f() {"), ('-', "a();"), ('+', "c();"), (' ', "}")]);
        assert_eq!(place(&changed, &n), None);
    }

    #[test]
    fn repeated_text_is_not_a_place() {
        let it = sides(&[(' ', "x();"), ('+', "y();"), (' ', "x();")]);
        let n = LineNote {
            item: "c::f".into(),
            old: None,
            new: span(&it, false, 10, 10),
            end_old: false,
            verdict: Verdict::Comment,
            text: "x".into(),
        };
        assert!(place(&it, &n).is_some());
        let later = sides(&[(' ', "z();"), (' ', "x();"), ('+', "y();"), (' ', "x();")]);
        // `x();` is now at lines 11 and 13: no telling which.
        assert_eq!(place(&later, &n), None);
    }

    #[test]
    fn lines_outside_the_item_have_no_span() {
        let it = sides(&[(' ', "a"), ('+', "b")]);
        assert!(span(&it, false, 9, 10).is_none());
        assert!(span(&it, false, 10, 12).is_none());
        assert!(span(&it, false, 10, 11).is_some());
    }
}
