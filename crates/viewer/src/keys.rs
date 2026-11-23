//! Keyboard shortcuts of review mode: `n` for the next unreviewed unit,
//! `a`, `r` and `c` for the verdicts, `j` and `k` for the hunks of the
//! current item, `Enter` to follow its next call of a new or changed item
//! that isn't reviewed yet, and `?` for the list of shortcuts.
//!
//! The keys act on the current unit: the one Next ▸, a verdict or a key
//! last went to, if the open view shows it, else the first unreviewed one
//! in view.

use crate::review::{self, Verdict};
use crate::{App, View, dom};
use wasm_bindgen::JsCast;
use web_sys::{Element, KeyboardEvent};

/// Handles a key press. Returns whether it was a shortcut, so that the
/// key does nothing else.
pub fn on_key(app: &mut App, k: &KeyboardEvent) -> bool {
    // Ctrl+Z (⌘Z) on the canvas undoes the last change to its cards.
    // In a text field it stays the field's own undo.
    if app.s.view == View::Canvas
        && (k.ctrl_key() || k.meta_key())
        && !k.shift_key()
        && !k.alt_key()
        && k.key().eq_ignore_ascii_case("z")
        && !typing(k)
        && !modal_open()
    {
        crate::canvas::undo(app);
        return true;
    }
    if k.ctrl_key() || k.meta_key() || k.alt_key() || typing(k) {
        return false;
    }
    let key = k.key();
    if key == "?" {
        crate::open_modal("Keyboard shortcuts", &cheat_sheet_html());
        return true;
    }
    if !review::is_on() || modal_open() {
        return false;
    }
    match key.as_str() {
        "n" => {
            let from = app.s.rv_current.clone();
            crate::go_next(app, from, false);
        }
        "a" => verdict(app, Verdict::Approve),
        "r" => verdict(app, Verdict::Changes),
        "c" => verdict(app, Verdict::Comment),
        "j" => step_hunk(app, true),
        "k" => step_hunk(app, false),
        "Enter" => follow_call(app),
        _ => return false,
    }
    true
}

/// Whether the key goes to a text field, or (Enter) to a focused button or
/// link, which it activates.
fn typing(k: &KeyboardEvent) -> bool {
    let Some(t) = k.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
        return false;
    };
    let tag = t.tag_name();
    matches!(tag.as_str(), "INPUT" | "TEXTAREA" | "SELECT")
        || t.dyn_ref::<web_sys::HtmlElement>()
            .is_some_and(|h| h.is_content_editable())
        || (k.key() == "Enter" && matches!(tag.as_str(), "BUTTON" | "A" | "SUMMARY"))
}

fn modal_open() -> bool {
    dom::by_id("modal").is_some_and(|m| !m.class_list().contains("hidden"))
}

/// The open view, if it shows review controls.
fn section(app: &App) -> Option<Element> {
    match app.s.view {
        View::Overview | View::Module | View::Canvas => {
            dom::by_id(&format!("view-{}", app.s.view.name()))
        }
        View::Space => None,
    }
}

/// The review control of a unit (or hunk) in the open view.
fn control(app: &App, unit: &str) -> Option<Element> {
    let sec = section(app)?;
    dom::query_all_in(&sec, ".rvc")
        .into_iter()
        .find(|c| dom::data(c, "rv").as_deref() == Some(unit))
}

/// The unit (or hunk) the keys act on. See the module docs.
fn current(app: &mut App) -> Option<String> {
    if let (Some(h), Some(item)) = (&app.s.rv_hunk, &app.s.rv_current)
        && review::hunk_item(h).as_ref() == Some(item)
        && control(app, h).is_some()
    {
        return Some(h.clone());
    }
    if let Some(u) = app.s.rv_current.clone()
        && control(app, &u).is_some()
    {
        return Some(u);
    }
    // The first unreviewed unit in view.
    let height = dom::window()
        .inner_height()
        .ok()
        .and_then(|h| h.as_f64())
        .unwrap_or(0.0);
    let sec = section(app)?;
    let first = dom::query_all_in(&sec, ".rvc.open, .rvc.stale")
        .into_iter()
        .filter(|c| !dom::data(c, "rv").is_some_and(|id| id.contains('#')))
        .find(|c| {
            let r = c.get_bounding_client_rect();
            r.height() > 0.0 && r.bottom() > 0.0 && r.top() < height
        })
        .and_then(|c| dom::data(&c, "rv"))?;
    app.s.rv_current = Some(first.clone());
    app.s.rv_hunk = None;
    Some(first)
}

