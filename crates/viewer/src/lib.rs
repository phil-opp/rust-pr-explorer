//! Front end of the Rust Semantic Review page, compiled to WebAssembly.
//!
//! The analysis is embedded in the page as JSON. Views are rendered as
//! HTML strings; one listener per event type on the document dispatches
//! on `data-*` attributes of the element that was clicked or dragged.

mod bump;
mod calls;
mod canvas;
mod carry;
mod code;
mod commit_filter;
mod css;
mod data;
mod dom;
mod drafts;
mod files;
mod graphs;
mod icons;
mod impact;
mod indirect;
mod js;
mod keys;
mod line_notes;
mod listing;
mod map;
mod nav;
mod outline;
mod pages;
mod post;
mod review;
mod route;
mod settings;
mod sha256;
mod since;
mod space;
mod threads;
mod unsafety;
mod yaml;

use code::esc;
use data::{Data, changed};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::wasm_bindgen;
use web_sys::{Element, Event, KeyboardEvent, PointerEvent, WheelEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Overview,
    Module,
    Canvas,
    Calls,
    Space,
}

impl View {
    fn name(self) -> &'static str {
        match self {
            View::Overview => "overview",
            View::Module => "module",
            View::Canvas => "canvas",
            View::Calls => "calls",
            View::Space => "space",
        }
    }

    fn from_name(name: &str) -> Option<View> {
        [
            View::Overview,
            View::Module,
            View::Canvas,
            View::Calls,
            View::Space,
        ]
        .into_iter()
        .find(|v| v.name() == name)
    }
}

pub struct State {
    pub view: View,
    /// Module shown on the module page.
    pub module: String,
    pub show_unchanged: bool,
    /// Whether the sidebar lists unchanged modules too.
    pub side_all: bool,
    pub canvas: canvas::Canvas,
    /// What the canvas was last asked to show from outside it, for the
    /// history entry (see [`nav`]).
    pub canvas_focus: Option<nav::OnCanvas>,
    /// The call graph last laid out on the canvas, if any: Back to its
    /// history entry shows the canvas as it is, instead of laying it out
    /// again.
    pub canvas_graph: Option<nav::OnCanvas>,
    /// Where on the overview or module page a link last jumped to, for the
    /// history entry (see [`nav`]).
    pub anchor: Option<nav::PageAnchor>,
    /// The call view.
    pub calls: calls::Calls,
    pub space: space::Space,
    search: Vec<String>,
    search_sel: usize,
    /// The unit last shown by "next unreviewed", where the next search
    /// continues.
    rv_current: Option<String>,
    /// The hunk of `rv_current` that `n` and `p` last moved to, which the
    /// verdict keys then act on.
    rv_hunk: Option<String>,
}

pub struct App {
    pub d: Rc<Data>,
    pub s: State,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app state. Must not be nested: only event handlers
/// and deferred callbacks call this.
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|app| app.try_borrow_mut().ok()?.as_mut().map(f))
}

#[wasm_bindgen(start)]
pub fn start() {
    // A bug must not leave a page that silently stops responding.
    std::panic::set_hook(Box::new(|info| {
        let body = dom::document().body();
        if let Some(body) = body {
            let msg = format!(
                r#"<div id="panic">The viewer hit an internal error and stopped: {}. Reload the page to continue.</div>"#,
                esc(&info.to_string())
            );
            let _ = body.insert_adjacent_html("afterbegin", &msg);
        }
    }));
    let Some(json) = dom::by_id("data").and_then(|el| el.text_content()) else {
        return;
    };
    let out: model::Output = match serde_json::from_str(&json) {
        Ok(out) => out,
        Err(e) => {
            dom::set_html(
                "main",
                &format!(
                    "<p>Cannot read the embedded analysis: {}</p>",
                    esc(&e.to_string())
                ),
            );
            return;
        }
    };
    let d = Rc::new(Data::new(out));
    let root = d.main_root().to_string();
    let app = App {
        s: State {
            view: View::Overview,
            module: root.clone(),
            show_unchanged: false,
            side_all: false,
            canvas: canvas::Canvas::new(None),
            canvas_focus: None,
            canvas_graph: None,
            anchor: None,
            calls: calls::Calls::default(),
            space: space::Space::new(),
            search: Vec::new(),
            search_sel: 0,
            rv_current: None,
            rv_hunk: None,
        },
        d,
    };
    settings::load();
    review::load(&app.d);
    let title = app.d.header_html();
    dom::set_html("pr-title", &title);
    dom::set_html("commit-menu", &commit_filter::menu_html(&app.d));
    APP.with(|a| *a.borrow_mut() = Some(app));
    install_listeners();
    with_app(nav::start);
}

/// Shows a view, and adds a history entry for it if it differs from the
/// current one. Showing the page that is shown again keeps its anchor.
pub fn set_view(app: &mut App, view: View) {
    let same_page = |a: &nav::PageAnchor| {
        a.view == view && app.s.view == view && (view != View::Module || a.module == app.s.module)
    };
    let anchor = app.s.anchor.take().filter(same_page).map(|a| a.to);
    set_view_at(app, view, anchor);
}

/// Shows a view, the overview or a module page with what a link jumped
/// to on it, if any, and adds a history entry for that.
pub fn set_view_at(app: &mut App, view: View, anchor: Option<nav::Anchor>) {
    app.s.anchor = anchor.map(|to| nav::PageAnchor {
        view,
        module: app.s.module.clone(),
        to,
    });
    if view != View::Canvas {
        app.s.canvas_focus = None;
    }
    app.s.view = view;
    for b in dom::query_all("#views [data-view]") {
        dom::toggle_class(
            &b,
            "on",
            dom::data(&b, "view").as_deref() == Some(view.name()),
        );
    }
    for section in dom::query_all(".view") {
        dom::toggle_class(
            &section,
            "active",
            section.id() == format!("view-{}", view.name()),
        );
    }
    // Show or hide the sidebar first, so that the view lays out at its final width.
    for id in ["sidebar", "side-btn", "side-show"] {
        if let Some(el) = dom::by_id(id) {
            dom::toggle_class(&el, "hidden", !pages::sidebar_shown(app));
        }
    }
    outline::apply_closed(app);
    match view {
        View::Overview => pages::render_overview(app),
        View::Module => pages::render_module_page(app),
        View::Canvas => canvas::render(app),
        View::Calls => calls::render(app),
        View::Space => space::show(app),
    }
    pages::render_sidebar(app);
    pages::sync_header_title();
    keys::mark_current(app);
    nav::record(app);
    nav::update_view_links(app);
}

