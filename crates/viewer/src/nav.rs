//! Browser history for the views. Each view change, each jump to an item
//! further down a page, and each item opened on the canvas or the call
//! view from elsewhere gets its own entry in the URL's fragment
//! (`#/module/<id>`, `#/overview/item/<id>`, `#/canvas/<id>`, …), so that
//! the browser's Back and Forward buttons move between them, and a link to
//! a fragment opens that view.
//!
//! The fragment is set through `location.hash`, not `history.pushState`:
//! a fragment change is allowed wherever the page is shown, also as a local
//! file and when it is served from an opaque origin (a sandboxed workflow
//! artifact). For the same reason the scroll position of each entry is kept
//! here, by its fragment, and not in the entry's state; the browser can't
//! restore it itself, since the pages scroll inside their view, not the
//! window.
//!
//! The same fragments are the `href` of the links and of the buttons that
//! switch views (see [`href`]), so that a middle click or Ctrl+click opens
//! the view in a new tab, while a plain click is handled by the app (see
//! `lib.rs`). A canvas that shows a call graph has a route of its own
//! ([`OnCanvas`]), which a new tab lays out again.
//!
//! Stepping through the review (Next ▸, `j`, `k`) adds one entry for the
//! first step that changes the view and replaces it for the steps after,
//! so that Back returns to where the review steps started instead of going
//! through every unit.

use crate::{App, View, dom};
use model::Kind;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use web_sys::Event;

/// A place further down a page that a link jumped to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Anchor {
    /// An item's row.
    Item(String),
    /// A changed file's entry on the overview, by its path.
    File(String),
}

/// The anchor of the page that the app shows, with the page it is on.
pub struct PageAnchor {
    pub view: View,
    /// The module of a module page.
    pub module: String,
    pub to: Anchor,
}

/// What the canvas was asked to show from outside it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OnCanvas {
    /// An item, added to the cards there are.
    Item(String),
    /// Items in place of the cards there are, arranged by module: a call
    /// graph.
    Items(Vec<String>),
    /// A module's internal call graph (see `canvas::show_module_graph`).
    ModuleGraph(String),
    /// The chains from the public API down to an item (see
    /// `canvas::show_impact`).
    Impact(String),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Route {
    Overview(Option<Anchor>),
    Module(String, Option<Anchor>),
    /// The canvas, with what it was last asked to show from elsewhere.
    Canvas(Option<OnCanvas>),
    /// The call view, focused on an item.
    Calls(Option<String>),
    Space,
}

impl Route {
    pub fn hash(&self) -> String {
        let at = |a: &Option<Anchor>| match a {
            None => String::new(),
            Some(Anchor::Item(id)) => format!("/item/{}", enc(id)),
            Some(Anchor::File(path)) => format!("/file/{}", enc(path)),
        };
        match self {
            Route::Overview(None) => "#/".to_string(),
            Route::Overview(a) => format!("#/overview{}", at(a)),
            Route::Module(m, a) => format!("#/module/{}{}", enc(m), at(a)),
            Route::Canvas(None) => "#/canvas".to_string(),
            Route::Canvas(Some(OnCanvas::Item(id))) => format!("#/canvas/{}", enc(id)),
            Route::Canvas(Some(OnCanvas::Items(ids))) => {
                let ids: Vec<String> = ids.iter().map(|id| enc(id)).collect();
                format!("#/graph/{}", ids.join(","))
            }
            Route::Canvas(Some(OnCanvas::ModuleGraph(m))) => format!("#/module-graph/{}", enc(m)),
            Route::Canvas(Some(OnCanvas::Impact(id))) => format!("#/impact/{}", enc(id)),
            Route::Calls(None) => "#/calls".to_string(),
            Route::Calls(Some(id)) => format!("#/calls/{}", enc(id)),
            Route::Space => "#/structure".to_string(),
        }
    }

