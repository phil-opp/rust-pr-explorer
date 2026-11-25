//! Keyboard shortcuts of review mode: `j` and `k` for the next unreviewed
//! and the previous item, `n` and `p` for the hunks of the current item,
//! `a`, `r` and `c` for the verdicts, `Enter` to follow its next call of a
//! new or changed item that isn't reviewed yet, and `?` for the list of
//! shortcuts.
//!
//! The keys act on the current unit: the one Next ▸, a verdict or a key
//! last went to, if the open view shows it, else the first unreviewed one
//! in view. It is outlined (see [`mark_current`]), so that it is clear
//! what `a` approves.
//!
//! Only plain keys are used, never with Ctrl, Alt or ⌘, so that they don't
//! take the browser's shortcuts, and none while a text field has the focus.

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
        // Again closes the list. Over another dialog (a comment being
        // written), it does nothing, so that the dialog isn't replaced.
        if !modal_open() {
            crate::open_modal("Keyboard shortcuts", &cheat_sheet_html());
        } else if dom::query("#modal-b .keys").is_some() {
            crate::close_modal();
        } else {
            return false;
        }
        return true;
    }
    if modal_open()
        || !matches!(
            key.as_str(),
            "j" | "k" | "n" | "p" | "a" | "r" | "c" | "Enter"
        )
    {
        return false;
    }
    if !review::is_on() {
        if key == "Enter" {
            return false;
        }
        crate::toast("The review keys work in review mode: turn it on with Review in the header.");
        return true;
    }
    // Holding a verdict key down must not approve one item after another.
    if k.repeat() && matches!(key.as_str(), "a" | "r" | "c") {
        return true;
    }
    match key.as_str() {
        "j" => {
            let from = app.s.rv_current.clone();
            crate::go_next(app, from, false);
        }
        "k" => prev_item(app),
        "n" => step_hunk(app, true),
        "p" => step_hunk(app, false),
        "a" => verdict(app, Verdict::Approve),
        "r" => verdict(app, Verdict::Changes),
        "c" => verdict(app, Verdict::Comment),
        _ => follow_call(app),
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
        View::Overview | View::Module | View::Canvas | View::Calls => {
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

/// The row (or card) that a review control belongs to.
pub fn row_of(control: &Element) -> Element {
    dom::closest(control, ".hunk-rv, .irow, .impl-line, .ientry, .card")
        .unwrap_or_else(|| control.clone())
}

/// The hunk, else the unit, that was last gone to, if the open view
/// shows it.
fn last(app: &App) -> Option<(String, Element)> {
    if let (Some(h), Some(item)) = (&app.s.rv_hunk, &app.s.rv_current)
        && review::hunk_item(h).as_ref() == Some(item)
        && let Some(c) = control(app, h)
    {
        return Some((h.clone(), c));
    }
    let u = app.s.rv_current.clone()?;
    let c = control(app, &u)?;
    Some((u, c))
}

/// The unit (or hunk) the keys act on. See the module docs.
fn current(app: &mut App) -> Option<String> {
    if let Some((u, _)) = last(app) {
        return Some(u);
    }
    // The first unreviewed unit in view.
    let sec = section(app)?;
    let first = dom::query_all_in(&sec, ".rvc.open, .rvc.stale")
        .into_iter()
        .filter(|c| !dom::data(c, "rv").is_some_and(|id| id.contains('#')))
        .find(in_view)
        .and_then(|c| dom::data(&c, "rv"))?;
    app.s.rv_current = Some(first.clone());
    app.s.rv_hunk = None;
    Some(first)
}

/// Whether an element is (partly) in the window.
fn in_view(el: &Element) -> bool {
    let height = dom::window()
        .inner_height()
        .ok()
        .and_then(|h| h.as_f64())
        .unwrap_or(0.0);
    let r = el.get_bounding_client_rect();
    r.height() > 0.0 && r.bottom() > 0.0 && r.top() < height
}

/// Outlines the row of the current unit (or hunk), and only that one.
/// Called after every key, click and render, since rendering replaces the
/// rows.
pub fn mark_current(app: &App) {
    let now = review::is_on().then(|| last(app)).flatten();
    let row = now.map(|(_, c)| row_of(&c));
    for el in dom::query_all(".rv-cur") {
        if Some(&el) != row.as_ref() {
            dom::toggle_class(&el, "rv-cur", false);
        }
    }
    if let Some(row) = row {
        dom::toggle_class(&row, "rv-cur", true);
    }
}

fn no_current() {
    crate::toast("Nothing selected: press j to go to the next unreviewed item.");
}

/// `a`, `r`, `c`: like the verdict buttons of the current unit or hunk.
/// When it was scrolled out of view, the first press only shows it again,
/// so that a key never marks something that isn't on the screen.
fn verdict(app: &mut App, v: Verdict) {
    let Some(id) = current(app) else {
        no_current();
        return;
    };
    // The canvas moves its cards itself; `reveal` scrolls pages only.
    if app.s.view != View::Canvas
        && let Some(c) = control(app, &id)
        && !in_view(&row_of(&c))
    {
        crate::reveal(&c);
        crate::toast("This is the current item: press the key again to mark it.");
        return;
    }
    if v == Verdict::Approve {
        // Keeps a comment written earlier.
        let text = review::note(&id).text;
        // Like the first approve button: for an item with the same change
        // as others, "Approve all".
        crate::save_verdict(app, &id, v, text, true);
    } else {
        crate::open_editor(app, &id, v);
    }
}

/// `k`: the unit before the current one in review order (on the overview:
/// the one above it), reviewed or not,
/// shown whole if it was folded after its review.
fn prev_item(app: &mut App) {
    let Some(cur) = current(app) else {
        no_current();
        return;
    };
    let item = review::hunk_item(&cur).unwrap_or(cur);
    // On the overview, the unit above; elsewhere, in review order.
    let prev = match crate::overview_order(app) {
        Some(order) => order
            .iter()
            .position(|u| *u == item)
            .and_then(|i| i.checked_sub(1))
            .map(|i| order[i].clone()),
        None => review::prev_unit(&item),
    };
    let Some(prev) = prev else {
        crate::toast("This is the first item.");
        return;
    };
    if review::mark(&prev) == Some(review::Mark::Done) && !review::is_unfolded(&prev) {
        review::toggle_unfolded(&prev);
    }
    crate::show_unit(app, &prev, None);
}

/// `n`, `p`: the next or previous hunk of the current item. After the last
/// hunk (or in an item without hunks), `n` goes on to the next unreviewed
/// unit; before the first, `p` goes back to the item's own controls.
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
            crate::toast("This is the start of the item: k goes to the previous item.");
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
    let groups: [(&str, &[(&str, &str)]); 3] = [
        (
            "Move",
            &[
                (
                    "j / k",
                    "Next unreviewed item (in call order) / previous item",
                ),
                ("n / p", "Next / previous change (hunk) of the item"),
                (
                    "Enter",
                    "Follow the item's next call of a new or changed item that isn't reviewed yet",
                ),
            ],
        ),
        (
            "Review",
            &[
                (
                    "a",
                    "Approve the current item or change, and go on (on the canvas: stay, j goes on)",
                ),
                ("r", "Request changes (asks for a comment)"),
                ("c", "Comment"),
                ("Ctrl+Enter", "Save the comment"),
            ],
        ),
        (
            "Other",
            &[
                ("Ctrl+Z", "On the canvas: undo the last change to the cards"),
                ("Esc", "Close a dialog"),
                ("?", "Show or hide this list"),
            ],
        ),
    ];
    let rows: String = groups
        .iter()
        .map(|(title, rows)| {
            let rows: String = rows
                .iter()
                .map(|(k, what)| {
                    let keys: Vec<String> =
                        k.split(" / ").map(|k| format!("<kbd>{k}</kbd>")).collect();
                    format!("<tr><td>{}</td><td>{what}</td></tr>", keys.join(" / "))
                })
                .collect();
            format!(r#"<tr><th colspan="2">{title}</th></tr>{rows}"#)
        })
        .collect();
    format!(
        r#"<p class="dim">In review mode, unless you are typing. The keys act on the outlined item: the one that Next ▸ or a key last went to, else the first unreviewed one in view.</p><table class="keys">{rows}</table>"#
    )
}
