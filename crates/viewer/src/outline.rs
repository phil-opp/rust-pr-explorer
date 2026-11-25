//! The sidebar of the canvas and the call view: an outline of the changed
//! call graphs that the cards on the canvas (or the call view's focused
//! item) belong to, with each function's review state, so that the
//! reader sees where they are in a graph and what is left in it. A click
//! on a row shows the item's card, or opens it next to its caller.
//!
//! Also whether the sidebar is collapsed, which is remembered per view.

use crate::code::esc;
use crate::graphs::{self, Graph, Graphs};
use crate::review::{self, Mark, Verdict};
use crate::{App, View, dom};
use model::Status;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use web_sys::Element;

thread_local! {
    /// The PR's call graphs: they depend only on the analysis, and the
    /// outline is drawn again after every change to the canvas.
    static GRAPHS: RefCell<Option<Rc<Graphs>>> = const { RefCell::new(None) };
    /// The views whose sidebar the reader collapsed.
    static CLOSED: RefCell<Option<BTreeSet<String>>> = const { RefCell::new(None) };
}

fn all_graphs(app: &App) -> Rc<Graphs> {
    GRAPHS.with(|g| {
        g.borrow_mut()
            .get_or_insert_with(|| Rc::new(graphs::graphs(&app.d)))
            .clone()
    })
}

/// Whether the view shows the outline instead of the modules and files:
/// the canvas and the call view, when the PR has a changed call graph.
pub fn shown(app: &App) -> bool {
    matches!(app.s.view, View::Canvas | View::Calls) && !all_graphs(app).graphs.is_empty()
}

/// The items the view shows: the canvas's cards (every tab), or the
/// call view's focused item.
fn present(app: &App) -> Vec<String> {
    match app.s.view {
        View::Calls => app.s.calls.focus.iter().cloned().collect(),
        _ => crate::canvas::items_on_canvas(app),
    }
}

fn in_graph(g: &Graph, id: &str) -> bool {
    g.rows.iter().any(|r| r.id == id)
        || g.types.iter().any(|t| t == id)
        || g.tests.iter().any(|t| t == id)
        || g.helpers.iter().any(|t| t == id)
}

/// The review state of a changed item, in review mode: ✓, ✗ or ✎ for
/// its verdict, ↻ for a mark its diff has changed since.
fn state_html(id: &str) -> &'static str {
    if !review::is_on() {
        return "";
    }
    match review::mark(id) {
        Some(Mark::Done) => match review::note(id).verdict {
            Verdict::Approve => r#"<span class="rv-all" title="Approved">✓</span>"#,
            Verdict::Changes => r#"<span class="ol-chg" title="Changes requested">✗</span>"#,
            Verdict::Comment => r#"<span class="ol-cmt" title="Commented">✎</span>"#,
        },
        Some(Mark::Stale) => r#"<span class="ol-stale" title="Changed since your review">↻</span>"#,
        Some(Mark::Open) | None => "",
    }
}

/// The ids of a graph that can be reviewed, for its "n left" count.
fn reviewable(g: &Graph) -> impl Iterator<Item = &str> {
    g.rows
        .iter()
        .map(|r| r.id.as_str())
        .chain(g.types.iter().map(String::as_str))
}

/// "3 left" or ✓ for a graph, in review mode.
fn left_html(g: &Graph) -> String {
    if !review::is_on() {
        return String::new();
    }
    let ids: Vec<&str> = reviewable(g).collect();
    match review::open_count(ids.iter().copied()) {
        0 => r#"<span class="rv-all" title="All reviewed">✓</span>"#.to_string(),
        n => format!(
            r#"<span title="{n} of {} not reviewed yet">{n} left</span>"#,
            ids.len()
        ),
    }
}

/// A graph's name: its roots, at most three.
fn graph_name(app: &App, g: &Graph) -> String {
    let mut name = g
        .roots
        .iter()
        .take(3)
        .map(|r| esc(&app.d.display_name_of(r)))
        .collect::<Vec<_>>()
        .join(" · ");
    if g.roots.len() > 3 {
        name.push_str(&format!(" +{}", g.roots.len() - 3));
    }
    name
}