    /// The route of a fragment, with or without its `#`. Anything unknown,
    /// including no fragment, is the overview.
    pub fn parse(hash: &str, decode: impl Fn(&str) -> Option<String>) -> Route {
        let path = hash.trim_start_matches('#').trim_start_matches('/');
        // Ids and paths are encoded, so `/` only separates the parts.
        let parts: Vec<&str> = path.split('/').collect();
        let arg = |i: usize| {
            parts
                .get(i)
                .filter(|a| !a.is_empty())
                .and_then(|a| decode(a))
        };
        let anchor = |i: usize| match parts.get(i).copied() {
            Some("item") => arg(i + 1).map(Anchor::Item),
            Some("file") => arg(i + 1).map(Anchor::File),
            _ => None,
        };
        match parts[0] {
            "overview" => Route::Overview(anchor(1)),
            "module" => match arg(1) {
                Some(m) => Route::Module(m, anchor(2)),
                None => Route::Overview(None),
            },
            "canvas" => Route::Canvas(arg(1).map(OnCanvas::Item)),
            // Ids are encoded, so `,` only separates them.
            "graph" => {
                let ids: Vec<String> = parts
                    .get(1)
                    .into_iter()
                    .flat_map(|list| list.split(','))
                    .filter(|id| !id.is_empty())
                    .filter_map(&decode)
                    .collect();
                Route::Canvas((!ids.is_empty()).then_some(OnCanvas::Items(ids)))
            }
            "module-graph" => Route::Canvas(arg(1).map(OnCanvas::ModuleGraph)),
            "impact" => Route::Canvas(arg(1).map(OnCanvas::Impact)),
            "calls" => Route::Calls(arg(1)),
            "structure" => Route::Space,
            _ => Route::Overview(None),
        }
    }

    /// The element that scrolls the route's view, for a view that scrolls
    /// like a page.
    fn scroller(&self) -> Option<&'static str> {
        match self {
            Route::Overview(_) => Some("view-overview"),
            Route::Module(..) => Some("view-module"),
            Route::Calls(_) => Some("view-calls"),
            Route::Canvas(_) | Route::Space => None,
        }
    }
}

