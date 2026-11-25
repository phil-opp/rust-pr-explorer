//! The call view: the call graph one item at a time, for narrow screens
//! where the canvas needs a lot of panning and zooming. The focused item's
//! card is in the middle, what calls it (or uses it) above, what it calls
//! below; on a wide screen they are columns left and right of the card. A
//! tap on a caller, a callee or any name in the card focuses that item,
//! and the trail at the top leads back.
//!
//! On a phone, the links that elsewhere open the canvas (an item's
//! "Open ▸", "Internal call graph ▸", a name on a page) open this view
//! instead; see [`crate::open_item`].

use crate::code::esc;
use crate::data::{Data, changed};
use crate::review::{self, Mark};
use crate::{App, View, dom};
use model::{Item, Kind};
use std::collections::BTreeSet;
use web_sys::Element;

/// The trail is cut to this many items.
const TRAIL: usize = 30;
/// A list shows at most this many changed items, and fills up to
/// [`ROWS`] with unchanged ones, until "Show all" is pressed: an item
/// with hundreds of callers would otherwise bury the card.
const CHANGED_ROWS: usize = 30;
const ROWS: usize = 12;

#[derive(Default)]
pub struct Calls {
    /// The focused item.
    pub focus: Option<String>,
    /// The items focused before, oldest first.
    trail: Vec<String>,
    /// Unchanged items whose card shows docs and source.
    expanded: BTreeSet<String>,
    /// The lists (by section class) shown whole for the current focus.
    show_all: BTreeSet<String>,
}

/// Focuses `id` and shows the call view: from the view's own links, the
/// trail grows (or, for an item already on it, goes back to it).
pub fn focus(app: &mut App, id: &str) {
    // On a phone the card is below the callers: start there.
    focus_at(app, id, "#view-calls .cg-focus");
}

/// Focuses `id` like [`focus`], but starts at what calls or uses it,
/// which on a phone is above the card: for "Show callers", whose callers
/// would otherwise be out of sight, and which would do nothing visible
/// when pressed on the focused item's own card.
pub fn show_callers(app: &mut App, id: &str) {
    focus_at(app, id, "#view-calls .cg-above");
}

/// Focuses `id` and scrolls the element matching `start` to the top.
fn focus_at(app: &mut App, id: &str, start: &'static str) {
    if app.d.item(id).is_none() {
        return;
    }
    let s = &mut app.s.calls;
    if s.focus.as_deref() != Some(id) {
        if let Some(i) = s.trail.iter().position(|t| t == id) {
            s.trail.truncate(i);
        } else if let Some(f) = s.focus.take() {
            s.trail.push(f);
            if s.trail.len() > TRAIL {
                s.trail.remove(0);
            }
        }
        s.focus = Some(id.to_string());
        s.show_all.clear();
    }
    if app.s.view == View::Calls {
        render(app);
        crate::nav::record(app);
    } else {
        crate::set_view(app, View::Calls);
    }
    if let Some(sec) = dom::html_by_id("view-calls") {
        sec.set_scroll_top(0);
    }
    dom::raf(move || {
        if let Some(el) = dom::query(start) {
            let opts = web_sys::ScrollIntoViewOptions::new();
            opts.set_block(web_sys::ScrollLogicalPosition::Start);
            el.scroll_into_view_with_scroll_into_view_options(&opts);
        }
    });
}

/// The callees of an item, without repeats, in the order it calls them.
fn callees(d: &Data, it: &Item) -> Vec<String> {
    let mut seen = BTreeSet::new();
    it.info
        .calls
        .iter()
        .map(|c| c.to.clone())
        .filter(|t| d.item(t).is_some() && seen.insert(t.clone()))
        .collect()
}

fn callers(d: &Data, id: &str) -> Vec<String> {
    let mut v: Vec<String> = d.callers.get(id).map_or(Vec::new(), |v| v.to_vec());
    v.sort();
    v.dedup();
    v.retain(|t| d.item(t).is_some());
    v
}

fn users(d: &Data, id: &str) -> Vec<String> {
    let mut v: Vec<String> = d
        .used_by
        .get(id)
        .map_or(Vec::new(), |v| v.iter().map(|(f, _)| f.clone()).collect());
    v.sort();
    v.dedup();
    v.retain(|t| d.item(t).is_some());
    v
}

/// Changed items first; the rest is context.
fn changed_first(d: &Data, list: &mut [String]) {
    list.sort_by_key(|t| !d.item(t).is_some_and(|it| changed(it) || d.touched(it)));
}

