//! Front end of the Rust PR Explorer page, compiled to WebAssembly.
//!
//! The analysis is embedded in the page as JSON. Views are rendered as
//! HTML strings; one listener per event type on the document dispatches
//! on `data-*` attributes of the element that was clicked or dragged.

mod canvas;
mod code;
mod data;
mod dom;
mod pages;
mod space;

use code::esc;
use data::{Data, changed};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::wasm_bindgen;
use web_sys::{Element, Event, KeyboardEvent, PointerEvent, WheelEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Overview,
    Module,
    Canvas,
    Space,
}

impl View {
    fn name(self) -> &'static str {
        match self {
            View::Overview => "overview",
            View::Module => "module",
            View::Canvas => "canvas",
            View::Space => "space",
        }
    }

    fn from_name(name: &str) -> Option<View> {
        [View::Overview, View::Module, View::Canvas, View::Space]
            .into_iter()
            .find(|v| v.name() == name)
    }
}

pub struct State {
    pub view: View,
    /// Module shown on the module page.
    pub module: String,
    pub show_unchanged: bool,
    pub canvas: canvas::Canvas,
    pub space: space::Space,
    search: Vec<String>,
    search_sel: usize,
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
            canvas: canvas::Canvas::new(root),
            space: space::Space::new(),
            search: Vec::new(),
            search_sel: 0,
        },
        d,
    };
    let meta = &app.d.out.meta;
    let title = if meta.title.contains(&meta.base) {
        format!("<b>{}</b>", esc(&meta.title))
    } else {
        format!(
            "<b>{}</b> &nbsp;{} → {}",
            esc(&meta.title),
            esc(&meta.base),
            esc(&meta.head)
        )
    };
    dom::set_html("pr-title", &title);
    APP.with(|a| *a.borrow_mut() = Some(app));
    install_listeners();
    with_app(|app| set_view(app, View::Overview));
}

pub fn set_view(app: &mut App, view: View) {
    app.s.view = view;
    for b in dom::query_all("#views button") {
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
    match view {
        View::Overview => pages::render_overview(app),
        View::Module => pages::render_module_page(app),
        View::Canvas => canvas::render(app),
        View::Space => space::show(app),
    }
    pages::render_sidebar(app);
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

fn install_listeners() {
    let doc: web_sys::EventTarget = dom::document().into();
    // Not passive: clicks on cross links prevent the default navigation.
    dom::listen_with(&doc, "click", false, false, |ev| {
        if let Some(el) = dom::target_element(&ev) {
            with_app(|app| on_click(app, &el, &ev));
        }
    });
    // Not passive: starting a card drag prevents text selection.
    dom::listen_with(&doc, "pointerdown", false, false, |ev| {
        let (Some(el), Ok(pev)) = (dom::target_element(&ev), ev.dyn_into::<PointerEvent>()) else {
            return;
        };
        with_app(|app| {
            // Search results are picked on press, before the input's blur hides them.
            if let Some(r) = dom::closest(&el, "#search-results [data-i]") {
                let i = dom::data(&r, "i").and_then(|i| i.parse().ok()).unwrap_or(0);
                pick_search(app, i);
                return;
            }
            let _ =
                canvas::on_pointer_down(app, &el, &pev) || space::on_pointer_down(app, &el, &pev);
        });
    });
    dom::listen(&doc, "pointermove", |ev| {
        let Ok(pev) = ev.dyn_into::<PointerEvent>() else {
            return;
        };
        with_app(|app| {
            canvas::on_pointer_move(app, &pev);
            if app.s.view == View::Space {
                space::on_pointer_move(app, &pev);
            }
        });
    });
    dom::listen(&doc, "pointerup", |ev| {
        let Ok(pev) = ev.dyn_into::<PointerEvent>() else {
            return;
        };
        with_app(|app| {
            canvas::on_pointer_up(app, &pev);
            space::on_pointer_up(app, &pev);
        });
    });
    // Not passive: zooming prevents the page from scrolling.
    dom::listen_with(&doc, "wheel", false, false, |ev| {
        let (Some(el), Ok(wev)) = (dom::target_element(&ev), ev.dyn_into::<WheelEvent>()) else {
            return;
        };
        with_app(|app| {
            if dom::closest(&el, "#viewport").is_some() {
                canvas::on_wheel(app, &el, &wev);
            } else if el.id() == "space" {
                space::on_wheel(app, &wev);
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
            if el.id() == "show-unchanged" {
                app.s.show_unchanged = el
                    .dyn_ref::<web_sys::HtmlInputElement>()
                    .is_some_and(|i| i.checked());
                pages::render_module_page(app);
            } else {
                space::on_change(app, &el);
            }
        });
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
    dom::listen(&dom::window().into(), "resize", |_| {
        with_app(|app| match app.s.view {
            View::Canvas => canvas::draw_edges(app),
            View::Space => space::draw(app),
            _ => {}
        });
    });
}

fn on_click(app: &mut App, el: &Element, ev: &Event) {
    if dom::closest(el, "#view-canvas").is_some() && canvas::on_click(app, el) {
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
            None => canvas::open(app, &id, None, true),
        }
        return;
    }
    if let Some(b) = dom::closest(el, "[data-open]") {
        canvas::open(app, &dom::data(&b, "open").unwrap_or_default(), None, true);
        return;
    }
    if let Some(b) = dom::closest(el, "[data-graph]") {
        canvas::show_module_graph(app, &dom::data(&b, "graph").unwrap_or_default());
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
        canvas::show_module_graph(app, &m);
    }
}

/// Tooltip with the fully qualified path of a hovered name.
fn on_mouse_over(app: &mut App, el: &Element) {
    for c in dom::query_all(".card.hover-target") {
        if app.s.canvas.drag.is_none() {
            dom::toggle_class(&c, "hover-target", false);
        }
    }
    let Some(r) = dom::closest(el, ".ref") else {
        if app.s.view != View::Space {
            hide_tooltip();
        }
        return;
    };
    let path = dom::data(&r, "path").unwrap_or_default();
    let d = app.d.clone();
    let mut html = format!(r#"<div class="tp">{}</div>"#, esc(&path));
    match dom::data(&r, "id").and_then(|id| d.item(&id).map(|_| id)) {
        Some(id) => {
            let it = d.item(&id).unwrap();
            html.push_str(&format!(
                "<div>{} · {}{}</div>",
                it.info.kind.label(),
                d.status_html(it),
                if r.class_list().contains("inferred") {
                    " · <i>guessed from the method name</i>"
                } else {
                    ""
                }
            ));
            if it.info.kind != model::Kind::Mod {
                html.push_str(&format!(
                    "<pre>{}</pre>",
                    esc(&it.info.sig.lines().take(6).collect::<Vec<_>>().join("\n"))
                ));
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
                r#"<div class="{}" data-i="{i}"><span class="dot {}"></span> <span class="kind">{}</span> {}</div>"#,
                if i == app.s.search_sel { "sel" } else { "" },
                it.status.css(),
                it.info.kind.label(),
                esc(d.short_path(&it.info.id))
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
        canvas::open(app, &id, None, true);
    }
}
