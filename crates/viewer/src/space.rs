//! The structure view: how a change spreads over the modules, as a module
//! map or as a module-by-module matrix (both drawn in [`crate::map`]).

use crate::{App, dom};
use wasm_bindgen::JsCast;
use web_sys::Element;

/// How the module structure is shown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Module tree with arrows between modules.
    Map,
    /// Module-by-module call counts.
    Matrix,
}

pub struct Space {
    pub mode: Mode,
    pub show_unchanged: bool,
    pub show_uses: bool,
}

impl Space {
    pub fn new() -> Space {
        Space {
            mode: Mode::Map,
            show_unchanged: false,
            show_uses: false,
        }
    }
}

fn render_bar(app: &App) {
    let sp = &app.s.space;
    let mode_button = |m: Mode, value: &str, label: &str| {
        format!(
            r#"<button data-space-mode="{value}" class="{}">{label}</button>"#,
            if sp.mode == m { "on" } else { "" }
        )
    };
    let hint = match sp.mode {
        Mode::Map => "hover a module to highlight its calls · click an item to open it",
        Mode::Matrix => "click a cell to open its calls",
    };
    dom::set_html(
        "space-bar",
        &format!(
            r#"<span class="seg">{}{}</span>
            <label><input type="checkbox" id="space-uses" {}> uses</label>
            <label><input type="checkbox" id="space-unchanged" {}> all items</label>
            <span style="flex:1"></span><span class="dim">{hint}</span>"#,
            mode_button(Mode::Map, "map", "Module map"),
            mode_button(Mode::Matrix, "matrix", "Matrix"),
            if sp.show_uses { "checked" } else { "" },
            if sp.show_unchanged { "checked" } else { "" },
        ),
    );
}

pub fn show(app: &mut App) {
    render_bar(app);
    match app.s.space.mode {
        Mode::Map => crate::map::render_map(app),
        Mode::Matrix => crate::map::render_matrix(app),
    }
}

pub fn on_click(app: &mut App, el: &Element) -> bool {
    if let Some(b) = dom::closest(el, "[data-space-mode]") {
        app.s.space.mode = match dom::data(&b, "space-mode").as_deref() {
            Some("matrix") => Mode::Matrix,
            _ => Mode::Map,
        };
        show(app);
        return true;
    }
    crate::map::on_click(app, el)
}

pub fn on_change(app: &mut App, el: &Element) -> bool {
    let on = el
        .dyn_ref::<web_sys::HtmlInputElement>()
        .is_some_and(|i| i.checked());
    match el.id().as_str() {
        "space-uses" => app.s.space.show_uses = on,
        "space-unchanged" => app.s.space.show_unchanged = on,
        _ => return false,
    }
    show(app);
    true
}