/// One related item: a row to tap, with how many callers and callees it
/// has in turn, so that one can tell where the graph goes on.
fn row_html(d: &Data, id: &str) -> String {
    let Some(it) = d.item(id) else {
        return String::new();
    };
    let n_in = callers(d, id).len();
    let n_out = callees(d, it).len();
    let counts = match (n_in, n_out) {
        (0, 0) => String::new(),
        _ => format!(
            r#"<span class="cg-n" title="{n_in} caller{}, calls {n_out}">↑{n_in} ↓{n_out}</span>"#,
            if n_in == 1 { "" } else { "s" }
        ),
    };
    let done = if review::mark(id) == Some(Mark::Done) {
        r#"<span class="cg-done" title="Reviewed">✓</span>"#
    } else {
        ""
    };
    let dot = if !changed(it) && d.touched(it) {
        "changed"
    } else {
        it.status.css()
    };
    let context = !changed(it) && !d.touched(it);
    format!(
        r#"<button class="cg-row{}" data-cfocus="{}" title="{}"><span class="dot {dot}"></span><span class="cg-name"><span class="kind">{}</span> {}{}<span class="cg-mod">{}</span></span>{done}{counts}</button>"#,
        if context { " context" } else { "" },
        esc(id),
        esc(&format!("Focus {}", d.short_path(id))),
        it.info.kind.label(),
        esc(&d.display_name(it)),
        crate::unsafety::tag(it),
        esc(&d.mod_label(d.mod_of(it))),
    )
}

