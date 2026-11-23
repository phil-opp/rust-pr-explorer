//! Front end of the Rust PR Explorer page, compiled to WebAssembly.
//!
//! The analysis is embedded in the page as JSON. Views are rendered as
//! HTML strings; one listener per event type on the document dispatches
//! on `data-*` attributes of the element that was clicked or dragged.

mod canvas;
mod code;
mod data;
mod dom;
mod files;
mod js;
mod map;
mod nav;
mod pages;
mod post;
mod review;
mod route;
mod settings;
mod since;
mod space;
mod yaml;

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
    /// Whether the sidebar lists unchanged modules too.
    pub side_all: bool,
    pub canvas: canvas::Canvas,
    /// The item last opened on the canvas from outside it, for the history
    /// entry (see [`nav`]).
    pub canvas_focus: Option<String>,
    pub space: space::Space,
    search: Vec<String>,
    search_sel: usize,
    /// The unit last shown by "next unreviewed", where the next search
    /// continues.
    rv_current: Option<String>,
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
            canvas: canvas::Canvas::new(root),
            canvas_focus: None,
            space: space::Space::new(),
            search: Vec::new(),
            search_sel: 0,
            rv_current: None,
        },
        d,
    };
    settings::load();
    review::load(&app.d);
    let title = app.d.compared_html();
    dom::set_html("pr-title", &title);
    APP.with(|a| *a.borrow_mut() = Some(app));
    install_listeners();
    with_app(nav::start);
}

