//! The canvas: item cards arranged freely, with connectors, tabs and a
//! module scope that follows Rust's privacy rules.

use crate::code::esc;
use crate::data::Data;
use crate::{App, dom};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use web_sys::Element;

mod card;
mod edges;
mod events;
mod layout;
mod open;
mod reflow;
mod spot;
mod undo;
mod zoom;

pub use edges::draw_edges;
pub use events::{
    active_item_of_card, highlight_card_of, on_click, on_dblclick, on_details_toggle,
    on_pointer_down, on_pointer_move, on_pointer_up, on_unfold, on_wheel, swallow_click,
};
pub use layout::{show_impact, show_items, show_module_graph};
pub use open::{open, open_for_review};
pub use undo::{Saved, after_change, before_change, restore, save, undo};

use card::card_html;

/// The width of a card whose code fits in it, and the least width a card
/// gets by default.
const MIN_W: f64 = 580.0;
/// How wide a card grows to show its code without scrolling it sideways:
/// 100 columns of code (rustfmt's default) with line numbers fit easily
/// in the default font; more for a side-by-side diff.
const MAX_W: f64 = 1000.0;
const MAX_SPLIT_W: f64 = 1600.0;
/// The range of widths a card can be resized to by hand.
const RESIZE_MIN: f64 = 320.0;
const RESIZE_MAX: f64 = 2400.0;
/// At most this many cards are opened by one "calls"/"callers" click.
const EXPAND_LIMIT: usize = 10;