fn section_html(
    d: &Data,
    s: &Calls,
    class: &str,
    title: &str,
    empty: &str,
    list: &[String],
) -> String {
    let all = s.show_all.contains(class);
    let (mut changed_n, mut shown_n) = (0, 0);
    let mut rows = String::new();
    let (mut hidden_changed, mut hidden) = (0, 0);
    for t in list {
        let ch = d.item(t).is_some_and(|it| changed(it) || d.touched(it));
        let show = all
            || if ch {
                changed_n < CHANGED_ROWS
            } else {
                shown_n < ROWS
            };
        if show {
            rows.push_str(&row_html(d, t));
            shown_n += 1;
            changed_n += usize::from(ch);
        } else {
            hidden += 1;
            hidden_changed += usize::from(ch);
        }
    }
    if hidden > 0 {
        let what = match (hidden_changed, hidden - hidden_changed) {
            (0, u) => format!("{u} unchanged"),
            (c, 0) => format!("{c} changed"),
            (c, u) => format!("{c} changed, {u} unchanged"),
        };
        rows.push_str(&format!(
            r#"<button class="cg-more" data-cmore="{class}">Show {hidden} more ({what})</button>"#
        ));
    }
    format!(
        r#"<section class="cg-sec {class}"><h2>{title} <span class="cg-count">{}</span></h2>{}</section>"#,
        list.len(),
        if rows.is_empty() {
            format!(r#"<div class="cg-empty">{empty}</div>"#)
        } else {
            format!(r#"<div class="cg-list">{rows}</div>"#)
        }
    )
}

/// The trail of items focused before, and the focused one.
fn trail_html(d: &Data, s: &Calls) -> String {
    let mut parts: Vec<String> = s
        .trail
        .iter()
        .filter_map(|t| d.item(t))
        .map(|it| {
            format!(
                r#"<a data-cfocus="{}">{}</a>"#,
                esc(&it.info.id),
                esc(&d.display_name(it))
            )
        })
        .collect();
    if let Some(it) = s.focus.as_deref().and_then(|f| d.item(f)) {
        parts.push(format!(
            r#"<span class="cg-here">{}</span>"#,
            esc(&d.display_name(it))
        ));
    }
    format!(
        r#"<nav class="cg-trail" aria-label="Items focused before">{}</nav>"#,
        parts.join(r#"<span class="cg-sep">›</span>"#)
    )
}

/// Where to start without a focus: the changed functions, those that no
/// other changed item calls first (where a review usually starts). With a
/// module (focused, or picked as "Internal call graph"), only the module's
/// own changed items, and "other" means other in that module.
fn start_html(d: &Data, s: &Calls, module: Option<&str>) -> String {
    let in_scope = |it: &Item| module.is_none_or(|m| d.mod_of(it) == m);
    let changed_here = |it: &Item| {
        (changed(it) || d.touched(it)) && in_scope(it) && crate::commit_filter::item_in(&it.info.id)
    };
    let mut fns: Vec<&Item> = d
        .items()
        .filter(|it| it.info.kind.is_callable() && changed_here(it))
        .collect();
    let called_by_changed = |it: &Item| {
        callers(d, &it.info.id)
            .iter()
            .any(|c| d.item(c).is_some_and(&changed_here))
    };
    fns.sort_by(|a, b| {
        called_by_changed(a)
            .cmp(&called_by_changed(b))
            .then_with(|| d.reading_order(a, b))
    });
    let (top, rest): (Vec<&Item>, Vec<&Item>) =
        fns.into_iter().partition(|it| !called_by_changed(it));
    let ids = |v: &[&Item]| v.iter().map(|it| it.info.id.clone()).collect::<Vec<_>>();
    let (title, intro, top_title, others) = match module.and_then(|m| d.item(m)) {
        Some(m) => {
            let mut others: Vec<&Item> = d
                .items()
                .filter(|it| {
                    !it.info.kind.is_callable()
                        && !matches!(it.info.kind, Kind::Mod | Kind::Impl)
                        && changed_here(it)
                })
                .collect();
            others.sort_by(|a, b| d.reading_order(a, b));
            (
                format!(
                    "Calls in <code>{}</code>",
                    esc(&d.mod_label(&m.info.id))
                ),
                format!(
                    r#"The changed items of this module. Tap one to see what calls it and what it calls. <a data-mod="{}"{}>Module page</a>"#,
                    esc(&m.info.id),
                    crate::nav::module_href(&m.info.id)
                ),
                "Changed functions nothing else here calls",
                ids(&others),
            )
        }
        None => (
            "Calls".to_string(),
            "The call graph one item at a time: what calls it above, what it calls below. Tap an item to focus it.".to_string(),
            "Changed functions nothing changed calls",
            Vec::new(),
        ),
    };
    let mut html = format!(
        r#"<div class="cg-start"><h1>{title}</h1><p class="sub">{intro}</p>{}"#,
        section_html(d, s, "cg-roots", top_title, "None.", &ids(&top))
    );
    if !rest.is_empty() {
        html.push_str(&section_html(
            d,
            s,
            "cg-rest",
            "Other changed functions",
            "None.",
            &ids(&rest),
        ));
    }
    if !others.is_empty() {
        html.push_str(&section_html(
            d,
            s,
            "cg-other",
            "Other changed items",
            "None.",
            &others,
        ));
    }
    html.push_str("</div>");
    html
}

pub fn render(app: &App) {
    let d = &app.d;
    let s = &app.s.calls;
    let Some(it) = s.focus.as_deref().and_then(|f| d.item(f)) else {
        dom::set_html("view-calls", &start_html(d, s, None));
        return;
    };
    // A module: its changed items, to pick one from.
    if it.info.kind == Kind::Mod {
        dom::set_html(
            "view-calls",
            &(trail_html(d, s) + &start_html(d, s, Some(&it.info.id))),
        );
        return;
    }
    let id = it.info.id.as_str();
    let mut up = callers(d, id);
    changed_first(d, &mut up);
    let mut down = callees(d, it);
    changed_first(d, &mut down);
    let mut used = users(d, id);
    changed_first(d, &mut used);
    let is_fn = it.info.kind.is_callable();
    let mut above = String::new();
    if is_fn || !up.is_empty() {
        above.push_str(&section_html(
            d,
            s,
            "cg-callers",
            "Called by",
            "Nothing in the analyzed code calls this.",
            &up,
        ));
    }
    if !used.is_empty() || !is_fn && it.info.kind != Kind::Mod {
        above.push_str(&section_html(
            d,
            s,
            "cg-users",
            "Used by",
            "Nothing in the analyzed code uses this.",
            &used,
        ));
    }
    let below = if is_fn || !down.is_empty() {
        let unresolved = match it.info.unresolved_calls {
            0 => String::new(),
            n => format!(
                r#"<div class="cg-empty">{n} call site{} not resolved.</div>"#,
                if n > 1 { "s" } else { "" }
            ),
        };
        section_html(
            d,
            s,
            "cg-callees",
            "Calls",
            "Calls nothing in the analyzed code.",
            &down,
        ) + &unresolved
    } else {
        String::new()
    };
    let card = crate::canvas::card_html_for(d, d.mod_of(it), id, s.expanded.contains(id));
    let tools = format!(
        r#"<div class="cg-tools"><button data-open-canvas="{}" title="Place this item on the canvas">Open on canvas</button></div>"#,
        esc(id)
    );
    dom::set_html(
        "view-calls",
        &format!(
            r#"{}<div class="cg-grid"><div class="cg-above">{above}</div><div class="cg-focus">{card}{tools}</div><div class="cg-below">{below}</div></div>"#,
            trail_html(d, s)
        ),
    );
}

/// Handles a click inside the call view. Returns whether it was consumed.
pub fn on_click(app: &mut App, el: &Element) -> bool {
    if let Some(b) = dom::closest(el, "[data-cmore]") {
        let class = dom::data(&b, "cmore").unwrap_or_default();
        app.s.calls.show_all.insert(class);
        render(app);
        return true;
    }
    if let Some(r) = dom::closest(el, "[data-cfocus]") {
        let id = dom::data(&r, "cfocus").unwrap_or_default();
        focus(app, &id);
        return true;
    }
    // A name in the card: focus its item, as a click on the canvas would
    // place it next to the card.
    if let Some(a) = dom::closest(el, ".card a.ref[data-id]") {
        let id = dom::data(&a, "id").unwrap_or_default();
        if app.d.item(&id).is_some() {
            focus(app, &id);
            return true;
        }
        return false;
    }
    let Some(focused) = app.s.calls.focus.clone() else {
        return false;
    };
    match dom::closest(el, "[data-act]")
        .and_then(|b| dom::data(&b, "act"))
        .as_deref()
    {
        Some("expand") => {
            app.s.calls.expanded.insert(focused);
            render(app);
        }
        Some("fold") => {
            app.s.calls.expanded.remove(&focused);
            render(app);
        }
        Some("page" | "drill") => {
            app.s.module = focused;
            crate::set_view(app, View::Module);
        }
        // The rest of the card's buttons ("calls", "members as tabs", …)
        // belong to the canvas and are hidden here.
        _ => return false,
    }
    true
}
