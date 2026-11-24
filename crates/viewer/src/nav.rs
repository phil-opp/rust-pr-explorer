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

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Route {
    Overview(Option<Anchor>),
    Module(String, Option<Anchor>),
    /// The canvas, focused on an item if it was opened from elsewhere.
    Canvas(Option<String>),
    /// The call view, focused on an item.
    Calls(Option<String>),
    Space,
}

impl Route {
    pub fn hash(&self) -> String {
        let enc = |id: &str| String::from(js_sys::encode_uri_component(id));
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
            Route::Canvas(Some(id)) => format!("#/canvas/{}", enc(id)),
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
            "canvas" => Route::Canvas(arg(1)),
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
    if stepping && STEP_ENTRY.get() {
        let _ = dom::window().location().replace(&route.hash());
    } else {
        let _ = dom::window().location().set_hash(&route.hash());
    }
    STEP_ENTRY.set(stepping);
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
        Route::Canvas(Some(id)) if app.d.item(&id).is_none() => Route::Canvas(None),
        Route::Calls(Some(id)) if app.d.item(&id).is_none() => Route::Calls(None),
        r => r,
    };
    CURRENT.with(|c| *c.borrow_mut() = route.clone());
    STEP_ENTRY.set(false);
    if route != wanted {
        let _ = dom::window().location().replace(&route.hash());
    }
    crate::close_modal();
    match route.clone() {
        Route::Overview(a) => show_page(app, View::Overview, None, a, first),
        Route::Module(m, a) => show_page(app, View::Module, Some(m), a, first),
        Route::Canvas(Some(id)) => crate::canvas::open(app, &id, None, true),
        Route::Canvas(None) => crate::set_view(app, View::Canvas),
        Route::Calls(Some(id)) => crate::calls::focus(app, &id),
        Route::Calls(None) => {
            app.s.calls = Default::default();
            crate::set_view(app, View::Calls);
        }
        Route::Space => crate::set_view(app, View::Space),
    }
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
            Route::Canvas(Some("demo::eval".into()))
        );
        assert_eq!(parse("#/calls"), Route::Calls(None));
        assert_eq!(
            parse("#/calls/demo%3A%3Aeval"),
            Route::Calls(Some("demo::eval".into()))
        );
        assert_eq!(parse("#/structure"), Route::Space);
    }
}
