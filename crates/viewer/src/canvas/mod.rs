//! The canvas: item cards arranged freely, with connectors, tabs and a
//! module scope that follows Rust's privacy rules.

use crate::code::esc;
use crate::data::Data;
use crate::{App, dom};
use std::collections::{BTreeSet, HashMap};
use web_sys::Element;

mod card;
mod edges;
mod events;
mod layout;
mod open;

pub use edges::draw_edges;
pub use events::{
    active_item_of_card, highlight_card_of, on_click, on_details_toggle, on_pointer_down,
    on_pointer_move, on_pointer_up, on_wheel,
};
pub use layout::show_module_graph;
pub use open::open;

use card::card_html;

const CARD_W: f64 = 580.0;
/// At most this many cards are opened by one "calls"/"callers" click.
const EXPAND_LIMIT: usize = 10;

pub struct Group {
    pub gid: u32,
    pub tabs: Vec<String>,
    pub active: String,
    pub x: f64,
    pub y: f64,
    /// Items whose source view is expanded.
    pub open_src: BTreeSet<String>,
}

#[derive(Clone, Copy)]
pub struct Pan {
    pub x: f64,
    pub y: f64,
    pub k: f64,
}

pub enum Drag {
    Card {
        gid: u32,
        sx: f64,
        sy: f64,
        gx: f64,
        gy: f64,
    },
    Pan {
        sx: f64,
        sy: f64,
        px: f64,
        py: f64,
    },
}

pub struct Canvas {
    /// The module the canvas looks from.
    pub scope: String,
    pub groups: Vec<Group>,
    /// Connectors recording where a card was opened from.
    pub links: Vec<(String, String)>,
    next_gid: u32,
    pub pan: Pan,
    pub frames: bool,
    pub drag: Option<Drag>,
}

impl Canvas {
    pub fn new(scope: String) -> Canvas {
        Canvas {
            scope,
            groups: Vec::new(),
            links: Vec::new(),
            next_gid: 1,
            pan: Pan {
                x: 60.0,
                y: 30.0,
                k: 1.0,
            },
            frames: false,
            drag: None,
        }
    }

    fn group_of(&self, id: &str) -> Option<usize> {
        self.groups
            .iter()
            .position(|g| g.tabs.iter().any(|t| t == id))
    }

    fn by_gid(&self, gid: u32) -> Option<usize> {
        self.groups.iter().position(|g| g.gid == gid)
    }

    fn add_group(&mut self, id: &str, x: f64, y: f64) -> usize {
        self.groups.push(Group {
            gid: self.next_gid,
            tabs: vec![id.to_string()],
            active: id.to_string(),
            x,
            y,
            open_src: BTreeSet::new(),
        });
        self.next_gid += 1;
        self.groups.len() - 1
    }

    fn add_link(&mut self, from: &str, to: &str) {
        if from != to
            && !self
                .links
                .iter()
                .any(|(a, b)| (a == from && b == to) || (a == to && b == from))
        {
            self.links.push((from.to_string(), to.to_string()));
        }
    }
}

fn visible(d: &Data, c: &Canvas, g: &Group) -> bool {
    d.visible_from(&g.active, &c.scope)
}