/// Encodes a part of a fragment like `encodeURIComponent`, which also
/// works where tests run (natively, without JavaScript).
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// An `href` attribute that leads to `route`, for a link that the app
/// handles on a plain click: a middle click or Ctrl+click opens the route
/// in a new tab.
pub fn href(route: &Route) -> String {
    // The fragment is encoded: no quotes or `&` to escape but `'`, which
    // can't end a double-quoted attribute.
    format!(r#" href="{}""#, route.hash())
}

/// The `href` of a link to an item, which opens it on the canvas.
pub fn item_href(id: &str) -> String {
    href(&Route::Canvas(Some(OnCanvas::Item(id.to_string()))))
}

/// The `href` of a link to a module's page.
pub fn module_href(m: &str) -> String {
    href(&Route::Module(m.to_string(), None))
}

/// The `href` of a link to an item's row on its module page.
pub fn row_href(id: &str) -> String {
    // The overview's anchors of items lead to their module page (see `show`).
    href(&Route::Overview(Some(Anchor::Item(id.to_string()))))
}

/// The `href` of a link to a changed file's entry on the overview.
pub fn file_href(path: &str) -> String {
    href(&Route::Overview(Some(Anchor::File(path.to_string()))))
}

fn decode(s: &str) -> Option<String> {
    js_sys::decode_uri_component(s).ok().map(String::from)
}

thread_local! {
    /// The route of the current history entry.
    static CURRENT: RefCell<Route> = const { RefCell::new(Route::Overview(None)) };
    /// The last scroll position of each entry's view, by fragment.
    static SCROLL: RefCell<HashMap<String, i32>> = RefCell::new(HashMap::new());
    /// Whether a review step is being shown (see [`step`]).
    static STEPPING: Cell<bool> = const { Cell::new(false) };
    /// Whether the current entry was added by a review step.
    static STEP_ENTRY: Cell<bool> = const { Cell::new(false) };
    /// Whether the view of an entry is being shown (see [`show`]).
    static SHOWING: Cell<bool> = const { Cell::new(false) };
    /// The cards of each canvas entry as last seen, by fragment.
    static CANVASES: RefCell<HashMap<String, crate::canvas::Saved>> = RefCell::new(HashMap::new());
}

/// After each event on the canvas: keeps its cards for the current entry,
/// so that coming back to the entry with Back or Forward shows them as
/// they were, also after another graph replaced them (see [`show`]).
pub fn remember_canvas(app: &App) {
    if app.s.view != View::Canvas {
        return;
    }
    CURRENT.with(|c| {
        let c = c.borrow();
        if matches!(*c, Route::Canvas(_)) {
            let saved = crate::canvas::save(&app.s.canvas);
            CANVASES.with(|m| m.borrow_mut().insert(c.hash(), saved));
        }
    });
}

fn location_route() -> Route {
    let hash = dom::window().location().hash().unwrap_or_default();
    Route::parse(&hash, decode)
}

/// The route of what the app shows now.
fn route_of(app: &App) -> Route {
    let anchor = |view: View| {
        app.s
            .anchor
            .as_ref()
            .filter(|a| a.view == view && (view != View::Module || a.module == app.s.module))
            .map(|a| a.to.clone())
    };
    match app.s.view {
        View::Overview => Route::Overview(anchor(View::Overview)),
        View::Module => Route::Module(app.s.module.clone(), anchor(View::Module)),
        View::Canvas => Route::Canvas(app.s.canvas_focus.clone()),
        View::Calls => Route::Calls(app.s.calls.focus.clone()),
        View::Space => Route::Space,
    }
}

/// Adds a history entry for what the app shows now, unless it is the
/// current entry already.
pub fn record(app: &App) {
    let route = route_of(app);
    let changed = CURRENT.with(|c| {
        let mut c = c.borrow_mut();
        let changed = *c != route;
        *c = route.clone();
        changed
    });
    if !changed {
        return;
    }
    let stepping = STEPPING.get();
    // A stale position (from an earlier visit) must not be restored.
    SCROLL.with(|s| s.borrow_mut().remove(&route.hash()));
    // Fires `hashchange` later, which then finds the route current.
    // Showing an entry ended up elsewhere (a call graph on a phone opens
    // in the call view): that replaces the entry.
    if (stepping && STEP_ENTRY.get()) || SHOWING.get() {
        let _ = dom::window().location().replace(&route.hash());
    } else {
        let _ = dom::window().location().set_hash(&route.hash());
    }
    STEP_ENTRY.set(stepping);
}

/// Points the view buttons at what each view shows now, so that one opened
/// in a new tab shows the same: the module page of the module shown, the
/// call view's item, and the canvas's cards (laid out again).
pub fn update_view_links(app: &App) {
    for b in dom::query_all("#views [data-view]") {
        let route = match dom::data(&b, "view").and_then(|v| View::from_name(&v)) {
            Some(View::Overview) => Route::Overview(None),
            Some(View::Module) => Route::Module(app.s.module.clone(), None),
            Some(View::Canvas) => {
                let mut ids = crate::canvas::shown_items(app);
                Route::Canvas(match ids.len() {
                    0 => None,
                    1 => ids.pop().map(OnCanvas::Item),
                    _ => Some(OnCanvas::Items(ids)),
                })
            }
            Some(View::Calls) => Route::Calls(app.s.calls.focus.clone()),
            Some(View::Space) => Route::Space,
            None => continue,
        };
        let _ = b.set_attribute("href", &route.hash());
    }
}

/// Runs `f`, which shows a review step: the history entry that it adds
/// replaces one that the step before added.
pub fn step<R>(f: impl FnOnce() -> R) -> R {
    STEPPING.set(true);
    let r = f();
    STEPPING.set(false);
    r
}

/// A view scrolled: keeps the position for the current entry.
pub fn on_scroll(ev: &Event) {
    let Some(el) = dom::target_element(ev) else {
        return;
    };
    CURRENT.with(|c| {
        let c = c.borrow();
        if c.scroller() == Some(el.id().as_str()) {
            SCROLL.with(|s| s.borrow_mut().insert(c.hash(), el.scroll_top()));
        }
    });
}

/// Shows the view that the URL names, when the page loads.
pub fn start(app: &mut App) {
    show(app, location_route(), true);
}

/// After Back or Forward (or an edited URL): shows the entry's view.
pub fn on_hash_change(app: &mut App) {
    let wanted = location_route();
    if CURRENT.with(|c| *c.borrow() != wanted) {
        show(app, wanted, false);
    }
}

fn show(app: &mut App, wanted: Route, first: bool) {
    // A route to an item that isn't there (a link from another analysis):
    // the nearest view that exists, which then replaces the entry.
    let route = match wanted.clone() {
        Route::Module(m, a) if app.d.item(&m).is_some_and(|it| it.info.kind == Kind::Mod) => {
            Route::Module(m, a)
        }
        Route::Module(..) => Route::Overview(None),
        // The overview listed public items in full before it listed them
        // as links: links to them (in PR comments) open the module page.
        Route::Overview(Some(Anchor::Item(id))) => match app.d.item(&id) {
            Some(it) => Route::Module(app.d.mod_of(it).to_string(), Some(Anchor::Item(id))),
            None => Route::Overview(None),
        },
        Route::Canvas(Some(OnCanvas::Item(id) | OnCanvas::Impact(id)))
            if app.d.item(&id).is_none() =>
        {
            Route::Canvas(None)
        }
        Route::Canvas(Some(OnCanvas::ModuleGraph(m)))
            if app.d.item(&m).is_none_or(|it| it.info.kind != Kind::Mod) =>
        {
            Route::Canvas(None)
        }
        Route::Canvas(Some(OnCanvas::Items(mut ids))) => {
            ids.retain(|id| app.d.item(id).is_some());
            Route::Canvas((!ids.is_empty()).then_some(OnCanvas::Items(ids)))
        }
        Route::Calls(Some(id)) if app.d.item(&id).is_none() => Route::Calls(None),
        r => r,
    };
    CURRENT.with(|c| *c.borrow_mut() = route.clone());
    STEP_ENTRY.set(false);
    if route != wanted {
        let _ = dom::window().location().replace(&route.hash());
    }
    crate::close_modal();
    SHOWING.set(true);
    match route.clone() {
        Route::Overview(a) => show_page(app, View::Overview, None, a, first),
        Route::Module(m, a) => show_page(app, View::Module, Some(m), a, first),
        // Back or Forward to a canvas entry seen before: its cards as the
        // reader left them, not laid out again.
        Route::Canvas(shows)
            if !first
                && let Some(saved) = CANVASES.with(|m| m.borrow().get(&route.hash()).cloned()) =>
        {
            crate::canvas::restore(app, &saved);
            app.s.canvas_focus = shows;
            crate::set_view(app, View::Canvas);
        }
        Route::Canvas(Some(OnCanvas::Item(id))) if crate::narrow() => crate::calls::focus(app, &id),
        Route::Canvas(Some(OnCanvas::Item(id))) => crate::canvas::open(app, &id, None, true),
        // Back to the call graph that the canvas shows: the cards stay as
        // the reader left them.
        Route::Canvas(Some(shows)) if !first && app.s.canvas_graph.as_ref() == Some(&shows) => {
            app.s.canvas_focus = Some(shows);
            crate::set_view(app, View::Canvas);
        }
        Route::Canvas(Some(shows)) => crate::show_on_canvas(app, &shows),
        Route::Canvas(None) => crate::set_view(app, View::Canvas),
        Route::Calls(Some(id)) => crate::calls::focus(app, &id),
        Route::Calls(None) => {
            app.s.calls = Default::default();
            crate::set_view(app, View::Calls);
        }
        Route::Space => crate::set_view(app, View::Space),
    }
    SHOWING.set(false);
    restore_scroll(&route);
}

/// Shows a page at an anchor. Moving between entries of the page that is
/// shown already (Back after a jump) only scrolls it.
fn show_page(app: &mut App, view: View, module: Option<String>, a: Option<Anchor>, first: bool) {
    let same = !first && app.s.view == view && module.as_ref().is_none_or(|m| *m == app.s.module);
    if let Some(m) = module {
        app.s.module = m;
    }
    if same {
        app.s.anchor = a.map(|to| PageAnchor {
            view,
            module: app.s.module.clone(),
            to,
        });
    } else {
        crate::set_view_at(app, view, a);
    }
}

/// Scrolls the entry's view to where it was when the entry was left, or
/// for an entry not seen in this visit, to its anchor or the top.
fn restore_scroll(route: &Route) {
    let Some(sec) = route.scroller() else {
        return;
    };
    let saved = SCROLL.with(|s| s.borrow().get(&route.hash()).copied());
    let (view, anchor) = match route {
        Route::Overview(a) => (View::Overview, a.clone()),
        Route::Module(_, a) => (View::Module, a.clone()),
        // The call view scrolls to its focused card itself.
        _ => (View::Calls, None),
    };
    // After the call view's own scrolling, a frame later.
    dom::raf(move || {
        let Some(sec) = dom::html_by_id(sec) else {
            return;
        };
        let el = anchor.as_ref().and_then(|a| crate::anchor_el(view, a));
        match (saved, el) {
            (Some(top), el) => {
                // The anchor's group was open, so the position counts it.
                if let Some(el) = el {
                    crate::open_groups_around(&el);
                }
                sec.set_scroll_top(top);
            }
            (None, Some(el)) => crate::reveal(&el),
            (None, None) if view != View::Calls => sec.set_scroll_top(0),
            (None, None) => {}
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(hash: &str) -> Route {
        // Tests run natively, without `decodeURIComponent`.
        Route::parse(hash, |s| Some(s.replace("%3A", ":")))
    }

    #[test]
    fn parses_fragments() {
        assert_eq!(parse(""), Route::Overview(None));
        assert_eq!(parse("#"), Route::Overview(None));
        assert_eq!(parse("#/"), Route::Overview(None));
        assert_eq!(parse("#/nonsense"), Route::Overview(None));
        assert_eq!(parse("#/module"), Route::Overview(None));
        assert_eq!(parse("#/overview"), Route::Overview(None));
        assert_eq!(
            parse("#/overview/item/demo%3A%3Aeval"),
            Route::Overview(Some(Anchor::Item("demo::eval".into())))
        );
        assert_eq!(
            parse("#/overview/file/README.md"),
            Route::Overview(Some(Anchor::File("README.md".into())))
        );
        assert_eq!(
            parse("#/module/demo%3A%3Aast"),
            Route::Module("demo::ast".into(), None)
        );
        assert_eq!(
            parse("#/module/demo%3A%3Aast/item/demo%3A%3Aast%3A%3AExpr"),
            Route::Module(
                "demo::ast".into(),
                Some(Anchor::Item("demo::ast::Expr".into()))
            )
        );
        assert_eq!(
            parse("#/module/demo%3A%3Aast/nonsense"),
            Route::Module("demo::ast".into(), None)
        );
        assert_eq!(parse("#/canvas"), Route::Canvas(None));
        assert_eq!(parse("#/canvas/"), Route::Canvas(None));
        assert_eq!(
            parse("#/canvas/demo%3A%3Aeval"),
            Route::Canvas(Some(OnCanvas::Item("demo::eval".into())))
        );
        assert_eq!(
            parse("#/graph/demo%3A%3Aeval,demo%3A%3Aast%3A%3AExpr"),
            Route::Canvas(Some(OnCanvas::Items(vec![
                "demo::eval".into(),
                "demo::ast::Expr".into()
            ])))
        );
        assert_eq!(parse("#/graph/"), Route::Canvas(None));
        assert_eq!(
            parse("#/module-graph/demo%3A%3Aast"),
            Route::Canvas(Some(OnCanvas::ModuleGraph("demo::ast".into())))
        );
        assert_eq!(
            parse("#/impact/demo%3A%3Aeval"),
            Route::Canvas(Some(OnCanvas::Impact("demo::eval".into())))
        );
        assert_eq!(parse("#/calls"), Route::Calls(None));
        assert_eq!(
            parse("#/calls/demo%3A%3Aeval"),
            Route::Calls(Some("demo::eval".into()))
        );
        assert_eq!(parse("#/structure"), Route::Space);
    }

    #[test]
    fn fragments_round_trip() {
        let decode = |s: &str| {
            // `decodeURIComponent` of what `enc` writes.
            let mut out = Vec::new();
            let b = s.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' {
                    out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).ok()?);
                    i += 3;
                } else {
                    out.push(b[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).ok()
        };
        let routes = [
            Route::Overview(None),
            Route::Overview(Some(Anchor::File("src/a b.rs".into()))),
            Route::Module(
                "demo::ast".into(),
                Some(Anchor::Item("demo::ast::Expr".into())),
            ),
            Route::Canvas(None),
            Route::Canvas(Some(OnCanvas::Item("demo::<impl Foo, Bar>::f".into()))),
            Route::Canvas(Some(OnCanvas::Items(vec![
                "demo::a,b".into(),
                "demo::c/d".into(),
            ]))),
            Route::Canvas(Some(OnCanvas::ModuleGraph("demo::ast".into()))),
            Route::Canvas(Some(OnCanvas::Impact("demo::eval".into()))),
            Route::Calls(Some("demo::eval".into())),
            Route::Space,
        ];
        for r in routes {
            assert_eq!(Route::parse(&r.hash(), decode), r, "{}", r.hash());
        }
        // Safe in a double-quoted attribute.
        let h = href(&Route::Canvas(Some(OnCanvas::Item(r#"a"<&>'"#.into()))));
        assert_eq!(h, r##" href="#/canvas/a%22%3C%26%3E'""##);
    }
}
