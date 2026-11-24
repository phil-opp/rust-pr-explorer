//! Viewer settings: color theme, diff layout, whitespace, and the code font.
//! They are kept in the browser's local storage, per viewer, when it allows that.

use crate::code::esc;
use crate::dom;
use std::cell::RefCell;
use wasm_bindgen::JsCast;
use web_sys::Element;

/// Light or dark colors.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Theme {
    /// The system's (the browser's) choice.
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    fn name(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Theme::System => "System",
            Theme::Light => "Light",
            Theme::Dark => "Dark",
        }
    }

    fn from_name(name: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.name() == name)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Color theme.
    pub theme: Theme,
    /// Source diffs side by side instead of inline.
    pub split: bool,
    /// Hide changes that only add, remove or move whitespace.
    pub ignore_ws: bool,
    /// Show lines that were only rewrapped (as rustfmt does) as unchanged.
    pub reformat: bool,
    /// Old and new line numbers in front of diff lines.
    pub line_numbers: bool,
    /// Code font family.
    pub font: String,
    /// Code font size in pixels.
    pub size: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::System,
            split: false,
            ignore_ws: false,
            reformat: true,
            line_numbers: true,
            font: DEFAULT_FONT.into(),
            size: 13,
        }
    }
}

/// Embedded in the page, so always available.
const DEFAULT_FONT: &str = "Iosevka";
/// Offered in the font menu; the others are used only if installed.
const FONTS: &[&str] = &[
    "Iosevka",
    "JetBrains Mono",
    "Fira Code",
    "Source Code Pro",
    "Cascadia Code",
    "Menlo",
    "Consolas",
];
const SIZES: std::ops::RangeInclusive<u32> = 10..=22;
const KEY: &str = "rust-pr-explorer.settings";

thread_local! {
    static SETTINGS: RefCell<Settings> = RefCell::new(Settings::default());
}

pub fn get() -> Settings {
    SETTINGS.with(|s| s.borrow().clone())
}

pub(crate) fn set(new: Settings) {
    SETTINGS.with(|s| *s.borrow_mut() = new);
}

impl Settings {
    fn encode(&self) -> String {
        format!(
            "theme={};split={};ws={};rf={};ln={};size={};font={}",
            self.theme.name(),
            u8::from(self.split),
            u8::from(self.ignore_ws),
            u8::from(self.reformat),
            u8::from(self.line_numbers),
            self.size,
            self.font
        )
    }