fn card_el(gid: u32) -> Option<Element> {
    dom::query(&format!(r#".card[data-gid="{gid}"]"#))
}

fn height_of(d: &Data, g: &Group) -> f64 {
    if let Some(el) = card_el(g.gid) {
        let (_, h) = dom::offset_size(&el);
        if h > 0.0 {
            return h;
        }
    }
    let it = d.item(&g.active);
    120.0
        + it.map_or(0.0, |it| it.info.sig.lines().count() as f64 * 18.0)
        + if it.is_some_and(|it| !it.info.docs.is_empty()) {
            40.0
        } else {
            0.0
        }
}

/// Moves a card down until it no longer overlaps another visible card.
fn free_spot(d: &Data, c: &Canvas, gi: usize, x: f64, mut y: f64) -> (f64, f64) {
    let h = height_of(d, &c.groups[gi]);
    for _ in 0..80 {
        let hit = c.groups.iter().enumerate().find(|(i, o)| {
            *i != gi && visible(d, c, o) && {
                let oh = height_of(d, o);
                x < o.x + CARD_W + 20.0
                    && x + CARD_W + 20.0 > o.x
                    && y < o.y + oh + 20.0
                    && y + h + 20.0 > o.y
            }
        });
        match hit {
            None => break,
            Some((_, o)) => y = o.y + height_of(d, o) + 30.0,
        }
    }
    (x, y)
}

fn place(d: &Data, c: &mut Canvas, gi: usize) {
    let (x, y) = free_spot(d, c, gi, c.groups[gi].x, c.groups[gi].y);
    c.groups[gi].x = x;
    c.groups[gi].y = y;
}

pub fn render(app: &mut App) {
    let d = app.d.clone();
    let c = &app.s.canvas;
    let chain = d.mod_chain(&c.scope);
    let hidden = c.groups.iter().filter(|g| !visible(&d, c, g)).count();
    let bar = format!(
        r#"<span>Viewing from</span><span class="scope">{}</span>{}
        <span class="sep"></span>
        <button id="cv-arrange" title="Lay cards out inside their modules">Arrange by module</button>
        <button id="cv-frames" class="{}" title="Draw module boundaries">Module frames</button>
        <button id="cv-fit">Fit</button>
        <button id="cv-clear">Clear</button>"#,
        chain
            .iter()
            .map(|m| format!(
                r#"<a data-scope="{}" title="Look from {}">{}</a>"#,
                esc(m),
                esc(&d.mod_label(m)),
                esc(d.item(m).map_or("", |i| &i.info.name))
            ))
            .collect::<Vec<_>>()
            .join("::"),
        if hidden > 0 {
            format!(
                r#"<span class="chip" title="Private to submodules of the current scope">{hidden} card{} hidden at this scope</span>"#,
                if hidden > 1 { "s" } else { "" }
            )
        } else {
            String::new()
        },
        if c.frames { "on" } else { "" },
    );
    dom::set_html("canvas-bar", &bar);
    let cards: String = c
        .groups
        .iter()
        .filter(|g| visible(&d, c, g))
        .map(|g| card_html(&d, c, g))
        .collect();
    dom::set_html("cards", &cards);
    let any = c.groups.iter().any(|g| visible(&d, c, g));
    dom::set_html(
        "canvas-empty",
        if any {
            ""
        } else {
            "<div>Nothing on the canvas yet.<br>Open items from the <b>Overview</b> or a <b>module page</b>, or use <b>Find item</b>.<br>Click any name in a card to place its item next to it.</div>"
        },
    );
    apply_pan(c);
    mark_on_canvas(app);
    dom::raf(|| {
        crate::with_app(|app| {
            draw_edges(app);
            mark_on_canvas(app);
        });
    });
}

pub fn apply_pan(c: &Canvas) {
    if let Some(world) = dom::by_id("world") {
        dom::set_style(
            &world,
            "transform",
            &format!("translate({}px, {}px) scale({})", c.pan.x, c.pan.y, c.pan.k),
        );
    }
}

/// Highlights names that refer to items already on the canvas.
fn mark_on_canvas(app: &App) {
    let d = &app.d;
    let c = &app.s.canvas;
    let on: BTreeSet<&str> = c
        .groups
        .iter()
        .filter(|g| visible(d, c, g))
        .map(|g| g.active.as_str())
        .collect();
    let actives: HashMap<u32, &str> = c
        .groups
        .iter()
        .map(|g| (g.gid, g.active.as_str()))
        .collect();
    for a in dom::query_all("#cards a.ref[data-id]") {
        let id = dom::data(&a, "id").unwrap_or_default();
        let own = dom::closest(&a, ".card")
            .and_then(|card| dom::data(&card, "gid"))
            .and_then(|g| g.parse::<u32>().ok())
            .and_then(|g| actives.get(&g).copied());
        dom::toggle_class(
            &a,
            "on-canvas",
            on.contains(id.as_str()) && own != Some(id.as_str()),
        );
    }
}