thread_local! {
    /// Counts toasts, so that only the latest one's timer hides it.
    static TOAST: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub fn toast(msg: &str) {
    let Some(el) = dom::by_id("toast") else {
        return;
    };
    el.set_text_content(Some(msg));
    dom::toggle_class(&el, "show", true);
    let generation = TOAST.with(|t| {
        t.set(t.get() + 1);
        t.get()
    });
    dom::timeout(2600, move || {
        if TOAST.with(|t| t.get()) == generation
            && let Some(el) = dom::by_id("toast")
        {
            dom::toggle_class(&el, "show", false);
        }
    });
}

pub fn open_modal(title: &str, body: &str) {
    dom::set_html("modal-title", title);
    dom::set_html("modal-b", body);
    if let Some(m) = dom::by_id("modal") {
        dom::toggle_class(&m, "hidden", false);
    }
}

pub fn close_modal() {
    if let Some(m) = dom::by_id("modal") {
        dom::toggle_class(&m, "hidden", true);
        // Keys go to the page again, not to the hidden comment field.
        if let Some(f) = dom::document().active_element()
            && m.contains(Some(&f))
            && let Some(f) = f.dyn_ref::<web_sys::HtmlElement>()
        {
            let _ = f.blur();
        }
    }
}

pub fn show_tooltip(html: &str) {
    if let Some(tip) = dom::by_id("tooltip") {
        tip.set_inner_html(html);
        dom::set_style(&tip, "display", "block");
    }
}

pub fn hide_tooltip() {
    if let Some(tip) = dom::by_id("tooltip") {
        dom::set_style(&tip, "display", "none");
    }
}

thread_local! {
    /// Whether the last pointer was a finger. A tap fires `mouseover`
    /// too, but no `mouseout` follows, so a hover preview it opened would
    /// stay on top of the page.
    static TOUCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn note_pointer(ev: &PointerEvent) {
    let touch = ev.pointer_type() == "touch";
    if touch && !TOUCH.replace(true) {
        hide_tooltip();
    } else if ev.pointer_type() == "mouse" {
        TOUCH.set(false);
    }
}

thread_local! {
    /// Whether reading the marks that another tab saved is due.
    static MARKS_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn install_listeners() {
    let doc: web_sys::EventTarget = dom::document().into();
    if let Some(overview) = dom::by_id("view-overview") {
        dom::listen_with(&overview, "scroll", false, true, |_| {
            pages::sync_header_title()
        });
    }
    // Not passive: clicks on cross links prevent the default navigation.
    dom::listen_with(&doc, "click", false, false, |ev| {
        if let Some(el) = dom::target_element(&ev) {
            // The app's links lead to a fragment (see `nav::href`). With a
            // modifier key the browser opens it in a new tab or window (a
            // middle click fires no `click`); else the app handles it.
            if dom::closest(&el, r##"a[href^="#/"], a[data-permalink]"##).is_some() {
                if ev
                    .dyn_ref::<web_sys::MouseEvent>()
                    .is_some_and(|m| m.ctrl_key() || m.meta_key() || m.shift_key() || m.alt_key())
                {
                    return;
                }
                ev.prevent_default();
            }
            with_app(|app| {
                // A touch that moved the view isn't a tap.
                if dom::closest(&el, "#viewport").is_some() && canvas::swallow_click(app) {
                    ev.prevent_default();
                    return;
                }
                canvas::before_change(app);
                on_click(app, &el, &ev);
                canvas::after_change(app);
                nav::sync_canvas(app);
                nav::update_view_links(app);
            });
            mark_current_later();
        }
    });
    // Cards are as wide as their code, measured in the code's font, which
    // may load after they were rendered.
    if let Some(fonts) = js_sys::Reflect::get(&dom::document(), &"fonts".into())
        .ok()
        .and_then(|f| f.dyn_into::<web_sys::EventTarget>().ok())
    {
        dom::listen(&fonts, "loadingdone", |_| {
            with_app(|app| canvas::refit(&app.s.canvas));
        });
    }
    dom::listen(&doc, "dblclick", |ev| {
        if let Some(el) = dom::target_element(&ev) {
            with_app(|app| {
                canvas::before_change(app);
                canvas::on_dblclick(app, &el);
                canvas::after_change(app);
            });
        }
    });
    // Not passive: starting a card drag prevents text selection.
    dom::listen_with(&doc, "pointerdown", false, false, |ev| {
        let (Some(el), Ok(pev)) = (dom::target_element(&ev), ev.dyn_into::<PointerEvent>()) else {
            return;
        };
        note_pointer(&pev);
        with_app(|app| {
            // Before the browser opens a view button's link in a new tab
            // (or its menu): what the view shows now.
            if dom::closest(&el, "#views").is_some() {
                nav::update_view_links(app);
            }
            // Search results are picked on press, before the input's blur hides them.
            if let Some(r) = dom::closest(&el, "#search-results [data-i]") {
                let i = dom::data(&r, "i").and_then(|i| i.parse().ok()).unwrap_or(0);
                pick_search(app, i);
                return;
            }
            if canvas::on_pointer_down(app, &el, &pev) {
                canvas::before_change(app);
            } else {
                let _ = space::on_pointer_down(app, &el, &pev);
            }
        });
    });
    dom::listen(&doc, "pointermove", |ev| {
        let Ok(pev) = ev.dyn_into::<PointerEvent>() else {
            return;
        };
        note_pointer(&pev);
        with_app(|app| {
            canvas::on_pointer_move(app, &pev);
            space::on_pointer_move(app, &pev);
        });
    });
    // A touch the browser takes over (to scroll or zoom the page) ends
    // like a lifted finger.
    for name in ["pointerup", "pointercancel"] {
        dom::listen(&doc, name, |ev| {
            let Ok(pev) = ev.dyn_into::<PointerEvent>() else {
                return;
            };
            with_app(|app| {
                canvas::on_pointer_up(app, &pev);
                canvas::after_change(app);
                nav::sync_canvas(app);
                space::on_pointer_up(app);
            });
        });
    }
    // Not passive: zooming prevents the page from scrolling.
    dom::listen_with(&doc, "wheel", false, false, |ev| {
        let (Some(el), Ok(wev)) = (dom::target_element(&ev), ev.dyn_into::<WheelEvent>()) else {
            return;
        };
        with_app(|app| {
            if dom::closest(&el, "#viewport").is_some() {
                canvas::on_wheel(app, &el, &wev);
            }
        });
    });
    dom::listen(&doc, "mouseover", |ev| {
        let Some(el) = dom::target_element(&ev) else {
            return;
        };
        with_app(|app| on_mouse_over(app, &el));
    });
    dom::listen(&doc, "mousemove", |ev| {
        let Ok(mev) = ev.dyn_into::<web_sys::MouseEvent>() else {
            return;
        };
        let Some(tip) = dom::html_by_id("tooltip") else {
            return;
        };
        if tip.style().get_property_value("display").ok().as_deref() != Some("block") {
            return;
        }
        let width = tip.offset_width() as f64;
        let inner = dom::window()
            .inner_width()
            .ok()
            .and_then(|w| w.as_f64())
            .unwrap_or(1000.0);
        let left = (mev.client_x() as f64 + 14.0).min(inner - width - 8.0);
        dom::set_style(&tip, "left", &format!("{left}px"));
        dom::set_style(&tip, "top", &format!("{}px", mev.client_y() + 16));
    });
    dom::listen(&doc, "change", |ev| {
        let Some(el) = dom::target_element(&ev) else {
            return;
        };
        with_app(|app| {
            if settings::on_change(&el) {
                set_view(app, app.s.view);
            } else if el.id() == "cv-scope" {
                let value = el
                    .dyn_ref::<web_sys::HtmlSelectElement>()
                    .map(|s| s.value())
                    .unwrap_or_default();
                canvas::before_change(app);
                canvas::set_privacy_view(app, Some(value).filter(|v| !v.is_empty()));
                canvas::after_change(app);
            } else if el.id() == "commit-filter" {
                let value = el
                    .dyn_ref::<web_sys::HtmlSelectElement>()
                    .map(|s| s.value())
                    .unwrap_or_default();
                commit_filter::set(&app.d, value.parse().ok());
                set_view(app, app.s.view);
            } else if el.id() == "show-unchanged" {
                app.s.show_unchanged = el
                    .dyn_ref::<web_sys::HtmlInputElement>()
                    .is_some_and(|i| i.checked());
                pages::render_module_page(app);
            } else if el.id() == "side-all" {
                app.s.side_all = el
                    .dyn_ref::<web_sys::HtmlInputElement>()
                    .is_some_and(|i| i.checked());
                pages::render_sidebar(app);
            } else if el.id() == "rv-body" || dom::closest(&el, ".rv-ev").is_some() {
                update_finish(app);
            } else {
                let _ = space::on_change(app, &el);
            }
        });
    });
    dom::listen(&doc, "input", |ev| {
        let Some(el) = dom::target_element(&ev) else {
            return;
        };
        // A dialog can close before its text is saved (see `drafts`).
        drafts::on_input(&el);
        if el.id() == "rv-body" {
            with_app(|app| update_finish(app));
        }
    });
    // Not passive: Ctrl+Enter in the comment editor prevents the newline.
    dom::listen_with(&doc, "keydown", false, false, |ev| {
        // Review keys (`j`, `a`, `n`, `?`, …), unless typing, and Ctrl+Z
        // on the canvas.
        if let Some(k) = ev.dyn_ref::<KeyboardEvent>()
            && with_app(|app| {
                canvas::before_change(app);
                let done = keys::on_key(app, k);
                canvas::after_change(app);
                nav::sync_canvas(app);
                done
            }) == Some(true)
        {
            ev.prevent_default();
            mark_current_later();
            return;
        }
        // Ctrl+Enter in the comment editor saves with its verdict.
        if let Some(k) = ev.dyn_ref::<KeyboardEvent>()
            && k.key() == "Enter"
            && (k.ctrl_key() || k.meta_key())
            && let Some(v) = dom::query(".rv-editor")
                .and_then(|e| dom::data(&e, "rv-verdict"))
                .and_then(|v| review::Verdict::from_name(&v))
        {
            ev.prevent_default();
            with_app(|app| save_from_editor(app, v));
            mark_current_later();
            return;
        }
        if ev
            .dyn_ref::<KeyboardEvent>()
            .is_some_and(|k| k.key() == "Escape")
        {
            // Unselects chosen lines, unless it closes a dialog.
            let modal = dom::by_id("modal").is_some_and(|m| !m.class_list().contains("hidden"));
            if !modal {
                line_notes::cancel();
            }
            close_modal();
            if let Some(p) = dom::by_id("settings") {
                dom::toggle_class(&p, "hidden", true);
            }
        }
    });
    // `toggle` does not bubble; listen in the capture phase.
    dom::listen_with(&doc, "toggle", true, true, |ev| {
        let Some(el) = dom::target_element(&ev) else {
            return;
        };
        with_app(|app| canvas::on_details_toggle(app, &el));
    });
    if let Some(input) = dom::by_id("search") {
        let target: web_sys::EventTarget = input.into();
        dom::listen(&target, "input", |_| {
            with_app(|app| {
                app.s.search_sel = 0;
                render_search(app);
            });
        });
        dom::listen(&target, "keydown", |ev| {
            let Ok(kev) = ev.dyn_into::<KeyboardEvent>() else {
                return;
            };
            with_app(|app| on_search_key(app, &kev));
        });
        dom::listen(&target, "blur", |_| {
            dom::timeout(150, || {
                if let Some(b) = dom::by_id("search-results") {
                    dom::toggle_class(&b, "open", false);
                }
            });
        });
    }
    // Back and Forward.
    dom::listen(&dom::window().into(), "hashchange", |_| {
        // Showing an entry's cards again can be undone, like a click.
        with_app(|app| {
            canvas::before_change(app);
            nav::on_hash_change(app);
            canvas::after_change(app);
        });
    });
    // Marks saved in another tab: shown here too. Saving writes several
    // keys; they are read once.
    dom::listen(&dom::window().into(), "storage", |ev| {
        let key = ev.dyn_ref::<web_sys::StorageEvent>().and_then(|e| e.key());
        if review::is_marks_key(key.as_deref()) && !MARKS_SYNC.replace(true) {
            dom::timeout(0, || {
                MARKS_SYNC.set(false);
                with_app(|app| {
                    if review::sync_from_storage() {
                        pages::render_sidebar(app);
                    }
                });
            });
        }
    });
    // `scroll` does not bubble; listen in the capture phase.
    dom::listen_with(&doc, "scroll", true, true, |ev| nav::on_scroll(&ev));
    dom::listen(&dom::window().into(), "resize", |_| {
        with_app(|app| match app.s.view {
            View::Canvas => canvas::draw_edges(app),
            View::Space if app.s.space.mode == space::Mode::Map => map::draw_map_edges(app),
            _ => {}
        });
    });
}

fn on_click(app: &mut App, el: &Element, ev: &Event) {
    if let Some(panel) = dom::by_id("settings") {
        if dom::closest(el, "#settings-btn").is_some() {
            let open = panel.class_list().contains("hidden");
            if open {
                panel.set_inner_html(&settings::panel_html());
            }
            dom::toggle_class(&panel, "hidden", !open);
            return;
        }
        if dom::closest(el, "#settings").is_some() {
            return;
        }
        dom::toggle_class(&panel, "hidden", true);
    }
    // On a wide screen the sidebar can be collapsed, per view.
    if dom::closest(el, "#side-hide, #side-show").is_some() {
        outline::toggle_closed(app.s.view);
        outline::apply_closed(app);
        if app.s.view == View::Canvas {
            canvas::draw_edges(app);
        }
        return;
    }
    // On a narrow screen the sidebar is a drawer over the view, opened by
    // its button. Picking one of its rows or clicking outside closes it.
    if let Some(body) = dom::document().body() {
        let open = body.class_list().contains("side-open");
        if dom::closest(el, "#side-btn").is_some() {
            dom::toggle_class(&body, "side-open", !open);
            return;
        }
        if open && dom::closest(el, "#sidebar").is_none() {
            // Only closes: the click wasn't aimed at what the drawer covered.
            ev.prevent_default();
            dom::toggle_class(&body, "side-open", false);
            return;
        }
        if open && dom::closest(el, ".tree-row").is_some() {
            dom::toggle_class(&body, "side-open", false);
        }
    }
    // A permalink: copied, also to paste it where links can't be
    // right-clicked.
    if let Some(a) = dom::closest(el, "a[data-permalink]") {
        if let Some(url) = a.get_attribute("href") {
            copy_with(
                &url,
                "The link is copied.",
                "The browser didn't allow copying: right-click the link icon and copy the link.",
            );
        }
        return;
    }
    if line_notes::on_click(app, el, ev) {
        return;
    }
    if on_review_click(app, el) {
        return;
    }
    if dom::closest(el, "[data-modal-close]").is_some() || el.id() == "modal" {
        close_modal();
        return;
    }
    if let Some(b) = dom::closest(el, "[data-compare-docs]") {
        let id = dom::data(&b, "compare-docs").unwrap_or_default();
        if let Some(it) = app.d.item(&id) {
            let html = code::docs_compare_html(&app.d, it);
            open_modal(&format!("Docs of {}", esc(&app.d.display_name(it))), &html);
        }
        return;
    }
    if let Some(b) = dom::closest(el, "[data-unfold], [data-unfold-all]") {
        canvas::on_unfold(app, &b);
        return;
    }
    if let Some(b) = dom::closest(el, "[data-impact]") {
        let id = dom::data(&b, "impact").unwrap_or_default();
        show_on_canvas(app, &nav::OnCanvas::Impact(id));
        return;
    }
    if dom::closest(el, "#view-canvas").is_some() && canvas::on_click(app, el) {
        return;
    }
    if dom::closest(el, "#view-calls").is_some() && calls::on_click(app, el) {
        return;
    }
    if dom::closest(el, "#view-space").is_some() && space::on_click(app, el) {
        return;
    }
    if let Some(a) = dom::closest(el, "a.ref[data-id]") {
        ev.prevent_default();
        let id = dom::data(&a, "id").unwrap_or_default();
        let from =
            dom::closest(&a, ".card").and_then(|card| canvas::active_item_of_card(app, &card));
        match from {
            Some(from) => canvas::open(app, &id, Some(&from), false),
            // On a page: jump to the item if the page shows it, else open
            // it on the canvas.
            None => match row_on_page(&a, &id) {
                Some(row) => jump_to_row(app, id, &row),
                None => open_item(app, &id),
            },
        }
        return;
    }
    if dom::closest(el, "[data-commit-clear]").is_some() {
        commit_filter::set(&app.d, None);
        if let Some(s) = dom::by_id("commit-filter")
            .and_then(|e| e.dyn_into::<web_sys::HtmlSelectElement>().ok())
        {
            s.set_value("");
        }
        set_view(app, app.s.view);
        return;
    }
    if let Some(b) = dom::closest(el, "[data-open-canvas]") {
        let id = dom::data(&b, "open-canvas").unwrap_or_default();
        canvas::open(app, &id, None, true);
        return;
    }
    if let Some(b) = dom::closest(el, "[data-open]") {
        open_item(app, &dom::data(&b, "open").unwrap_or_default());
        return;
    }
    // The overview's call graphs: from a graph's summary to its items and
    // back.
    for (attr, target) in [("cgitems", "cgitems"), ("cgraph-summary", "cgraph")] {
        if let Some(b) = dom::closest(el, &format!("[data-{attr}]")) {
            let i = dom::data(&b, attr).unwrap_or_default();
            if let Some(t) = dom::by_id(&format!("{target}-{i}")) {
                reveal(&t);
            }
            return;
        }
    }
    if let Some(b) = dom::closest(el, "[data-calls-focus]") {
        let id = dom::data(&b, "calls-focus").unwrap_or_default();
        calls::focus(app, &id);
        return;
    }
    if let Some(b) = dom::closest(el, "[data-cgraph]") {
        let i = dom::data(&b, "cgraph")
            .and_then(|i| i.parse().ok())
            .unwrap_or(0);
        let ids = graphs::graph_items(&app.d, i);
        show_on_canvas(app, &nav::OnCanvas::Items(ids));
        return;
    }
    if let Some(b) = dom::closest(el, "[data-graph]") {
        let m = dom::data(&b, "graph").unwrap_or_default();
        show_on_canvas(app, &nav::OnCanvas::ModuleGraph(m));
        return;
    }
    if let Some(f) = dom::closest(el, "[data-goto-item]") {
        // An item further down the page if it lists the item (the
        // overview's list of public API changes), else on its module page.
        let id = dom::data(&f, "goto-item").unwrap_or_default();
        match row_on_page(&f, &id) {
            Some(row) => jump_to_row(app, id, &row),
            None => show_on_module_page(app, &id),
        }
        return;
    }
    if let Some(f) = dom::closest(el, "[data-goto-file]") {
        let path = dom::data(&f, "goto-file").unwrap_or_default();
        let anchor = nav::Anchor::File(path);
        set_view_at(app, View::Overview, Some(anchor.clone()));
        if let Some(card) = anchor_el(View::Overview, &anchor) {
            reveal(&card);
        }
        return;
    }
    if outline::on_click(app, el) {
        return;
    }
    if let Some(m) = dom::closest(el, "[data-mod]") {
        app.s.module = dom::data(&m, "mod").unwrap_or_default();
        set_view(app, View::Module);
        return;
    }
    if let Some(v) = dom::closest(el, "#views [data-view]")
        .and_then(|b| dom::data(&b, "view"))
        .and_then(|v| View::from_name(&v))
    {
        set_view(app, v);
        return;
    }
    if dom::closest(el, "#mod-graph").is_some() {
        let m = app.s.module.clone();
        show_on_canvas(app, &nav::OnCanvas::ModuleGraph(m));
    }
}

/// Whether the screen is phone-sized: as narrow as the layout for phones
/// in `explorer.css` (`max-width: 760px`).
pub fn narrow() -> bool {
    dom::window()
        .inner_width()
        .ok()
        .and_then(|w| w.as_f64())
        .is_some_and(|w| w <= 760.0)
}

/// Shows an item that a link or button outside the canvas points to: on
/// a phone, or while the call view is open, in the call view, which needs
/// no panning and zooming; else on the canvas.
pub fn open_item(app: &mut App, id: &str) {
    if narrow() || app.s.view == View::Calls {
        calls::focus(app, id);
    } else {
        canvas::open(app, id, None, true);
    }
}

/// Shows an item's row on its module page (a module: its page), or on the
/// canvas if that page doesn't list it (an unchanged item).
pub fn show_on_module_page(app: &mut App, id: &str) {
    let Some(it) = app.d.item(id) else {
        return;
    };
    app.s.module = app.d.mod_of(it).to_string();
    // A module: its own page.
    if it.info.kind == model::Kind::Mod {
        set_view(app, View::Module);
        return;
    }
    let anchor = nav::Anchor::Item(id.to_string());
    set_view_at(app, View::Module, Some(anchor.clone()));
    match anchor_el(View::Module, &anchor) {
        Some(row) => reveal(&row),
        None => open_item(app, id),
    }
}

/// Shows a call graph on the canvas. On a phone, in the call view, which
/// needs no panning and zooming: the graph's first item, a module's
/// changed items, or an item's callers, which lead up to the public API.
pub fn show_on_canvas(app: &mut App, shows: &nav::OnCanvas) {
    use nav::OnCanvas;
    match shows {
        OnCanvas::Item(id) => open_item(app, id),
        OnCanvas::Items(ids) if narrow() => {
            if let Some(root) = ids.first() {
                calls::focus(app, root);
            }
        }
        OnCanvas::Items(ids) => canvas::show_items(app, ids),
        OnCanvas::ModuleGraph(m) if narrow() => calls::focus(app, m),
        OnCanvas::ModuleGraph(m) => canvas::show_module_graph(app, m),
        OnCanvas::Impact(id) if narrow() => calls::show_callers(app, id),
        OnCanvas::Impact(id) => canvas::show_impact(app, id),
    }
}

/// Clicks on review controls. Returns whether the click was handled.
fn on_review_click(app: &mut App, el: &Element) -> bool {
    if let Some(c) = dom::closest(el, ".rvc") {
        let id = dom::data(&c, "rv").unwrap_or_default();
        if dom::closest(el, "[data-rv-go-next]").is_some() {
            go_next(app, Some(id), false);
        } else if let Some(b) = dom::closest(el, "[data-rv-go-graph]") {
            let i = dom::data(&b, "rv-go-graph").unwrap_or_default();
            if app.s.view != View::Overview {
                set_view(app, View::Overview);
            }
            if let Some(g) = dom::by_id(&format!("cgraph-{i}")) {
                reveal(&g);
            }
        } else if dom::closest(el, "[data-rv-since]").is_some() {
            show_since_review(app, &id);
        } else if let Some(btn) = dom::closest(el, "[data-rv-act]")
            && let Some(act) =
                dom::data(&btn, "rv-act").and_then(|a| review::Verdict::from_name(&a))
        {
            if act == review::Verdict::Approve {
                // Keeps a comment written earlier.
                let text = review::note(&id).text;
                let all = dom::data(&btn, "rv-all").is_some();
                save_verdict(app, &id, act, text, all);
            } else {
                open_editor(app, &id, act);
            }
        } else if dom::closest(el, "[data-rv-edit]").is_some() {
            let v = review::note(&id).verdict;
            open_editor(app, &id, v);
        } else if dom::closest(el, "[data-rv-fold]").is_some() {
            review::toggle_unfolded(&id);
        } else if dom::closest(el, "[data-rv-undo]").is_some() {
            let text = review::note(&id).text;
            let sure = text.trim().is_empty()
                || dom::window()
                    .confirm_with_message("Remove the mark and your comment?")
                    .unwrap_or(false);
            if sure {
                review::set_mark(&app.d, &id, None);
                app.s.rv_current = Some(id);
                pages::render_sidebar(app);
            }
        }
        return true;
    }
    if dom::closest(el, "[data-rv-draft-discard]").is_some() {
        drafts::discard_editor();
        return true;
    }
    if let Some(b) = dom::closest(el, "[data-rv-save]") {
        let verdict = dom::data(&b, "rv-save").and_then(|v| review::Verdict::from_name(&v));
        if let Some(v) = verdict {
            save_from_editor(app, v);
        }
        return true;
    }
    if let Some(b) = dom::closest(el, "[data-rv-out]") {
        finish_output(app, &dom::data(&b, "rv-out").unwrap_or_default());
        return true;
    }
    if dom::closest(el, "#rv-finish").is_some() {
        open_modal("Finish your review", &post::finish_html(&app.d));
        drafts::restore_summary();
        update_finish(app);
        return true;
    }
    if dom::closest(el, "#rv-mode, #rv-exit").is_some() {
        review::set_on(!review::is_on());
        pages::render_sidebar(app);
        return true;
    }
    if dom::closest(el, "#rv-keys").is_some() {
        open_modal("Keyboard shortcuts", &keys::cheat_sheet_html());
        return true;
    }
    if dom::closest(el, "#rv-next").is_some() {
        let from = app.s.rv_current.clone();
        go_next(app, from, false);
        return true;
    }
    if dom::closest(el, "#rv-clear").is_some() {
        let (done, _) = review::progress();
        let sure = done == 0
            || dom::window()
                .confirm_with_message(&format!(
                    "Remove all {done} review marks of this pull request?"
                ))
                .unwrap_or(false);
        if sure {
            review::clear();
            app.s.rv_current = None;
            close_modal();
            set_view(app, app.s.view);
        }
        return true;
    }
    if dom::closest(el, "#rv-marks").is_some() {
        open_modal("Your review marks", &carry::dialog_html());
        return true;
    }
    if dom::closest(el, "#rv-load").is_some() {
        match review::saved_marks().as_slice() {
            [one] => load_marks(app, carry::import_saved(&one.data)),
            _ => open_modal("Your review marks", &carry::dialog_html()),
        }
        return true;
    }
    if let Some(b) = dom::closest(el, "[data-rv-load]") {
        let i: usize = dom::data(&b, "rv-load")
            .and_then(|i| i.parse().ok())
            .unwrap_or(usize::MAX);
        if let Some(m) = review::saved_marks().get(i) {
            load_marks(app, carry::import_saved(&m.data));
        }
        return true;
    }
    if dom::closest(el, "#rv-paste-load").is_some() {
        load_marks(app, carry::import_text(&textarea_value("rv-paste")));
        return true;
    }
    if dom::closest(el, "#rv-copy-marks").is_some() {
        if let Some(text) = carry::copy_text() {
            if let Some(out) =
                dom::by_id("rv-out").and_then(|t| t.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
            {
                out.set_value(&text);
                dom::toggle_class(&out, "hidden", false);
                out.select();
            }
            copy(&text, "Your marks");
        }
        return true;
    }
    false
}

/// Loads marks (see `carry`) and shows them.
fn load_marks(app: &mut App, loaded: Result<String, String>) {
    match loaded {
        Ok(msg) => {
            close_modal();
            review::load(&app.d);
            set_view(app, app.s.view);
            pages::render_sidebar(app);
            toast(&msg);
        }
        Err(e) => toast(&e),
    }
}

/// Gives a unit a verdict and moves on to the next unreviewed one, or
/// stays on it when the review steps didn't go there or the canvas is
/// open (see `stay`).
/// For a hunk: moves on to the item's next hunk without a verdict, and
/// when there is none, gives the item the verdict its hunks add up to.
/// With `same`, an approval also approves the open items with the same
/// change (see `review::mark_same_change`).
fn save_verdict(app: &mut App, id: &str, verdict: review::Verdict, text: String, same: bool) {
    // Only a unit that the review steps (Next ▸, a key, the verdict
    // before) went to moves on to the next one. One that the reviewer
    // opened themselves, e.g. from a call graph, stays, with links to
    // where to go next; also for its next hunks. On the canvas every unit
    // stays: moving on there adds a card and moves the view, so it is
    // left to Next ▸, `j` and the links of the row under the verdict.
    let unit = review::hunk_item(id).unwrap_or_else(|| id.to_string());
    let guided = app.s.view != View::Canvas
        && app.s.rv_current.as_deref() == Some(unit.as_str())
        && review::stayed().as_deref() != Some(unit.as_str());
    if !guided {
        review::set_stayed(Some((unit.clone(), String::new())));
    }
    review::set_mark(&app.d, id, Some(review::Note { verdict, text }));
    if let Some(item) = review::hunk_item(id) {
        app.s.rv_current = Some(item.clone());
        app.s.rv_hunk = review::next_open_hunk(id);
        if let Some(next) = app.s.rv_hunk.clone() {
            // The canvas doesn't scroll like a page; the hunk is in its card.
            if app.s.view != View::Canvas {
                reveal_unit(&format!("view-{}", app.s.view.name()), &next);
            }
            return;
        }
        if review::mark(&item) != Some(review::Mark::Done) {
            let note = review::Note {
                verdict: review::hunks_verdict(&item),
                // Keeps a comment the item has from an earlier review.
                text: review::note(&item).text,
            };
            review::set_mark(&app.d, &item, Some(note));
        }
        if same && review::note(&item).verdict == review::Verdict::Approve {
            review::mark_same_change(&app.d, &item);
        }
        pages::render_sidebar(app);
        if guided {
            go_next(app, Some(item), true);
        } else {
            stay(app, &item);
        }
        return;
    }
    if same && verdict == review::Verdict::Approve {
        review::mark_same_change(&app.d, id);
    }
    pages::render_sidebar(app);
    if guided {
        go_next(app, Some(unit), true);
    } else {
        stay(app, &unit);
    }
}

/// After a verdict on a unit that the review steps didn't go to, or on a
/// canvas card: makes it the current unit and shows where to go next in its control.
fn stay(app: &mut App, unit: &str) {
    app.s.rv_current = Some(unit.to_string());
    app.s.rv_hunk = None;
    let next = next_steps_html(app, unit);
    review::set_stayed(Some((unit.to_string(), next)));
    keys::mark_current(app);
}

/// Where to go after reviewing `unit` without moving on: the overview's
/// call graph that lists it, and the next unreviewed unit in review order,
/// named in its button. On the canvas, that is the next unit with a card
/// there; when there is none, the button says whether the next unit in
/// review order adds a card or opens its call graph on a new canvas.
fn next_steps_html(app: &App, unit: &str) -> String {
    let d = &app.d;
    let name = |id: &str| esc(&d.display_name_of(id));
    // The way back first, so that its ◂ points away from the way on.
    let back = graphs::graph_of(d, unit).map_or(String::new(), |i| {
        format!(
            r#"<button class="rv-go-btn" data-rv-go-graph="{i}" title="The overview's call graph that lists this item">◂ Call graph</button>"#
        )
    });
    let on = if app.s.view == View::Canvas {
        match canvas_next(d, &canvas_items(app), Some(unit)) {
            CanvasNext::Card(next) => format!(
                r#"<button class="rv-go-btn" data-rv-go-next title="The next card on the canvas that needs a review, in review order (j)">Next unreviewed: <code>{}</code> ▸</button>"#,
                name(&next)
            ),
            CanvasNext::Add(next) => format!(
                r#"<button class="rv-go-btn" data-rv-go-next title="Every card on the canvas is reviewed. Adds the next item that needs a review, in review order, next to the cards it is connected to (j)">Add <code>{}</code> ▸</button>"#,
                name(&next)
            ),
            CanvasNext::NewCanvas(next) => format!(
                r#"<button class="rv-go-btn" data-rv-go-next title="Every card on the canvas is reviewed, and the next item that needs a review isn't connected to them: opens its call graph on a new canvas, in place of these cards (Ctrl+Z brings them back) (j)">Next call graph on a new canvas: <code>{}</code> ▸</button>"#,
                name(&next)
            ),
            CanvasNext::Done => String::new(),
        }
    } else {
        let next = match overview_order(app) {
            Some(order) => next_on_page(&order, Some(unit)),
            None => review::next_open(Some(unit)),
        };
        next.map_or(String::new(), |next| {
            format!(
                r#"<button class="rv-go-btn" data-rv-go-next title="The next item that needs a review, in review order (j)">Next unreviewed: <code>{}</code> ▸</button>"#,
                name(&next)
            )
        })
    };
    if back.is_empty() && on.is_empty() {
        return String::new();
    }
    format!(r#"<span class="rv-next">{back}{on}</span>"#)
}

/// Where Next ▸ goes on a canvas that shows `cards`, after unit `from`.
#[derive(Debug, PartialEq)]
enum CanvasNext {
    /// The next unit in review order that needs a review and has a card.
    Card(String),
    /// Every card is reviewed: the next unit in review order, which is
    /// connected to the cards, gets a card next to them.
    Add(String),
    /// Every card is reviewed, and the next unit in review order isn't
    /// connected to them: its call graph replaces them.
    NewCanvas(String),
    /// Nothing needs a review.
    Done,
}

fn canvas_next(d: &Data, cards: &HashSet<String>, from: Option<&str>) -> CanvasNext {
    if let Some(next) = review::next_open_where(from, |u| cards.contains(u)) {
        return CanvasNext::Card(next);
    }
    match review::next_open(from) {
        Some(next) if connected(d, cards, &next) => CanvasNext::Add(next),
        Some(next) => CanvasNext::NewCanvas(next),
        None => CanvasNext::Done,
    }
}

/// The items that have a card on the canvas (in any of its tabs), if the
/// canvas' scope shows the card.
fn canvas_items(app: &App) -> HashSet<String> {
    let c = &app.s.canvas;
    c.groups
        .iter()
        .filter(|g| c.shows(&app.d, &g.active))
        .flat_map(|g| g.tabs.iter().cloned())
        .collect()
}

/// Whether `id` calls or uses one of `cards`, is called or used by one, or
/// is in the same call graph of the overview as one.
fn connected(d: &Data, cards: &HashSet<String>, id: &str) -> bool {
    let callers = d.callers.get(id).into_iter().flatten();
    let users = d.used_by.get(id).into_iter().flatten().map(|(u, _)| u);
    let calls = d.item(id).into_iter().flat_map(|it| &it.info.calls);
    callers.chain(users).any(|c| cards.contains(c))
        || calls.map(|c| &c.to).any(|c| cards.contains(c))
        || graphs::shares_graph(d, id, cards)
}

/// Opens a new canvas with the overview's call graph that lists `unit`
/// (or only `unit`), and makes it the current unit.
fn new_canvas_for(app: &mut App, unit: &str) {
    let mut ids =
        graphs::graph_of(&app.d, unit).map_or(Vec::new(), |i| graphs::graph_items(&app.d, i));
    if !ids.iter().any(|i| i == unit) {
        ids.insert(0, unit.to_string());
    }
    canvas::show_items(app, &ids);
    review::set_stayed(None);
    app.s.rv_current = Some(unit.to_string());
    app.s.rv_hunk = None;
}

/// The dialog for writing a unit's comment.
fn open_editor(app: &App, id: &str, verdict: review::Verdict) {
    let title = match verdict {
        review::Verdict::Changes => "Request changes on",
        _ => "Comment on",
    };
    open_modal(
        &format!(
            "{title} <code>{}</code>{}",
            esc(&post::unit_name(&app.d, id)),
            esc(&post::part_label(id))
        ),
        &post::editor_html(&app.d, id, verdict),
    );
    drafts::restore_editor();
    if let Some(t) = dom::by_id("rv-text").and_then(|t| t.dyn_into::<web_sys::HtmlElement>().ok()) {
        let _ = t.focus();
    }
}

fn textarea_value(id: &str) -> String {
    dom::by_id(id)
        .and_then(|t| t.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        .map(|t| t.value())
        .unwrap_or_default()
}

/// Saves the comment in the open editor with a verdict.
fn save_from_editor(app: &mut App, verdict: review::Verdict) {
    let Some(id) = dom::query(".rv-editor").and_then(|e| dom::data(&e, "rv-for")) else {
        return;
    };
    let text = textarea_value("rv-text");
    if verdict != review::Verdict::Approve && text.trim().is_empty() {
        toast("Write a comment first.");
        return;
    }
    if let Some(field) = drafts::editor_field() {
        drafts::remove(&field);
    }
    close_modal();
    if let Some(id) = line_notes::editor_id(&id) {
        line_notes::save(app, id, verdict, text.trim().to_string());
        return;
    }
    save_verdict(app, &id, verdict, text.trim().to_string(), false);
}

/// The verdict and summary chosen in the "Finish review" dialog.
fn finish_choice() -> (review::Verdict, String) {
    let event = dom::query(r#"input[name="rv-event"]:checked"#)
        .and_then(|i| i.dyn_into::<web_sys::HtmlInputElement>().ok())
        .and_then(|i| review::Verdict::from_name(&i.value()))
        .unwrap_or(review::Verdict::Comment);
    (event, textarea_value("rv-body"))
}

/// Says again what the command posts, after the verdict or the summary
/// changed.
fn update_finish(app: &App) {
    if dom::by_id("rv-what").is_none() {
        return;
    }
    let (event, own) = finish_choice();
    let comments = post::summary(&app.d).comments;
    dom::set_html("rv-what", &post::posts_html(&comments, &own, event));
}

/// The buttons of the "Finish review" dialog: copy the review in one form.
fn finish_output(app: &App, what: &str) {
    let (event, own) = finish_choice();
    let comments = post::summary(&app.d).comments;
    let commit = app
        .d
        .out
        .meta
        .pr
        .as_ref()
        .map_or("", |p| p.head_commit.as_str());
    let (text, label) = match what {
        "md" => (
            post::review_markdown(&comments, &own, event),
            "The review as Markdown",
        ),
        "json" => (
            post::review_json(&comments, &own, event, commit, &carry::for_review()),
            "The review's JSON",
        ),
        _ => {
            let in_text = match what {
                "cmd" => false,
                "cmd-text" => true,
                _ => return,
            };
            (
                post::command(
                    &app.d,
                    &comments,
                    &own,
                    event,
                    &carry::for_review(),
                    in_text,
                )
                .unwrap_or_default(),
                "The command",
            )
        }
    };
    // Also shown, selected, in case the browser doesn't allow the
    // clipboard.
    if let Some(out) =
        dom::by_id("rv-out").and_then(|t| t.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
    {
        out.set_value(&text);
        dom::toggle_class(&out, "hidden", false);
        out.select();
    }
    copy(&text, label);
}

/// Copies a text to the clipboard and says whether that worked.
fn copy(text: &str, label: &str) {
    copy_with(
        text,
        &format!("{label} is copied."),
        "The browser didn't allow copying: the text below is selected, press Ctrl+C.",
    );
}

/// Copies `text`, then shows `done`, or `failed` if the browser refused.
fn copy_with(text: &str, done: &str, failed: &str) {
    use wasm_bindgen::{JsValue, closure::Closure};
    let done = done.to_string();
    let failed = failed.to_string();
    let ok = Closure::<dyn FnMut(JsValue)>::new(move |_| toast(&done));
    let failed = Closure::<dyn FnMut(JsValue)>::new(move |_| toast(&failed));
    let _ = dom::window()
        .navigator()
        .clipboard()
        .write_text(text)
        .then2(&ok, &failed);
    // One of them runs once, later; a copy is rare enough to keep both.
    ok.forget();
    failed.forget();
}

/// The dialog with what changed since a stale mark was made.
fn show_since_review(app: &App, id: &str) {
    let Some(now) = since::snapshot(&app.d, id) else {
        return;
    };
    let saved = review::reviewed_version(id);
    let when = saved.as_ref().map_or(String::new(), |s| {
        js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(s.at))
            .to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED)
            .as_string()
            .unwrap_or_default()
    });
    let name = match id.strip_prefix("file:") {
        Some(path) => format!("<code>{}</code>", esc(path)),
        None => app
            .d
            .item(id)
            .map(|it| format!("<code>{}</code>", esc(&app.d.display_name(it))))
            .unwrap_or_default(),
    };
    open_modal(
        &format!("Changes since your review: {name}"),
        &since::since_html(&app.d, saved.as_ref(), &now, &when),
    );
}

/// Shows the next unit that still needs a review, after unit `from`, in
/// review order: down the calls, from the changed items that nothing
/// changed calls (see `review::call_order`). It stays on the overview or
/// module page when the unit is listed there, and otherwise opens its
/// module page (or, when the canvas is open, a card for it). With `stay`
/// (after a verdict), it doesn't switch tabs: on the overview it goes on
/// with the next unit the overview lists, on a module page it skips the
/// files, and it says so when the tab has nothing left.
fn go_next(app: &mut App, from: Option<String>, stay: bool) {
    // On a canvas with cards, Next ▸ goes to the next card that needs a
    // review. Only when every card is reviewed does it add a card or open
    // a new canvas, as the button under a verdict says (see
    // `next_steps_html`).
    let cards = canvas_items(app);
    if app.s.view == View::Canvas && !cards.is_empty() {
        match canvas_next(&app.d, &cards, from.as_deref()) {
            CanvasNext::Card(next) => {
                review::set_stayed(None);
                app.s.rv_current = Some(next.clone());
                app.s.rv_hunk = None;
                // Pans only if the card is out of view.
                nav::step(|| canvas::open(app, &next, None, true));
            }
            CanvasNext::Add(next) => show_unit(app, &next, from.as_deref()),
            CanvasNext::NewCanvas(next) => new_canvas_for(app, &next),
            CanvasNext::Done => toast(&all_reviewed()),
        }
        return;
    }
    review::set_stayed(None);
    // The units this page lists, by their controls (items and files;
    // their hunks are reached from the item).
    let here: Vec<Element> = if matches!(app.s.view, View::Overview | View::Module) {
        dom::by_id(&format!("view-{}", app.s.view.name()))
            .map(|sec| dom::query_all_in(&sec, ".rvc"))
            .unwrap_or_default()
            .into_iter()
            .filter(|c| !dom::data(c, "rv").is_some_and(|id| id.contains('#')))
            .collect()
    } else {
        Vec::new()
    };
    let control = |unit: &str| {
        here.iter()
            .find(|c| dom::data(c, "rv").as_deref() == Some(unit))
            .cloned()
    };
    // The overview lists every unit: Next ▸ goes down the page, like a
    // review of the files on GitHub.
    if app.s.view == View::Overview {
        let order: Vec<String> = here.iter().filter_map(|c| dom::data(c, "rv")).collect();
        match next_on_page(&order, from.as_deref()) {
            Some(next) => {
                if let Some(c) = control(&next) {
                    nav::step(|| {
                        app.s.rv_current = Some(next);
                        app.s.rv_hunk = None;
                        reveal(&c);
                    });
                }
            }
            None if stay && review::next_open(from.as_deref()).is_some() => {
                toast("Everything on the overview is reviewed. Next ▸ goes on with the rest.");
            }
            None => match review::next_open(from.as_deref()) {
                Some(next) => show_unit(app, &next, from.as_deref()),
                None => toast(&all_reviewed()),
            },
        }
        return;
    }
    // After a verdict on a module page, files (shown on the overview)
    // come last, so that the tab doesn't change while items are left.
    let in_modules = stay && app.s.view == View::Module;
    let next =
        review::next_open_where(from.as_deref(), |u| !(in_modules && u.starts_with("file:")));
    if next.is_none() && in_modules && review::next_open(from.as_deref()).is_some() {
        toast("Every item is reviewed. Next ▸ goes on with the changed files.");
        return;
    }
    let Some(next) = next else {
        toast(&all_reviewed());
        return;
    };
    show_unit(app, &next, from.as_deref());
}

/// The first unit of `order` (the units a page lists, top to bottom)
/// after `from` that still needs a review, wrapping around.
fn next_on_page(order: &[String], from: Option<&str>) -> Option<String> {
    let start = from
        .and_then(|f| order.iter().position(|u| u == f))
        .map_or(0, |i| i + 1);
    let n = order.len();
    (0..n)
        .map(|k| &order[(start + k) % n])
        .find(|u| review::mark(u) != Some(review::Mark::Done))
        .cloned()
}

/// The units the overview lists, top to bottom, if it is the open view:
/// the order of the review steps there.
pub fn overview_order(app: &App) -> Option<Vec<String>> {
    if app.s.view != View::Overview {
        return None;
    }
    let sec = dom::by_id("view-overview")?;
    let mut seen = HashSet::new();
    Some(
        dom::query_all_in(&sec, ".rvc")
            .iter()
            .filter_map(|c| dom::data(c, "rv"))
            .filter(|id| !id.contains('#') && seen.insert(id.clone()))
            .collect(),
    )
}

/// Makes `unit` the current unit and shows it: on the overview or module
/// page that is open if it is listed there, else on its module's page (a
/// file on the overview), or as a card next to `from` when the canvas is
/// open.
fn show_unit(app: &mut App, unit: &str, from: Option<&str>) {
    nav::step(|| show_unit_now(app, unit, from));
}

fn show_unit_now(app: &mut App, unit: &str, from: Option<&str>) {
    review::set_stayed(None);
    app.s.rv_current = Some(unit.to_string());
    app.s.rv_hunk = None;
    if matches!(app.s.view, View::Overview | View::Module)
        && reveal_unit(&format!("view-{}", app.s.view.name()), unit)
    {
        return;
    }
    if unit.starts_with("file:") {
        set_view(app, View::Overview);
        reveal_unit("view-overview", unit);
        return;
    }
    if app.s.view == View::Canvas {
        canvas::open_for_review(app, unit, from);
        return;
    }
    if app.s.view == View::Calls {
        calls::focus(app, unit);
        return;
    }
    let Some(module) = app.d.item(unit).map(|it| app.d.mod_of(it).to_string()) else {
        return;
    };
    app.s.module = module;
    set_view(app, View::Module);
    if !reveal_unit("view-module", unit) {
        // Not listed on a page (shouldn't happen): the canvas shows anything.
        canvas::open(app, unit, None, true);
    }
}

/// What the toast says when nothing is left to review.
fn all_reviewed() -> String {
    match review::progress() {
        (_, 0) => "Nothing to review.".to_string(),
        (_, total) => format!("All {total} items and files are reviewed."),
    }
}

/// Scrolls to the control of `unit` in a view. Returns whether it is there.
fn reveal_unit(section: &str, unit: &str) -> bool {
    let Some(sec) = dom::by_id(section) else {
        return false;
    };
    match dom::query_all_in(&sec, ".rvc")
        .into_iter()
        .find(|c| dom::data(c, "rv").as_deref() == Some(unit))
    {
        Some(c) => {
            reveal(&c);
            true
        }
        None => false,
    }
}

/// Scrolls to item `id`'s row on the page that is shown, with a history
/// entry, so that Back returns to the link that led there.
fn jump_to_row(app: &mut App, id: String, row: &Element) {
    app.s.anchor = Some(nav::PageAnchor {
        view: app.s.view,
        module: app.s.module.clone(),
        to: nav::Anchor::Item(id),
    });
    reveal(row);
    nav::record(app);
}

/// The row of item `id` in the view that `link` is in, if that view shows
/// the item.
fn row_on_page(link: &Element, id: &str) -> Option<Element> {
    let view = dom::closest(link, "[id^='view-']")?;
    item_row_in(&view, id)
}

fn item_row_in(view: &Element, id: &str) -> Option<Element> {
    dom::query_all_in(&view.clone().into(), ".irow[data-id], .impl-line[data-id]")
        .into_iter()
        .find(|row| dom::data(row, "id").as_deref() == Some(id))
}

/// The element on a page that an anchor of its history entry names.
fn anchor_el(view: View, anchor: &nav::Anchor) -> Option<Element> {
    let sec = dom::by_id(&format!("view-{}", view.name()))?;
    match anchor {
        nav::Anchor::Item(id) => item_row_in(&sec, id),
        nav::Anchor::File(path) => dom::query_all_in(&sec.into(), ".ientry.file[data-file]")
            .into_iter()
            .find(|c| dom::data(c, "file").as_deref() == Some(path.as_str())),
    }
}

/// Outlines the current review unit once the views have drawn what a
/// key or click changed (the canvas draws some of it a frame later).
fn mark_current_later() {
    dom::raf(|| {
        dom::raf(|| {
            with_app(|app| keys::mark_current(app));
        })
    });
}

/// Scrolls the row of a review control into view and highlights it.
fn reveal(control: &Element) {
    open_groups_around(control);
    let row = keys::row_of(control);
    let opts = web_sys::ScrollIntoViewOptions::new();
    opts.set_block(web_sys::ScrollLogicalPosition::Start);
    if dom::closest(&row, "#view-overview").is_some() {
        // The overview renders the bodies of its rows once they come near
        // the view (see `listing::observe`), so the rows above the target
        // grow after a jump. A smooth scroll would render those it passes
        // and end short of the target, so the jump is instant, and is
        // made again once the rows around the target have their height.
        listing::fill_in(&row);
        row.scroll_into_view_with_scroll_into_view_options(&opts);
        let again = row.clone();
        dom::raf(move || {
            dom::raf(move || {
                let opts = web_sys::ScrollIntoViewOptions::new();
                opts.set_block(web_sys::ScrollLogicalPosition::Start);
                again.scroll_into_view_with_scroll_into_view_options(&opts);
            })
        });
    } else {
        opts.set_behavior(web_sys::ScrollBehavior::Smooth);
        row.scroll_into_view_with_scroll_into_view_options(&opts);
    }
    dom::toggle_class(&row, "rv-flash", false);
    dom::raf(move || {
        dom::toggle_class(&row, "rv-flash", true);
    });
}

/// Opens the closed groups (generated files, for example) that `el` is in.
fn open_groups_around(el: &Element) {
    let mut up = el.parent_element();
    while let Some(el) = up {
        if let Some(det) = el.dyn_ref::<web_sys::HtmlDetailsElement>()
            && !det.open()
        {
            det.set_open(true);
        }
        up = el.parent_element();
    }
}

/// Tooltip with the fully qualified path of a hovered name.
/// Linked hover in signature diffs: hovering one part of a change, or its
/// note, highlights every part of that change.
fn highlight_diff_group(el: &Element) {
    for hot in dom::query_all(".g-hot") {
        dom::toggle_class(&hot, "g-hot", false);
    }
    let Some(block) = dom::closest(el, ".sig-block") else {
        return;
    };
    let groups: Vec<String> = if let Some(note) = dom::closest(el, "li[data-gs]") {
        dom::data(&note, "gs")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    } else if let Some(part) = dom::closest(el, "[data-g]") {
        dom::data(&part, "g").into_iter().collect()
    } else {
        return;
    };
    for g in &groups {
        for part in dom::query_all_in(&block.clone().into(), &format!(r#"[data-g="{g}"]"#)) {
            dom::toggle_class(&part, "g-hot", true);
        }
    }
    for note in dom::query_all_in(&block.into(), "li[data-gs]") {
        let gs = dom::data(&note, "gs").unwrap_or_default();
        if gs.split_whitespace().any(|g| groups.iter().any(|h| h == g)) {
            dom::toggle_class(&note, "g-hot", true);
        }
    }
}

fn on_mouse_over(app: &mut App, el: &Element) {
    highlight_diff_group(el);
    if app.s.view == View::Space && app.s.space.mode == space::Mode::Map {
        map::hover(el);
    }
    for c in dom::query_all(".card.hover-target") {
        if app.s.canvas.drag.is_none() {
            dom::toggle_class(&c, "hover-target", false);
        }
    }
    outline::on_hover(app, el);
    // No hover previews for taps: the tap opens the item anyway.
    let Some(r) = dom::closest(el, ".ref").filter(|_| !TOUCH.get()) else {
        hide_tooltip();
        return;
    };
    let path = dom::data(&r, "path").unwrap_or_default();
    let d = app.d.clone();
    let mut html = format!(r#"<div class="tp">{}</div>"#, esc(&path));
    match dom::data(&r, "id").and_then(|id| d.item(&id).map(|_| id)) {
        Some(id) => {
            let it = d.item(&id).unwrap();
            html.push_str(&format!(
                "<div>{}{}{} · {}</div>",
                it.info.kind.label(),
                if it.info.test { " · test" } else { "" },
                match it
                    .unsafe_code
                    .iter()
                    .any(|c| c.status != model::Status::Removed)
                {
                    true => " · <b>⚠ changes unsafe code</b>",
                    false if !it.unsafe_code.is_empty() => " · removes unsafe code",
                    false => "",
                },
                if d.external(&id).is_some() {
                    "outside the analyzed crates · click for its docs".to_string()
                } else {
                    let st = d.status_html(it);
                    if st.is_empty() {
                        format!(r#"<span class="st st-{0}">{0}</span>"#, it.status.css())
                    } else {
                        st
                    }
                }
            ));
            if it.info.kind != model::Kind::Mod {
                html.push_str(&format!(
                    "<pre>{}</pre>",
                    esc(&it.info.sig.lines().take(6).collect::<Vec<_>>().join("\n"))
                ));
            }
            if d.external(&id).is_some() {
                let first = code::first_paragraph(&it.info.docs);
                if !first.is_empty() {
                    html.push_str(&code::docs_html(&d, first, &Default::default()));
                }
            }
            canvas::highlight_card_of(app, &id);
        }
        None => html.push_str(r#"<div class="dim">outside the analyzed crates</div>"#),
    }
    show_tooltip(&html);
}

fn search_query() -> String {
    dom::by_id("search")
        .and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok())
        .map(|i| i.value().trim().to_lowercase())
        .unwrap_or_default()
}

fn render_search(app: &mut App) {
    let Some(b) = dom::by_id("search-results") else {
        return;
    };
    let q = search_query();
    if q.is_empty() {
        // No stale hit for Enter to open.
        app.s.search.clear();
        dom::toggle_class(&b, "open", false);
        return;
    }
    let d = app.d.clone();
    let mut results: Vec<&model::Item> = d
        .items()
        .filter(|it| {
            it.info.id.to_lowercase().contains(&q) || d.display_name(it).to_lowercase().contains(&q)
        })
        .collect();
    results.sort_by(|a, b| {
        changed(b)
            .cmp(&changed(a))
            .then(a.info.id.len().cmp(&b.info.id.len()))
    });
    results.truncate(30);
    app.s.search = results.iter().map(|it| it.info.id.clone()).collect();
    app.s.search_sel = app.s.search_sel.min(results.len().saturating_sub(1));
    let html: String = results
        .iter()
        .enumerate()
        .map(|(i, it)| {
            format!(
                r#"<div class="{}" data-i="{i}"><span class="dot {}"></span> <span class="kind">{}</span> {} {}{}</div>"#,
                if i == app.s.search_sel { "sel" } else { "" },
                it.status.css(),
                it.info.kind.label(),
                esc(d.short_path(&it.info.id)),
                data::test_tag_of(it),
                unsafety::tag(it),
            )
        })
        .collect();
    b.set_inner_html(if html.is_empty() {
        r#"<div class="dim">No match</div>"#
    } else {
        &html
    });
    dom::toggle_class(&b, "open", true);
}

fn on_search_key(app: &mut App, ev: &KeyboardEvent) {
    match ev.key().as_str() {
        "ArrowDown" => {
            app.s.search_sel = (app.s.search_sel + 1).min(app.s.search.len().saturating_sub(1));
            render_search(app);
            ev.prevent_default();
        }
        "ArrowUp" => {
            app.s.search_sel = app.s.search_sel.saturating_sub(1);
            render_search(app);
            ev.prevent_default();
        }
        "Enter" => {
            let sel = app.s.search_sel;
            pick_search(app, sel);
        }
        "Escape" => {
            if let Some(i) =
                dom::by_id("search").and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok())
            {
                i.set_value("");
            }
            render_search(app);
        }
        _ => {}
    }
}

fn pick_search(app: &mut App, i: usize) {
    let Some(id) = app.s.search.get(i).cloned() else {
        return;
    };
    if let Some(input) =
        dom::by_id("search").and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok())
    {
        input.set_value("");
    }
    if let Some(b) = dom::by_id("search-results") {
        dom::toggle_class(&b, "open", false);
    }
    if app
        .d
        .item(&id)
        .is_some_and(|it| it.info.kind == model::Kind::Mod)
    {
        app.s.module = id;
        set_view(app, View::Module);
    } else {
        open_item(app, &id);
    }
}