#[derive(Clone)]
pub struct Group {
    pub gid: u32,
    pub tabs: Vec<String>,
    pub active: String,
    pub x: f64,
    pub y: f64,
    /// Items whose source view the reader opened (`true`) or closed.
    pub open_src: BTreeMap<String, bool>,
    /// Folds of unchanged source lines that were expanded, by item and
    /// first row.
    pub unfolded: BTreeMap<String, BTreeSet<usize>>,
    /// Unchanged items whose card the reader expanded beyond the signature.
    pub expanded: BTreeSet<String>,
    /// The width the reader gave the card by dragging its right edge; by
    /// default (`None`) the card is as wide as its code, see
    /// [`fit_width`].
    pub w: Option<f64>,
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
    /// The cards of a module and its submodules, moved by the module's frame.
    Frame {
        sx: f64,
        sy: f64,
        cards: Vec<(u32, f64, f64)>,
    },
    Pan {
        sx: f64,
        sy: f64,
        px: f64,
        py: f64,
    },
    /// A card's right edge, dragged from `sx` when the card was `w0` wide.
    Resize {
        gid: u32,
        sx: f64,
        w0: f64,
    },
    /// Two fingers on a touch screen: their distance zooms, their midpoint
    /// pans. `d0` and `mx`, `my` are the distance and the midpoint (in
    /// viewport coordinates) when the pinch started, with the view at `pan`.
    Pinch {
        d0: f64,
        mx: f64,
        my: f64,
        pan: Pan,
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
    /// The touch points on the canvas: pointer id and client position.
    touches: Vec<(i32, f64, f64)>,
    /// The last rendered size (width, height) of each card, to tell how
    /// much it changed.
    sizes: HashMap<u32, (f64, f64)>,
    /// What each "calls", "callers" and "used by" click opened, oldest
    /// first, so that a second click can close it again.
    expansions: Vec<Expansion>,
    /// Earlier layouts, oldest first, for Undo.
    history: Vec<undo::Snapshot>,
    /// The layout before the current click, key press or drag.
    before: Option<undo::Snapshot>,
    /// A finger resting on a card's header (or a module frame) that lifts
    /// the card when it stays put long enough.
    hold: Option<events::Hold>,
    /// Whether the last touch moved the view, so that lifting the finger
    /// doesn't also click what it started on.
    panned: bool,
    /// For a touch that started on code wider than its card: the code's
    /// box, its scroll position then, and once the finger moved, whether
    /// it moves sideways (scrolling the code) or not (panning the view).
    sideways: Option<(Element, f64, Option<bool>)>,
}

/// The cards and connectors one expansion of an item added.
#[derive(Clone)]
struct Expansion {
    item: String,
    kind: open::Related,
    /// The cards it opened (by group), with the item each one shows.
    cards: Vec<(u32, String)>,
    /// Connectors it added between cards that were already open.
    links: Vec<(String, String)>,
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
            touches: Vec::new(),
            sizes: HashMap::new(),
            expansions: Vec::new(),
            history: Vec::new(),
            before: None,
            hold: None,
            panned: false,
            sideways: None,
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
            open_src: BTreeMap::new(),
            unfolded: BTreeMap::new(),
            expanded: BTreeSet::new(),
            w: None,
        });
        self.next_gid += 1;
        self.groups.len() - 1
    }

    /// Adds a connector; `false` if there already was one.
    fn add_link(&mut self, from: &str, to: &str) -> bool {
        if from != to
            && !self
                .links
                .iter()
                .any(|(a, b)| (a == from && b == to) || (a == to && b == from))
        {
            self.links.push((from.to_string(), to.to_string()));
            return true;
        }
        false
    }

    /// Whether anything an expansion added is still on the canvas.
    fn expansion_live(&self, e: &Expansion) -> bool {
        e.cards.iter().any(|(gid, id)| {
            self.by_gid(*gid)
                .is_some_and(|gi| self.groups[gi].tabs.contains(id))
        }) || e.links.iter().any(|l| self.links.contains(l))
    }

    /// The live expansion of `item` by `kind`, if any; forgets ones whose
    /// cards and connectors were all closed by hand.
    fn live_expansion(&mut self, item: &str, kind: open::Related) -> Option<usize> {
        let e: Vec<Expansion> = std::mem::take(&mut self.expansions);
        self.expansions = e.into_iter().filter(|e| self.expansion_live(e)).collect();
        self.expansions
            .iter()
            .rposition(|e| e.item == item && e.kind == kind)
    }

    /// Whether `item`'s `kind` button would close what it opened.
    fn is_expanded(&self, item: &str, kind: open::Related) -> bool {
        self.expansions
            .iter()
            .any(|e| e.item == item && e.kind == kind && self.expansion_live(e))
    }

    /// Closes what an expansion opened; returns how many cards.
    fn collapse(&mut self, i: usize) -> usize {
        let e = self.expansions.remove(i);
        let mut closed = 0;
        for (gid, id) in &e.cards {
            let Some(gi) = self.by_gid(*gid) else {
                continue;
            };
            if !self.groups[gi].tabs.contains(id) {
                continue;
            }
            closed += 1;
            self.groups[gi].tabs.retain(|t| t != id);
            self.links.retain(|(a, b)| a != id && b != id);
            if self.groups[gi].tabs.is_empty() {
                self.groups.remove(gi);
            } else if self.groups[gi].active == *id {
                self.groups[gi].active = self.groups[gi].tabs[0].clone();
            }
        }
        self.links.retain(|l| !e.links.contains(l));
        closed
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

/// The width of a card: as rendered, else as set by the reader, else the
/// least default width.
fn width_of(g: &Group) -> f64 {
    if let Some(el) = card_el(g.gid) {
        let (w, _) = dom::offset_size(&el);
        if w > 0.0 {
            return w;
        }
    }
    g.w.unwrap_or(MIN_W)
}

/// Makes a rendered card as wide as the widest code it shows (signature,
/// source, code in docs), between [`MIN_W`] and [`MAX_W`], unless the
/// reader resized it. Called after each render and when a card shows more
/// code: an opened source view, unfolded lines.
///
/// Measured rather than computed from the text, since the code's font,
/// line numbers and the badges after names all take room.
fn fit_widths(cards: &[(Element, Option<f64>)]) {
    // All cards at their least width first, then all measured, so that the
    // page is laid out once.
    for (el, w) in cards {
        let w = w.map_or(MIN_W, |w| w.clamp(RESIZE_MIN, RESIZE_MAX));
        dom::set_style(el, "width", &format!("{w}px"));
    }
    let fits: Vec<Option<f64>> = cards
        .iter()
        .map(|(el, w)| {
            if w.is_some() {
                return None;
            }
            let more = dom::query_all_in(el, "pre, .sig, .sbs")
                .iter()
                .map(|b| b.scroll_width() - b.client_width())
                .max()
                .unwrap_or(0);
            let max = if dom::query_all_in(el, ".sbs")
                .iter()
                .any(|b| b.client_width() > 0)
            {
                MAX_SPLIT_W
            } else {
                MAX_W
            };
            (more > 0).then(|| (MIN_W + more as f64).min(max))
        })
        .collect();
    for ((el, _), w) in cards.iter().zip(fits) {
        if let Some(w) = w {
            dom::set_style(el, "width", &format!("{w}px"));
        }
    }
}

/// [`fit_widths`] for all rendered cards. Also called when a font has
/// loaded, since the code's font may arrive after the cards were rendered.
pub fn refit(c: &Canvas) {
    let rendered: Vec<(Element, Option<f64>)> = c
        .groups
        .iter()
        .filter_map(|g| Some((card_el(g.gid)?, g.w)))
        .collect();
    fit_widths(&rendered);
}

/// [`fit_widths`] for one card, after it shows more code.
fn fit_width(c: &Canvas, card: &Element) {
    let w = dom::data(card, "gid")
        .and_then(|g| g.parse::<u32>().ok())
        .and_then(|g| c.by_gid(g))
        .and_then(|gi| c.groups[gi].w);
    fit_widths(&[(card.clone(), w)]);
}

/// Moves a card down until it no longer overlaps another visible card.
fn free_spot(d: &Data, c: &Canvas, gi: usize, x: f64, mut y: f64) -> (f64, f64) {
    let h = height_of(d, &c.groups[gi]);
    let w = width_of(&c.groups[gi]);
    for _ in 0..80 {
        let hit = c.groups.iter().enumerate().find(|(i, o)| {
            *i != gi && visible(d, c, o) && {
                let oh = height_of(d, o);
                x < o.x + width_of(o) + 20.0
                    && x + w + 20.0 > o.x
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

/// The items of the cards that the canvas shows, with the view it looks
/// from: what a new tab lays out again (see `nav::update_view_links`).
pub fn shown_items(app: &App) -> Vec<String> {
    let c = &app.s.canvas;
    c.groups
        .iter()
        .filter(|g| visible(&app.d, c, g))
        .map(|g| g.active.clone())
        .collect()
}

/// The card of item `id` as the canvas would show it, looking from
/// `scope`, for a view outside the canvas (see [`crate::calls`]). `full`
/// shows an unchanged item's docs and source, not only its signature.
pub fn card_html_for(d: &Data, scope: &str, id: &str, full: bool) -> String {
    let mut c = Canvas::new(scope.to_string());
    let gi = c.add_group(id, 0.0, 0.0);
    if full {
        c.groups[gi].expanded.insert(id.to_string());
    }
    card_html(d, &c, &c.groups[gi])
}

pub fn render(app: &mut App) {
    let d = app.d.clone();
    let c = &app.s.canvas;
    let chain = d.mod_chain(&c.scope);
    let hidden = c.groups.iter().filter(|g| !visible(&d, c, g)).count();
    let bar = format!(
        r#"<button id="cv-undo" title="Undo the last change to the cards (Ctrl+Z)"{undo}>↶ Undo</button><span>Viewing from</span><span class="scope">{}</span>{}
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
                r#"<span class="chip" title="Not visible from this scope">{hidden} card{} hidden at this scope</span>"#,
                if hidden > 1 { "s" } else { "" }
            )
        } else {
            String::new()
        },
        if c.frames { "on" } else { "" },
        undo = if c.history.is_empty() {
            " disabled"
        } else {
            ""
        },
    );
    dom::set_html("canvas-bar", &bar);
    let cards: String = c
        .groups
        .iter()
        .filter(|g| visible(&d, c, g))
        .map(|g| card_html(&d, c, g))
        .collect();
    dom::set_html("cards", &cards);
    refit(c);
    reflow::observe_cards();
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
    // The sidebar's outline marks which items have a card.
    if app.s.view == crate::View::Canvas {
        crate::pages::render_sidebar(app);
    }
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

/// The items that have a card (as any of its tabs), also those hidden at
/// the canvas's scope.
pub fn items_on_canvas(app: &App) -> Vec<String> {
    app.s
        .canvas
        .groups
        .iter()
        .flat_map(|g| g.tabs.iter().cloned())
        .collect()
}