/// One item's row. `from` is the card a new card opens next to.
fn row_html(app: &App, id: &str, depth: usize, from: Option<&str>, on: &[String]) -> String {
    let d = &app.d;
    let Some(it) = d.item(id) else {
        return String::new();
    };
    let canvas = app.s.view == View::Canvas;
    let off = canvas && !on.iter().any(|o| o == id);
    let sel = match app.s.view {
        View::Calls => app.s.calls.focus.as_deref() == Some(id),
        _ => app.s.canvas_focus.as_deref() == Some(id),
    };
    let hint = match (app.s.view, off) {
        (View::Calls, _) => "focus it",
        (_, true) => "open it on the canvas",
        (_, false) => "show its card",
    };
    // Everything in a graph is changed, so only new and removed items are
    // marked, like in the overview's graphs; other cards can also be
    // unchanged.
    let tag = match it.status {
        Status::Added => r#"<span class="st st-added badge">new</span>"#,
        Status::Removed => r#"<span class="st st-removed badge">removed</span>"#,
        Status::Unchanged if !d.touched(it) => r#"<span class="ol-unch">unchanged</span>"#,
        _ => "",
    };
    format!(
        r#"<li style="--depth:{}"><div class="tree-row ol-row{}{}" data-ol="{}"{} title="{} · click to {hint}"><span class="ol-nm" data-kind="{}">{}</span>{tag}<span class="cnt">{}</span></div></li>"#,
        depth.min(5),
        if off { " off" } else { "" },
        if sel { " sel" } else { "" },
        esc(id),
        from.map_or(String::new(), |f| format!(r#" data-ol-from="{}""#, esc(f))),
        esc(id),
        it.info.kind.css(),
        esc(&d.display_name(it)),
        state_html(id),
    )
}

/// A graph as a call tree, its types and constants after its functions.
fn tree_html(app: &App, g: &Graph, on: &[String]) -> String {
    // The caller of each row: the last row above it one call less deep.
    let mut path: Vec<&str> = Vec::new();
    let mut rows = String::new();
    for r in &g.rows {
        path.truncate(r.depth);
        let from = path.last().copied();
        rows.push_str(&row_html(app, &r.id, r.depth, from, on));
        path.push(&r.id);
    }
    let types: String = g
        .types
        .iter()
        .map(|t| row_html(app, t, 0, None, on))
        .collect();
    let types = if types.is_empty() {
        String::new()
    } else {
        format!(r#"<div class="ol-sub">Types and constants</div><ul class="tree ol">{types}</ul>"#)
    };
    format!(
        r#"<div class="ol-g-h" title="{}"><span class="ol-g-nm">{}</span><span class="cnt">{}</span></div><ul class="tree ol">{rows}</ul>{types}"#,
        match g.rows.len() {
            1 => "1 function".to_string(),
            n => format!("{n} functions"),
        },
        graph_name(app, g),
        left_html(g),
    )
}

/// The sidebar's content.
pub fn html(app: &App) -> String {
    let gs = all_graphs(app);
    let on = present(app);
    type Listed<'a> = Vec<(usize, &'a Graph)>;
    let (current, other): (Listed, Listed) = gs
        .graphs
        .iter()
        .enumerate()
        .partition(|(_, g)| on.iter().any(|id| in_graph(g, id)));
    let mut out = String::new();
    if !current.is_empty() {
        out.push_str(&format!(
            r#"<div class="side-h">{}</div>"#,
            match (app.s.view, current.len()) {
                (View::Calls, _) => "Call graph",
                (_, 1) => "Call graph on the canvas",
                _ => "Call graphs on the canvas",
            }
        ));
        for (_, g) in &current {
            out.push_str(&tree_html(app, g, &on));
        }
    }
    // Cards outside those graphs: unchanged callees, types, tests.
    if app.s.view == View::Canvas {
        let rest: String = on
            .iter()
            .filter(|id| !current.iter().any(|(_, g)| in_graph(g, id)))
            .map(|id| row_html(app, id, 0, None, &on))
            .collect();
        if !rest.is_empty() {
            out.push_str(&format!(
                r#"<div class="side-h">Other cards</div><ul class="tree ol">{rest}</ul>"#
            ));
        }
    }
    if !other.is_empty() {
        let rows: String = other
            .iter()
            .map(|(i, g)| {
                format!(
                    r#"<li><div class="tree-row ol-graph" data-ol-graph="{i}" title="{}"><span class="ol-g-nm">{}</span><span class="cnt">{}</span></div></li>"#,
                    if app.s.view == View::Canvas {
                        "Open the graph's functions on the canvas"
                    } else {
                        "Focus the graph's root"
                    },
                    graph_name(app, g),
                    match left_html(g) {
                        s if s.is_empty() => g.rows.len().to_string(),
                        s => s,
                    }
                )
            })
            .collect();
        out.push_str(&format!(
            r#"<div class="side-h">{} <span class="side-h-n">{}</span></div><ul class="tree ol">{rows}</ul>"#,
            if current.is_empty() {
                "Changed call graphs"
            } else {
                "Other call graphs"
            },
            other.len()
        ));
    }
    let legend = match app.s.view {
        View::Canvas => {
            "Functions in call order, callees indented. Names in gray italics have no card yet: a click opens one next to its caller, any other name shows its card. The sidebar lists modules and files on the Overview and module pages."
        }
        _ => {
            "Functions in call order, callees indented. A click focuses one. The sidebar lists modules and files on the Overview and module pages."
        }
    };
    out.push_str(&format!(
        r#"<div class="side-legend"><div>{legend}</div></div>"#
    ));
    out
}

/// A click in the outline. Returns whether it was one.
pub fn on_click(app: &mut App, el: &Element) -> bool {
    if let Some(r) = dom::closest(el, "[data-ol]") {
        let id = dom::data(&r, "ol").unwrap_or_default();
        if app.s.view == View::Calls {
            crate::calls::focus(app, &id);
        } else {
            let on = crate::canvas::items_on_canvas(app);
            // Next to its caller in the graph, else next to any caller
            // that has a card.
            let from = dom::data(&r, "ol-from")
                .filter(|f| on.contains(f))
                .or_else(|| {
                    app.d
                        .callers
                        .get(&id)
                        .and_then(|cs| cs.iter().find(|c| on.contains(c)).cloned())
                });
            crate::canvas::open(app, &id, from.as_deref(), true);
        }
        return true;
    }
    if let Some(r) = dom::closest(el, "[data-ol-graph]") {
        let i = dom::data(&r, "ol-graph")
            .and_then(|i| i.parse().ok())
            .unwrap_or(0);
        let ids = graphs::graph_items(&app.d, i);
        if app.s.view == View::Calls || crate::narrow() {
            if let Some(root) = ids.first() {
                crate::calls::focus(app, root);
            }
        } else {
            crate::canvas::show_items(app, &ids);
        }
        return true;
    }
    false
}

/// Hovering a row outlines its card.
pub fn on_hover(app: &App, el: &Element) {
    if app.s.view == View::Canvas
        && let Some(r) = dom::closest(el, "[data-ol]")
    {
        crate::canvas::highlight_card_of(app, &dom::data(&r, "ol").unwrap_or_default());
    }
}

// Keeps the tool's old name, like the other settings.
const CLOSED_KEY: &str = "rust-pr-explorer.sidebar-closed";

fn storage() -> Option<web_sys::Storage> {
    dom::window().local_storage().ok().flatten()
}

fn with_closed<R>(f: impl FnOnce(&mut BTreeSet<String>) -> R) -> R {
    CLOSED.with(|c| {
        let mut c = c.borrow_mut();
        let set = c.get_or_insert_with(|| {
            storage()
                .and_then(|s| s.get_item(CLOSED_KEY).ok().flatten())
                .map(|v| {
                    v.split(',')
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default()
        });
        f(set)
    })
}

/// Whether the reader collapsed the sidebar of `view`.
pub fn closed(view: View) -> bool {
    with_closed(|c| c.contains(view.name()))
}

/// Collapses or opens the sidebar of `view`, and remembers it.
pub fn toggle_closed(view: View) {
    let list = with_closed(|c| {
        if !c.remove(view.name()) {
            c.insert(view.name().to_string());
        }
        c.iter().cloned().collect::<Vec<_>>().join(",")
    });
    if let Some(s) = storage() {
        let _ = s.set_item(CLOSED_KEY, &list);
    }
}

/// What the sidebar shows, for its buttons' labels.
fn what(app: &App) -> &'static str {
    if shown(app) {
        "the call graph"
    } else {
        "modules and files"
    }
}

/// Collapses the sidebar if the reader did so in this view (on a wide
/// screen: on a narrow one it is a drawer anyway), and labels its buttons.
pub fn apply_closed(app: &App) {
    if let Some(body) = dom::document().body() {
        dom::toggle_class(&body, "side-closed", closed(app.s.view));
    }
    let what = what(app);
    for (id, title) in [
        ("side-btn", format!("Show {what}")),
        ("side-hide", format!("Hide {what}")),
        ("side-show", format!("Show {what}")),
    ] {
        if let Some(b) = dom::by_id(id) {
            let _ = b.set_attribute("title", &title);
            let _ = b.set_attribute("aria-label", &title);
        }
    }
}
