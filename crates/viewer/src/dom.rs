//! Small helpers over `web-sys`.

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{
    AddEventListenerOptions, Document, Element, Event, EventTarget, HtmlElement, Window,
};

pub fn window() -> Window {
    web_sys::window().expect("no window")
}

pub fn document() -> Document {
    window().document().expect("no document")
}

pub fn by_id(id: &str) -> Option<Element> {
    document().get_element_by_id(id)
}

pub fn html_by_id(id: &str) -> Option<HtmlElement> {
    by_id(id).and_then(|e| e.dyn_into().ok())
}

pub fn set_html(id: &str, html: &str) {
    if let Some(el) = by_id(id) {
        el.set_inner_html(html);
    }
}

pub fn query(sel: &str) -> Option<Element> {
    document().query_selector(sel).ok().flatten()
}

pub fn query_all(sel: &str) -> Vec<Element> {
    query_all_in(&document().into(), sel)
}

pub fn query_all_in(root: &web_sys::Node, sel: &str) -> Vec<Element> {
    let list = if let Some(el) = root.dyn_ref::<Element>() {
        el.query_selector_all(sel)
    } else if let Some(doc) = root.dyn_ref::<Document>() {
        doc.query_selector_all(sel)
    } else {
        return Vec::new();
    };
    let Ok(list) = list else { return Vec::new() };
    (0..list.length())
        .filter_map(|i| list.item(i))
        .filter_map(|n| n.dyn_into::<Element>().ok())
        .collect()
}

/// The element an event happened on (or the parent of a text node).
pub fn target_element(event: &Event) -> Option<Element> {
    let target = event.target()?;
    match target.dyn_into::<Element>() {
        Ok(el) => Some(el),
        Err(t) => t.dyn_into::<web_sys::Node>().ok()?.parent_element(),
    }
}

pub fn closest(el: &Element, sel: &str) -> Option<Element> {
    el.closest(sel).ok().flatten()
}

pub fn data(el: &Element, key: &str) -> Option<String> {
    el.get_attribute(&format!("data-{key}"))
}

pub fn set_style(el: &Element, prop: &str, value: &str) {
    if let Some(h) = el.dyn_ref::<HtmlElement>() {
        let _ = h.style().set_property(prop, value);
    }
}

pub fn offset_size(el: &Element) -> (f64, f64) {
    match el.dyn_ref::<HtmlElement>() {
        Some(h) => (h.offset_width() as f64, h.offset_height() as f64),
        None => (0.0, 0.0),
    }
}

pub fn toggle_class(el: &Element, class: &str, on: bool) {
    let _ = el.class_list().toggle_with_force(class, on);
}

/// Runs `f` before the next repaint.
pub fn raf(f: impl FnOnce() + 'static) {
    let cb = Closure::once_into_js(f);
    let _ = window().request_animation_frame(cb.unchecked_ref());
}

pub fn timeout(ms: i32, f: impl FnOnce() + 'static) {
    let cb = Closure::once_into_js(f);
    let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(cb.unchecked_ref(), ms);
}

/// Adds an event listener that lives as long as the page.
pub fn listen(target: &EventTarget, event: &str, f: impl FnMut(Event) + 'static) {
    listen_with(target, event, false, true, f);
}

pub fn listen_with(
    target: &EventTarget,
    event: &str,
    capture: bool,
    passive: bool,
    f: impl FnMut(Event) + 'static,
) {
    let cb = Closure::<dyn FnMut(Event)>::new(f);
    let opts = AddEventListenerOptions::new();
    opts.set_capture(capture);
    opts.set_passive(passive);
    let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
        event,
        cb.as_ref().unchecked_ref(),
        &opts,
    );
    cb.forget();
}