fn no_current() {
    crate::toast("Nothing selected: press n to go to the next unreviewed item.");
}

/// `a`, `r`, `c`: like the verdict buttons of the current unit or hunk.
fn verdict(app: &mut App, v: Verdict) {
    let Some(id) = current(app) else {
        no_current();
        return;
    };
    if v == Verdict::Approve {
        // Keeps a comment written earlier.
        let text = review::note(&id).text;
        crate::save_verdict(app, &id, v, text);
    } else {
        crate::open_editor(app, &id, v);
    }
}

/// `j`, `k`: the next or previous hunk of the current item. After the last
/// hunk (or in an item without hunks), `j` goes on to the next unreviewed
/// unit; before the first, `k` goes back to the item's own controls.
fn step_hunk(app: &mut App, forward: bool) {
    let Some(cur) = current(app) else {
        no_current();
        return;
    };
    let item = review::hunk_item(&cur).unwrap_or(cur);
    let hunks = review::hunks_of(&item);
    let at = app
        .s
        .rv_hunk
        .as_ref()
        .and_then(|h| hunks.iter().position(|x| &x.id == h));
    let to = match (forward, at) {
        (true, None) if !hunks.is_empty() => Some(0),
        (true, Some(i)) if i + 1 < hunks.len() => Some(i + 1),
        (true, _) => {
            crate::go_next(app, Some(item), false);
            return;
        }
        (false, Some(i)) if i > 0 => Some(i - 1),
        (false, Some(_)) => None,
        (false, None) => {
            crate::toast("This is the start of the item.");
            return;
        }
    };
    app.s.rv_current = Some(item.clone());
    app.s.rv_hunk = to.map(|i| hunks[i].id.clone());
    let target = app.s.rv_hunk.clone().unwrap_or(item);
    match control(app, &target) {
        Some(c) => crate::reveal(&c),
        None => crate::toast("This change isn't shown here: open the item's source."),
    }
}

/// `Enter`: the next call (or name) in the current item's source of an
/// item the PR adds or changes that isn't reviewed yet, as the source
/// diff tags them.
fn follow_call(app: &mut App) {
    let Some(cur) = current(app) else {
        no_current();
        return;
    };
    let item = review::hunk_item(&cur).unwrap_or(cur);
    match review::next_open_call(&app.d, &item) {
        Some(next) => crate::show_unit(app, &next, Some(&item)),
        None => crate::toast("Every new or changed item this one calls is reviewed."),
    }
}

/// The list that `?` shows.
pub fn cheat_sheet_html() -> String {
    let rows = [
        ("n", "Next unreviewed item, in call order"),
        ("a", "Approve the current item or change, and go on"),
        ("r", "Request changes (asks for a comment)"),
        ("c", "Comment"),
        ("j / k", "Next / previous change (hunk) of the item"),
        (
            "Enter",
            "Follow the item's next call of a new or changed item that isn't reviewed yet",
        ),
        ("Ctrl+Enter", "Save the comment"),
        ("Ctrl+Z", "On the canvas: undo the last change to the cards"),
        ("Esc", "Close a dialog"),
        ("?", "This list"),
    ];
    let rows: String = rows
        .iter()
        .map(|(k, what)| {
            let keys: Vec<String> = k.split(" / ").map(|k| format!("<kbd>{k}</kbd>")).collect();
            format!("<tr><td>{}</td><td>{what}</td></tr>", keys.join(" / "))
        })
        .collect();
    format!(
        r#"<p class="dim">In review mode, unless you are typing. The keys act on the item that Next ▸ or a key last went to, else on the first unreviewed one in view.</p><table class="keys">{rows}</table>"#
    )
}
