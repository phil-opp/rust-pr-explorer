//! Browser history for the views. Each view change, and each item opened
//! on the canvas from outside the canvas, gets its own entry in the URL's
//! fragment (`#/module/<id>`, `#/canvas/<id>`, …), so that the browser's
//! Back and Forward buttons move between them, and a link to a fragment
//! opens that view.
//!
//! The fragment is set through `location.hash`, not `history.pushState`:
//! a fragment change is allowed wherever the page is shown, also as a local
//! file and when it is served from an opaque origin (a sandboxed workflow
//! artifact).

use crate::{App, View, dom};
use model::Kind;
use std::cell::RefCell;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Route {
    Overview,
    Module(String),
    /// The canvas, focused on an item if it was opened from elsewhere.
    Canvas(Option<String>),
    /// The call view, focused on an item.
    Calls(Option<String>),
    Space,
}

impl Route {
    pub fn hash(&self) -> String {
        let enc = |id: &str| String::from(js_sys::encode_uri_component(id));
        match self {
            Route::Overview => "#/".to_string(),
            Route::Module(m) => format!("#/module/{}", enc(m)),
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
        let (head, arg) = match path.split_once('/') {
            Some((h, a)) => (h, Some(a).filter(|a| !a.is_empty()).and_then(&decode)),
            None => (path, None),
        };
        match (head, arg) {
            ("module", Some(m)) => Route::Module(m),
            ("canvas", id) => Route::Canvas(id),
            ("calls", id) => Route::Calls(id),
            ("structure", _) => Route::Space,
            _ => Route::Overview,
        }
    }
}

fn decode(s: &str) -> Option<String> {
    js_sys::decode_uri_component(s).ok().map(String::from)
}

thread_local! {
    /// The route of the current history entry.
    static CURRENT: RefCell<Route> = const { RefCell::new(Route::Overview) };
}

fn location_route() -> Route {
    let hash = dom::window().location().hash().unwrap_or_default();
    Route::parse(&hash, decode)
}

/// The route of what the app shows now.
fn route_of(app: &App) -> Route {
    match app.s.view {
        View::Overview => Route::Overview,
        View::Module => Route::Module(app.s.module.clone()),
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
    if changed {
        // Fires `hashchange` later, which then finds the route current.
        let _ = dom::window().location().set_hash(&route.hash());
    }
}

/// Shows the view that the URL names, when the page loads.
pub fn start(app: &mut App) {
    show(app, location_route());
}

/// After Back or Forward (or an edited URL): shows the entry's view.
pub fn on_hash_change(app: &mut App) {
    let wanted = location_route();
    if CURRENT.with(|c| *c.borrow() != wanted) {
        show(app, wanted);
    }
}

fn show(app: &mut App, wanted: Route) {
    // A route to an item that isn't there (a link from another analysis):
    // the nearest view that exists, which then replaces the entry.
    let route = match wanted.clone() {
        Route::Module(m) if app.d.item(&m).is_some_and(|it| it.info.kind == Kind::Mod) => {
            Route::Module(m)
        }
        Route::Module(_) => Route::Overview,
        Route::Canvas(Some(id)) if app.d.item(&id).is_none() => Route::Canvas(None),
        Route::Calls(Some(id)) if app.d.item(&id).is_none() => Route::Calls(None),
        r => r,
    };
    CURRENT.with(|c| *c.borrow_mut() = route.clone());
    if route != wanted {
        let _ = dom::window().location().replace(&route.hash());
    }
    crate::close_modal();
    match route {
        Route::Overview => crate::set_view(app, View::Overview),
        Route::Module(m) => {
            app.s.module = m;
            crate::set_view(app, View::Module);
        }
        Route::Canvas(Some(id)) => crate::canvas::open(app, &id, None, true),
        Route::Canvas(None) => crate::set_view(app, View::Canvas),
        Route::Calls(Some(id)) => crate::calls::focus(app, &id),
        Route::Calls(None) => {
            app.s.calls = Default::default();
            crate::set_view(app, View::Calls);
        }
        Route::Space => crate::set_view(app, View::Space),
    }
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
        assert_eq!(parse(""), Route::Overview);
        assert_eq!(parse("#"), Route::Overview);
        assert_eq!(parse("#/"), Route::Overview);
        assert_eq!(parse("#/nonsense"), Route::Overview);
        assert_eq!(parse("#/module"), Route::Overview);
        assert_eq!(
            parse("#/module/demo%3A%3Aast"),
            Route::Module("demo::ast".into())
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