    fn decode(s: &str) -> Settings {
        let mut out = Settings::default();
        for part in s.split(';') {
            match part.split_once('=') {
                Some(("theme", v)) => out.theme = Theme::from_name(v).unwrap_or_default(),
                Some(("split", v)) => out.split = v == "1",
                Some(("ws", v)) => out.ignore_ws = v == "1",
                Some(("rf", v)) => out.reformat = v == "1",
                Some(("ln", v)) => out.line_numbers = v == "1",
                Some(("size", v)) => {
                    if let Some(n) = v.parse().ok().filter(|n| SIZES.contains(n)) {
                        out.size = n;
                    }
                }
                Some(("font", v)) => {
                    let f = clean_font(v);
                    if !f.is_empty() {
                        out.font = f;
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// A font name as typed: only characters that can't break out of the CSS
/// string it is put in.
fn clean_font(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
        .take(60)
        .collect::<String>()
        .trim()
        .to_string()
}

fn storage() -> Option<web_sys::Storage> {
    dom::window().local_storage().ok().flatten()
}

/// Reads the saved settings (if any) and applies them.
pub fn load() {
    if let Some(saved) = storage().and_then(|s| s.get_item(KEY).ok().flatten()) {
        set(Settings::decode(&saved));
    }
    apply();
}

fn save() {
    if let Some(s) = storage() {
        let _ = s.set_item(KEY, &get().encode());
    }
}

/// Sets the theme and the code font on the root element: the theme as
/// `data-theme` (none for the system's), which sets the CSS `color-scheme`,
/// the font as CSS custom properties.
fn apply() {
    let s = get();
    if let Some(root) = dom::document().document_element() {
        let _ = match s.theme {
            Theme::System => root.remove_attribute("data-theme"),
            t => root.set_attribute("data-theme", t.name()),
        };
        dom::set_style(
            &root,
            "--code-font",
            &format!("\"{}\", var(--mono)", clean_font(&s.font)),
        );
        dom::set_style(&root, "--code-size", &format!("{}px", s.size));
        dom::toggle_class(&root, "no-lno", !s.line_numbers);
    }
}

pub fn panel_html() -> String {
    let s = get();
    let custom = !FONTS.contains(&s.font.as_str());
    let fonts: String = FONTS
        .iter()
        .map(|f| {
            format!(
                r#"<option value="{f}" {}>{f}{}</option>"#,
                if *f == s.font { "selected" } else { "" },
                if *f == DEFAULT_FONT { " (default)" } else { "" }
            )
        })
        .chain(std::iter::once(format!(
            r#"<option value="" {}>Other…</option>"#,
            if custom { "selected" } else { "" }
        )))
        .collect();
    let sizes: String = SIZES
        .map(|n| {
            format!(
                r#"<option value="{n}" {}>{n} px</option>"#,
                if n == s.size { "selected" } else { "" }
            )
        })
        .collect();
    let themes: String = Theme::ALL
        .into_iter()
        .map(|t| {
            format!(
                r#"<label><input type="radio" name="set-theme" value="{}" {}> {}</label>"#,
                t.name(),
                if t == s.theme { "checked" } else { "" },
                t.label()
            )
        })
        .collect();
    format!(
        r#"<div class="set-h">Theme</div>
        <div class="set-row">{themes}</div>
        <div class="set-h">Diffs</div>
        <label><input type="radio" name="set-layout" value="inline" {}> Inline</label>
        <label><input type="radio" name="set-layout" value="split" {}> Side by side</label>
        <label><input type="checkbox" id="set-ws" {}> Hide whitespace changes</label>
        <label title="Lines that only got wrapped differently, as by rustfmt"><input type="checkbox" id="set-rf" {}> Hide reformatting</label>
        <label><input type="checkbox" id="set-ln" {}> Line numbers</label>
        <div class="set-h">Code</div>
        <label>Font <select id="set-font">{fonts}</select></label>
        <label class="{}">Font name <input id="set-font-custom" type="text" value="{}" placeholder="installed font"></label>
        <label>Size <select id="set-size">{sizes}</select></label>
        <div class="set-note">Iosevka is embedded in the page; other fonts must be installed. Settings are kept in this browser.</div>"#,
        if s.split { "" } else { "checked" },
        if s.split { "checked" } else { "" },
        if s.ignore_ws { "checked" } else { "" },
        if s.reformat { "checked" } else { "" },
        if s.line_numbers { "checked" } else { "" },
        if custom { "" } else { "hidden" },
        if custom { esc(&s.font) } else { String::new() },
    )
}

/// Handles a change in the settings panel. Returns whether the views need
/// to be rendered again.
pub fn on_change(el: &Element) -> bool {
    if dom::closest(el, "#settings").is_none() {
        return false;
    }
    let mut s = get();
    let value = || {
        el.dyn_ref::<web_sys::HtmlInputElement>()
            .map(|i| i.value())
            .or_else(|| {
                el.dyn_ref::<web_sys::HtmlSelectElement>()
                    .map(|i| i.value())
            })
            .unwrap_or_default()
    };
    let checked = || {
        el.dyn_ref::<web_sys::HtmlInputElement>()
            .is_some_and(|i| i.checked())
    };
    match el.id().as_str() {
        "set-ws" => s.ignore_ws = checked(),
        "set-rf" => s.reformat = checked(),
        "set-ln" => s.line_numbers = checked(),
        "set-size" => {
            if let Ok(n) = value().parse() {
                s.size = n;
            }
        }
        "set-font" => {
            let v = value();
            if v.is_empty() {
                // "Other…": show the name field, keep the font until typed.
                if let Some(f) = dom::closest(el, "#settings")
                    .and_then(|p| p.query_selector("#set-font-custom").ok().flatten())
                    .and_then(|i| i.parent_element())
                {
                    dom::toggle_class(&f, "hidden", false);
                }
                return false;
            }
            s.font = v;
        }
        "set-font-custom" => {
            let f = clean_font(&value());
            if f.is_empty() {
                return false;
            }
            s.font = f;
        }
        _ if el.get_attribute("name").as_deref() == Some("set-theme") => {
            s.theme = Theme::from_name(&value()).unwrap_or_default();
        }
        _ if el.get_attribute("name").as_deref() == Some("set-layout") => {
            s.split = value() == "split";
        }
        _ => return false,
    }
    let old = get();
    let layout_changed =
        s.split != old.split || s.ignore_ws != old.ignore_ws || s.reformat != old.reformat;
    set(s);
    save();
    apply();
    // Font and line number changes need no new HTML (a class on the root
    // element shows or hides the numbers), only new sizes of the cards. A
    // theme change needs neither: the colors are CSS variables.
    layout_changed
        || el.id().starts_with("set-font")
        || el.id() == "set-size"
        || el.id() == "set-ln"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        let s = Settings {
            theme: Theme::Dark,
            split: true,
            ignore_ws: true,
            reformat: false,
            line_numbers: false,
            font: "Fira Code".into(),
            size: 15,
        };
        assert_eq!(Settings::decode(&s.encode()), s);
        assert_eq!(Settings::decode("garbage"), Settings::default());
        // Settings saved before line numbers existed keep them on.
        assert!(Settings::decode("split=1;ws=0;size=14").line_numbers);
        assert_eq!(Settings::decode("size=99").size, 13);
        // Settings saved before the theme existed follow the system.
        assert_eq!(Settings::decode("split=1").theme, Theme::System);
        assert_eq!(Settings::decode("theme=sepia").theme, Theme::System);
        // A font name can't break out of the CSS string.
        assert_eq!(clean_font("a\"; } body { x"), "a  body  x");
    }
}