/// Shows a view, and adds a history entry for it if it differs from the
/// current one.
pub fn set_view(app: &mut App, view: View) {
    if view != View::Canvas {
        app.s.canvas_focus = None;
    }
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
    // Show or hide the sidebar first, so that the view lays out at its final width.
    if let Some(el) = dom::by_id("sidebar") {
        dom::toggle_class(&el, "hidden", !pages::sidebar_shown(app));
    }
    match view {
        View::Overview => pages::render_overview(app),
        View::Module => pages::render_module_page(app),
        View::Canvas => canvas::render(app),
        View::Space => space::show(app),
    }
    pages::render_sidebar(app);
    nav::record(app);
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
            let _ = canvas::on_pointer_down(app, &el, &pev);
        });
    });
    dom::listen(&doc, "pointermove", |ev| {
        let Ok(pev) = ev.dyn_into::<PointerEvent>() else {
            return;
        };
        with_app(|app| {
            canvas::on_pointer_move(app, &pev);
        });
    });
    dom::listen(&doc, "pointerup", |ev| {
        let Ok(pev) = ev.dyn_into::<PointerEvent>() else {
            return;
        };
        with_app(|app| {
            canvas::on_pointer_up(app, &pev);
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
            } else {
                let _ = space::on_change(app, &el);
            }
        });
    });
    // Not passive: Ctrl+Enter in the comment editor prevents the newline.
    dom::listen_with(&doc, "keydown", false, false, |ev| {
        // `n`: the next unreviewed item, in review mode, unless typing.
        if let Some(k) = ev.dyn_ref::<KeyboardEvent>()
            && k.key() == "n"
            && !(k.ctrl_key() || k.meta_key() || k.alt_key())
            && review::is_on()
            && !dom::target_element(&ev)
                .is_some_and(|t| matches!(t.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT"))
        {
            with_app(|app| {
                let from = app.s.rv_current.clone();
                go_next(app, from);
            });
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
            return;
        }
        if ev
            .dyn_ref::<KeyboardEvent>()
            .is_some_and(|k| k.key() == "Escape")
        {
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
        with_app(nav::on_hash_change);
    });
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
    if let Some(f) = dom::closest(el, "#sidebar [data-goto-file]") {
        let path = dom::data(&f, "goto-file").unwrap_or_default();
        set_view(app, View::Overview);
        if let Some(card) = dom::query_all("#view-overview .ientry.file[data-file]")
            .into_iter()
            .find(|c| dom::data(c, "file").as_deref() == Some(path.as_str()))
        {
            reveal(&card);
        }
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

/// Clicks on review controls. Returns whether the click was handled.
fn on_review_click(app: &mut App, el: &Element) -> bool {
    if let Some(c) = dom::closest(el, ".rvc") {
        let id = dom::data(&c, "rv").unwrap_or_default();
        if dom::closest(el, "[data-rv-since]").is_some() {
            show_since_review(app, &id);
        } else if let Some(act) = dom::closest(el, "[data-rv-act]")
            .and_then(|b| dom::data(&b, "rv-act"))
            .and_then(|a| review::Verdict::from_name(&a))
        {
            if act == review::Verdict::Approve {
                // Keeps a comment written earlier.
                let text = review::note(&id).text;
                save_verdict(app, &id, act, text);
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
        return true;
    }
    if dom::closest(el, "#rv-mode").is_some() {
        review::set_on(!review::is_on());
        pages::render_sidebar(app);
        return true;
    }
    if dom::closest(el, "#rv-next").is_some() {
        let from = app.s.rv_current.clone();
        go_next(app, from);
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
            set_view(app, app.s.view);
        }
        return true;
    }
    false
}

/// Gives a unit a verdict and moves on to the next unreviewed one.
/// For a hunk: moves on to the item's next hunk without a verdict, and
/// when there is none, gives the item the verdict its hunks add up to.
fn save_verdict(app: &mut App, id: &str, verdict: review::Verdict, text: String) {
    review::set_mark(&app.d, id, Some(review::Note { verdict, text }));
    if let Some(item) = review::hunk_item(id) {
        app.s.rv_current = Some(item.clone());
        if let Some(next) = review::next_open_hunk(id) {
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
        pages::render_sidebar(app);
        go_next(app, Some(item));
        return;
    }
    pages::render_sidebar(app);
    go_next(app, Some(id.to_string()));
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
    close_modal();
    save_verdict(app, &id, verdict, text.trim().to_string());
}

/// The buttons of the "Finish review" dialog: download, or copy, the
/// review in one form.
fn finish_output(app: &App, what: &str) {
    let event = dom::query(r#"input[name="rv-event"]:checked"#)
        .and_then(|i| i.dyn_into::<web_sys::HtmlInputElement>().ok())
        .and_then(|i| review::Verdict::from_name(&i.value()))
        .unwrap_or(review::Verdict::Comment);
    let own = textarea_value("rv-body");
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
        "cmd" => (post::command(&app.d).unwrap_or_default(), "The command"),
        _ => (
            post::review_json(&comments, &own, event, commit),
            "The review's JSON",
        ),
    };
    // Also shown, selected, in case the browser doesn't allow the
    // download or the clipboard.
    if let Some(out) =
        dom::by_id("rv-out").and_then(|t| t.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
    {
        out.set_value(&text);
        dom::toggle_class(&out, "hidden", false);
        out.select();
    }
    if what == "download" {
        if download(&post::file_name(&app.d), &text) {
            toast("Downloaded. Run the command to post it.");
        } else {
            toast("The browser didn't allow the download: save the text below as the file.");
        }
        return;
    }
    copy(&text, label);
}

/// Offers a text as a file to save.
fn download(name: &str, text: &str) -> bool {
    let parts = js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(text));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("application/json");
    let Ok(blob) = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts) else {
        return false;
    };
    let Ok(url) = web_sys::Url::create_object_url_with_blob(&blob) else {
        return false;
    };
    let Some(a) = dom::document()
        .create_element("a")
        .ok()
        .and_then(|a| a.dyn_into::<web_sys::HtmlAnchorElement>().ok())
    else {
        return false;
    };
    a.set_href(&url);
    a.set_download(name);
    a.click();
    dom::timeout(1000, move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    true
}

/// Copies a text to the clipboard and says whether that worked.
fn copy(text: &str, label: &str) {
    use wasm_bindgen::{JsValue, closure::Closure};
    let done = format!("{label} is copied.");
    let ok = Closure::<dyn FnMut(JsValue)>::new(move |_| toast(&done));
    let failed = Closure::<dyn FnMut(JsValue)>::new(|_| {
        toast("The browser didn't allow copying: the text below is selected, press Ctrl+C.")
    });
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

/// Shows the next unit that still needs a review, after unit `from`: on
/// the overview and module pages the next one further down the page, then
/// the next one in reading order (on its module page, or on the canvas
/// when the canvas is open).
fn go_next(app: &mut App, from: Option<String>) {
    if matches!(app.s.view, View::Overview | View::Module)
        && let Some(sec) = dom::by_id(&format!("view-{}", app.s.view.name()))
    {
        // Items and files; their hunks are reached from the item.
        let controls: Vec<Element> = dom::query_all_in(&sec, ".rvc")
            .into_iter()
            .filter(|c| !dom::data(c, "rv").is_some_and(|id| id.contains('#')))
            .collect();
        let start = from
            .as_deref()
            .and_then(|f| {
                controls
                    .iter()
                    .position(|c| dom::data(c, "rv").as_deref() == Some(f))
            })
            .map_or(0, |i| i + 1);
        if let Some(c) = controls[start.min(controls.len())..]
            .iter()
            .find(|c| !c.class_list().contains("done"))
        {
            app.s.rv_current = dom::data(c, "rv");
            reveal(c);
            return;
        }
    }
    let Some(next) = review::next_open(from.as_deref()) else {
        let (_, total) = review::progress();
        toast(&if total == 0 {
            "Nothing to review.".to_string()
        } else {
            format!("All {total} items and files are reviewed.")
        });
        return;
    };
    app.s.rv_current = Some(next.clone());
    if next.starts_with("file:") {
        set_view(app, View::Overview);
        reveal_unit("view-overview", &next);
        return;
    }
    if app.s.view == View::Canvas {
        canvas::open_for_review(app, &next, from.as_deref());
        return;
    }
    let Some(module) = app.d.item(&next).map(|it| app.d.mod_of(it).to_string()) else {
        return;
    };
    app.s.module = module;
    set_view(app, View::Module);
    if !reveal_unit("view-module", &next) {
        // Not listed on a page (shouldn't happen): the canvas shows anything.
        canvas::open(app, &next, None, true);
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

/// Scrolls the row of a review control into view and highlights it.
fn reveal(control: &Element) {
    // Inside a closed group (generated files, for example): open it.
    let mut up = control.parent_element();
    while let Some(el) = up {
        if let Some(det) = el.dyn_ref::<web_sys::HtmlDetailsElement>()
            && !det.open()
        {
            det.set_open(true);
        }
        up = el.parent_element();
    }
    let row = dom::closest(control, ".hunk-rv, .irow, .impl-line, .ientry, .card")
        .unwrap_or_else(|| control.clone());
    let opts = web_sys::ScrollIntoViewOptions::new();
    opts.set_block(web_sys::ScrollLogicalPosition::Start);
    opts.set_behavior(web_sys::ScrollBehavior::Smooth);
    row.scroll_into_view_with_scroll_into_view_options(&opts);
    dom::toggle_class(&row, "rv-flash", false);
    dom::raf(move || {
        dom::toggle_class(&row, "rv-flash", true);
    });
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
    let Some(r) = dom::closest(el, ".ref") else {
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
                "<div>{} · {}</div>",
                it.info.kind.label(),
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
