//! Clicks, drags, wheel and toggle events on the canvas.

use super::{
    CARD_W, Drag, apply_pan, card_el,
    edges::draw_edges,
    layout::{arrange_by_module, fit_view},
    open::{Related, close_tab, expand, members_as_tabs},
    place, render, visible, zoom,
};
use crate::data::within;
use crate::{App, View, dom, toast};
use web_sys::{Element, PointerEvent, WheelEvent};

/// How long a finger rests on a card's header before the card follows it.
const HOLD_MS: i32 = 450;
/// How far a finger may move, in pixels, and still count as resting (or,
/// when lifted, as a tap rather than a pan).
pub(super) const SLOP: f64 = 10.0;

/// A finger resting on what a mouse would drag right away: a card's
/// header or a module frame. On a touch screen a drag there pans the view
/// like anywhere else, so that moving the view doesn't move cards by
/// accident; a card or frame follows the finger only after it rested for
/// [`HOLD_MS`].
pub struct Hold {
    pointer: i32,
    what: Lift,
    sx: f64,
    sy: f64,
}

#[derive(Clone)]
enum Lift {
    Card(u32),
    Frame(String),
}

/// Handles a click inside the canvas. Returns whether it was consumed.
pub fn on_click(app: &mut App, el: &Element) -> bool {
    let gid = dom::closest(el, ".card")
        .and_then(|c| dom::data(&c, "gid"))
        .and_then(|g| g.parse::<u32>().ok());
    if let Some(x) = dom::closest(el, "[data-close]")
        && let Some(gi) = gid.and_then(|g| app.s.canvas.by_gid(g))
    {
        close_tab(app, gi, &dom::data(&x, "close").unwrap_or_default());
        render(app);
        return true;
    }
    if let Some(t) = dom::closest(el, "[data-tab]")
        && let Some(gi) = gid.and_then(|g| app.s.canvas.by_gid(g))
    {
        app.s.canvas.groups[gi].active = dom::data(&t, "tab").unwrap_or_default();
        render(app);
        return true;
    }
    if let Some(b) = dom::closest(el, "[data-act]")
        && let Some(gi) = gid.and_then(|g| app.s.canvas.by_gid(g))
    {
        let id = app.s.canvas.groups[gi].active.clone();
        match dom::data(&b, "act").as_deref() {
            Some("callees") => expand(app, &id, Related::Callees),
            Some("callers") => expand(app, &id, Related::Callers),
            Some("uses") => expand(app, &id, Related::UsedBy),
            Some("tabs") => members_as_tabs(app, &id),
            Some("expand") => {
                app.s.canvas.groups[gi].expanded.insert(id);
                render(app);
            }
            Some("fold") => {
                app.s.canvas.groups[gi].expanded.remove(&id);
                render(app);
            }
            Some("drill") => {
                app.s.canvas.scope = id;
                render(app);
            }
            Some("page") => {
                app.s.module = id;
                crate::set_view(app, View::Module);
            }
            Some("detach") => {
                let d = app.d.clone();
                let c = &mut app.s.canvas;
                let g = &mut c.groups[gi];
                g.tabs.retain(|t| *t != id);
                g.active = g.tabs[0].clone();
                let (x, y) = (g.x + CARD_W + 60.0, g.y);
                let ni = c.add_group(&id, x, y);
                place(&d, c, ni);
                render(app);
            }
            _ => return false,
        }
        return true;
    }
    if let Some(a) = dom::closest(el, "[data-drill]") {
        let m = dom::data(&a, "drill").unwrap_or_default();
        toast(&format!("Now viewing from {}", app.d.mod_label(&m)));
        app.s.canvas.scope = m;
        render(app);
        return true;
    }
    if let Some(a) = dom::closest(el, "[data-scope]") {
        app.s.canvas.scope = dom::data(&a, "scope").unwrap_or_default();
        render(app);
        return true;
    }
    match el
        .closest("button")
        .ok()
        .flatten()
        .map(|b| b.id())
        .as_deref()
    {
        Some("cv-arrange") => {
            arrange_by_module(app);
            render(app);
            fit_view();
        }
        Some("cv-frames") => {
            app.s.canvas.frames = !app.s.canvas.frames;
            render(app);
        }
        Some("cv-fit") => fit_view(),
        Some("cv-clear") => {
            app.s.canvas.groups.clear();
            app.s.canvas.links.clear();
            app.s.canvas.expansions.clear();
            render(app);
            toast("Cleared the canvas (Undo brings the cards back)");
        }
        Some("cv-undo") => super::undo(app),
        _ => return false,
    }
    true
}

/// Whether a click ends a touch that moved the view (or a card) rather
/// than tapped: then it shouldn't also open or press what it started on.
pub fn swallow_click(app: &mut App) -> bool {
    std::mem::take(&mut app.s.canvas.panned)
}

/// Starts dragging a card (by its header or tabs) or panning the canvas.
pub fn on_pointer_down(app: &mut App, el: &Element, ev: &PointerEvent) -> bool {
    if dom::closest(el, "#viewport").is_none() {
        return false;
    }
    zoom::on_press(el, ev);
    let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
    let d = app.d.clone();
    let c = &mut app.s.canvas;
    if ev.pointer_type() != "touch" || c.touches.is_empty() {
        c.panned = false;
    }
    if ev.pointer_type() == "touch" {
        c.touches.retain(|t| t.0 != ev.pointer_id());
        c.touches.push((ev.pointer_id(), x, y));
        if c.touches.len() > 1 {
            zoom::cancel_press();
            c.panned = true;
            c.hold = None;
            // A second finger turns panning into a pinch.
            if matches!(c.drag, None | Some(Drag::Pan { .. }))
                && let Some((d0, mx, my)) = pinch_geometry(&c.touches)
            {
                ev.prevent_default();
                c.drag = Some(Drag::Pinch {
                    d0,
                    mx,
                    my,
                    pan: c.pan,
                });
            }
            return true;
        }
    }
    let lift = if let Some(frame) = dom::closest(el, "[data-frame]") {
        Some(Lift::Frame(dom::data(&frame, "frame").unwrap_or_default()))
    } else if let Some(card) = dom::closest(el, ".card") {
        // Cards are moved by their header, tabs, location line, footer, or
        // border; not by code or text, which can be selected.
        let handle = el == &card
            || dom::closest(el, ".card-h, .card-tabs, .card-loc").is_some()
            || el
                .matches(".card-foot, .card-acts, .card-rv")
                .unwrap_or(false);
        let control = dom::closest(el, "[data-close], button, a, input, textarea").is_some();
        let gid = dom::data(&card, "gid").and_then(|g| g.parse::<u32>().ok());
        match gid {
            Some(gid) if handle && !control => Some(Lift::Card(gid)),
            // On a touch screen, a finger on the rest of a card (its code,
            // a button) pans the view; with a mouse, that selects text or
            // clicks.
            _ if ev.pointer_type() == "touch" => None,
            _ => return false,
        }
    } else {
        None
    };
    if ev.pointer_type() == "touch" {
        start_pan(c, x, y);
        c.sideways = sideways_box(el).map(|b| {
            let left = b.scroll_left() as f64;
            (b, left, None)
        });
        if let Some(what) = lift {
            c.hold = Some(Hold {
                pointer: ev.pointer_id(),
                what,
                sx: x,
                sy: y,
            });
            let pointer = ev.pointer_id();
            dom::timeout(HOLD_MS, move || {
                crate::with_app(|app| lift_held(app, pointer));
            });
        }
        return true;
    }
    match lift {
        Some(what) => {
            ev.prevent_default();
            start_lift(c, &d, what, x, y);
        }
        None => start_pan(c, x, y),
    }
    true
}

/// The box around `el` inside its card that is wider than it shows (code,
/// a side-by-side diff, tabs), which a sideways swipe scrolls.
fn sideways_box(el: &Element) -> Option<Element> {
    let mut cur = Some(el.clone());
    while let Some(e) = cur {
        if e.class_list().contains("card") {
            return None;
        }
        if e.scroll_width() > e.client_width()
            && e.matches("pre, .sig, .sbs, .card-tabs").unwrap_or(false)
        {
            return Some(e);
        }
        cur = e.parent_element();
    }
    None
}

fn start_pan(c: &mut super::Canvas, x: f64, y: f64) {
    c.hold = None;
    c.sideways = None;
    c.drag = Some(Drag::Pan {
        sx: x,
        sy: y,
        px: c.pan.x,
        py: c.pan.y,
    });
    if let Some(vp) = dom::by_id("viewport") {
        dom::toggle_class(&vp, "panning", true);
    }
}

/// Starts moving a card or a module frame's cards from pointer position
/// `x`, `y`.
fn start_lift(c: &mut super::Canvas, d: &crate::data::Data, what: Lift, x: f64, y: f64) {
    match what {
        Lift::Frame(m) => {
            let cards = c
                .groups
                .iter()
                .filter(|g| {
                    visible(d, c, g) && d.item(&g.active).is_some_and(|it| within(d.mod_of(it), &m))
                })
                .map(|g| (g.gid, g.x, g.y))
                .collect();
            c.drag = Some(Drag::Frame {
                sx: x,
                sy: y,
                cards,
            });
        }
        Lift::Card(gid) => {
            let Some(gi) = c.by_gid(gid) else { return };
            let g = &c.groups[gi];
            c.drag = Some(Drag::Card {
                gid,
                sx: x,
                sy: y,
                gx: g.x,
                gy: g.y,
            });
            if let Some(card) = card_el(gid) {
                dom::set_style(&card, "z-index", "10");
                dom::set_style(&card, "pointer-events", "none");
            }
        }
    }
}

thread_local! {
    static HINTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Says once how to move a card on a touch screen, the first time a
/// swipe on a card's header pans the view instead.
fn hint_hold() {
    if !HINTED.with(|h| h.replace(true)) {
        toast("To move a card, hold its header for a moment, then drag");
    }
}

/// After a finger rested on a card's header or a frame for [`HOLD_MS`]:
/// from now on the card or frame follows the finger instead of the view.
fn lift_held(app: &mut App, pointer: i32) {
    let d = app.d.clone();
    let c = &mut app.s.canvas;
    let Some(hold) = c.hold.take_if(|h| h.pointer == pointer) else {
        return;
    };
    if !matches!(c.drag, Some(Drag::Pan { .. })) || c.touches.len() != 1 {
        return;
    }
    // The view moved by at most the slop; put it back where it was.
    if let Some(Drag::Pan { px, py, .. }) = c.drag {
        c.pan.x = px;
        c.pan.y = py;
        apply_pan(c);
    }
    if let Some(vp) = dom::by_id("viewport") {
        dom::toggle_class(&vp, "panning", false);
        dom::toggle_class(&vp, "lifting", true);
    }
    if let Lift::Card(gid) = hold.what
        && let Some(card) = card_el(gid)
    {
        dom::toggle_class(&card, "lifted", true);
    }
    start_lift(c, &d, hold.what, hold.sx, hold.sy);
    // A short buzz where supported, like a long press elsewhere.
    let _ = dom::window().navigator().vibrate_with_duration(15);
}

/// The card whose header is under the pointer, if the pointer is close
/// enough to the top to mean "drop here to merge into tabs".
fn drop_target(x: f64, y: f64, dragged: u32) -> Option<Element> {
    let under = dom::document().element_from_point(x as f32, y as f32)?;
    let card = dom::closest(&under, ".card")?;
    let gid = dom::data(&card, "gid")?.parse::<u32>().ok()?;
    (gid != dragged && y - card.get_bounding_client_rect().top() < 60.0).then_some(card)
}

/// The distance between the first two touch points, and their midpoint in
/// viewport coordinates.
fn pinch_geometry(touches: &[(i32, f64, f64)]) -> Option<(f64, f64, f64)> {
    let &[(_, x1, y1), (_, x2, y2), ..] = touches else {
        return None;
    };
    let r = dom::by_id("viewport")?.get_bounding_client_rect();
    let d = (x2 - x1).hypot(y2 - y1).max(1.0);
    Some((d, (x1 + x2) / 2.0 - r.left(), (y1 + y2) / 2.0 - r.top()))
}

pub fn on_pointer_move(app: &mut App, ev: &PointerEvent) {
    let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
    let c = &mut app.s.canvas;
    if let Some(t) = c.touches.iter_mut().find(|t| t.0 == ev.pointer_id()) {
        (t.1, t.2) = (x, y);
    }
    match c.drag {
        Some(Drag::Card {
            gid,
            sx,
            sy,
            gx,
            gy,
        }) => {
            let Some(gi) = c.by_gid(gid) else { return };
            c.panned = true;
            let g = &mut c.groups[gi];
            g.x = gx + (x - sx) / c.pan.k;
            g.y = gy + (y - sy) / c.pan.k;
            if let Some(el) = card_el(gid) {
                dom::set_style(&el, "left", &format!("{}px", g.x));
                dom::set_style(&el, "top", &format!("{}px", g.y));
            }
            draw_edges(app);
            for el in dom::query_all(".card.hover-target") {
                dom::toggle_class(&el, "hover-target", false);
            }
            if let Some(target) = drop_target(x, y, gid) {
                dom::toggle_class(&target, "hover-target", true);
            }
        }
        Some(Drag::Frame { sx, sy, ref cards }) => {
            let (dx, dy) = ((x - sx) / c.pan.k, (y - sy) / c.pan.k);
            for &(gid, gx, gy) in cards {
                let Some(gi) = c.by_gid(gid) else { continue };
                let g = &mut c.groups[gi];
                g.x = gx + dx;
                g.y = gy + dy;
                if let Some(el) = card_el(gid) {
                    dom::set_style(&el, "left", &format!("{}px", g.x));
                    dom::set_style(&el, "top", &format!("{}px", g.y));
                }
            }
            draw_edges(app);
        }
        Some(Drag::Pan { sx, sy, px, py }) => {
            if (x - sx).hypot(y - sy) > SLOP {
                if c.hold.take().is_some() {
                    hint_hold();
                }
                c.panned = true;
            }
            // A finger on code that is wider than its card: a mostly
            // sideways swipe scrolls the code, anything else the view.
            // (The browser can't do it: it would take over the touch from
            // the canvas.)
            if let Some((b, left, mode)) = &mut c.sideways {
                if mode.is_none() && (x - sx).hypot(y - sy) > SLOP {
                    *mode = Some((x - sx).abs() > (y - sy).abs());
                }
                if *mode == Some(true) {
                    b.set_scroll_left((*left - (x - sx)) as i32);
                    return;
                }
            }
            c.pan.x = px + x - sx;
            c.pan.y = py + y - sy;
            apply_pan(c);
        }
        Some(Drag::Pinch { d0, mx, my, pan }) => {
            let Some((d, nx, ny)) = pinch_geometry(&c.touches) else {
                return;
            };
            // The point that was under the fingers' midpoint stays under it.
            let k = (pan.k * d / d0).clamp(0.2, 2.0);
            c.pan.x = nx - (mx - pan.x) * k / pan.k;
            c.pan.y = ny - (my - pan.y) * k / pan.k;
            c.pan.k = k;
            apply_pan(c);
        }
        None => {}
    }
}

pub fn on_pointer_up(app: &mut App, ev: &PointerEvent) {
    let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
    app.s.canvas.touches.retain(|t| t.0 != ev.pointer_id());
    app.s.canvas.hold = None;
    let double_click = zoom::on_release(ev);
    let Some(drag) = app.s.canvas.drag.take() else {
        return;
    };
    if let Some(vp) = dom::by_id("viewport") {
        dom::toggle_class(&vp, "panning", false);
        dom::toggle_class(&vp, "lifting", false);
    }
    for el in dom::query_all(".card.lifted") {
        dom::toggle_class(&el, "lifted", false);
    }
    for el in dom::query_all(".card.hover-target") {
        dom::toggle_class(&el, "hover-target", false);
    }
    if let Drag::Card { gid, .. } = drag {
        // Look for a drop target while the dragged card ignores the pointer.
        let target = drop_target(x, y, gid)
            .and_then(|t| dom::data(&t, "gid"))
            .and_then(|g| g.parse::<u32>().ok());
        let c = &mut app.s.canvas;
        if let (Some(tgid), Some(si)) = (target, c.by_gid(gid)) {
            let source = c.groups.remove(si);
            if let Some(ti) = c.by_gid(tgid) {
                let t = &mut c.groups[ti];
                for tab in source.tabs {
                    if !t.tabs.contains(&tab) {
                        t.tabs.push(tab);
                    }
                }
                t.active = source.active;
                toast("Merged into tabs");
            }
        }
        render(app);
    }
    if let Some(gid) = double_click {
        zoom::zoom_to(app, gid);
    }
}

/// Wheel input as in other canvas and map tools: scrolling moves the view
/// vertically, Shift+scrolling horizontally, and Ctrl+scrolling (or a
/// pinch on a touchpad, which arrives as such) zooms around the pointer.
/// A sideways touchpad swipe moves the view horizontally.
pub fn on_wheel(app: &mut App, el: &Element, ev: &WheelEvent) {
    let Some(vp) = dom::by_id("viewport") else {
        return;
    };
    let r = vp.get_bounding_client_rect();
    // Deltas in lines or pages, as some mice send them, in pixels.
    let unit = match ev.delta_mode() {
        WheelEvent::DOM_DELTA_LINE => 16.0,
        WheelEvent::DOM_DELTA_PAGE => r.height(),
        _ => 1.0,
    };
    let (mut dx, mut dy) = (ev.delta_x() * unit, ev.delta_y() * unit);
    let c = &mut app.s.canvas;
    if ev.ctrl_key() || ev.meta_key() {
        ev.prevent_default();
        let (mx, my) = (
            ev.client_x() as f64 - r.left(),
            ev.client_y() as f64 - r.top(),
        );
        let k = (c.pan.k * (-dy * 0.0015).exp()).clamp(0.2, 2.0);
        c.pan.x = mx - (mx - c.pan.x) * k / c.pan.k;
        c.pan.y = my - (my - c.pan.y) * k / c.pan.k;
        c.pan.k = k;
        apply_pan(c);
        return;
    }
    if ev.shift_key() && dx == 0.0 {
        (dx, dy) = (dy, 0.0);
    }
    // Leave sideways scrolling to code that is wider than its card.
    if dy == 0.0 && scrolls_sideways(el, dx) {
        return;
    }
    ev.prevent_default();
    c.pan.x -= dx;
    c.pan.y -= dy;
    apply_pan(c);
}

/// Whether a sideways scrolling box around `el` inside its card (code
/// blocks, tabs) can still scroll by `dx`.
fn scrolls_sideways(el: &Element, dx: f64) -> bool {
    let mut cur = Some(el.clone());
    while let Some(e) = cur {
        if e.class_list().contains("card") {
            return false;
        }
        let max = (e.scroll_width() - e.client_width()) as f64;
        if max > 0.0 && e.matches("pre, .sig, .sbs, .card-tabs").unwrap_or(false) {
            let left = e.scroll_left() as f64;
            if (dx < 0.0 && left > 0.0) || (dx > 0.0 && left < max - 1.0) {
                return true;
            }
        }
        cur = e.parent_element();
    }
    false
}

/// Remembers which source views are expanded across re-renders.
pub fn on_details_toggle(app: &mut App, el: &Element) {
    let Some(details) = dom::closest(el, "details.src") else {
        return;
    };
    let Some(id) = dom::data(&details, "src") else {
        return;
    };
    let open = details.has_attribute("open");
    let gid = dom::closest(&details, ".card")
        .and_then(|c| dom::data(&c, "gid"))
        .and_then(|g| g.parse::<u32>().ok());
    if let Some(gi) = gid.and_then(|g| app.s.canvas.by_gid(g)) {
        app.s.canvas.groups[gi].open_src.insert(id, open);
    }
    draw_edges(app);
}

/// Shows folded unchanged source lines: those behind `button`, or with
/// `data-unfold-all`, all of its source view. Inside a card, remembers
/// them across re-renders.
pub fn on_unfold(app: &mut App, button: &Element) {
    let Some(code) = dom::closest(button, ".src-code") else {
        return;
    };
    let folds = if button.has_attribute("data-unfold-all") {
        dom::query_all_in(&code, "[data-unfold]")
    } else {
        vec![button.clone()]
    };
    let mut starts = Vec::new();
    for b in folds {
        if let Some(fold) = b.next_element_sibling() {
            let _ = fold.remove_attribute("hidden");
        }
        starts.extend(dom::data(&b, "unfold").and_then(|s| s.parse::<usize>().ok()));
        b.remove();
    }
    if dom::query_all_in(&code, "[data-unfold]").is_empty() {
        for b in dom::query_all_in(&code, "[data-unfold-all]") {
            b.remove();
        }
    }
    let gid = dom::closest(&code, ".card")
        .and_then(|c| dom::data(&c, "gid"))
        .and_then(|g| g.parse::<u32>().ok());
    if let (Some(gi), Some(id)) = (
        gid.and_then(|g| app.s.canvas.by_gid(g)),
        dom::data(&code, "item"),
    ) {
        app.s.canvas.groups[gi]
            .unfolded
            .entry(id)
            .or_default()
            .extend(starts);
        draw_edges(app);
    }
}

/// Outlines the card of the item a hovered name refers to.
pub fn highlight_card_of(app: &App, id: &str) {
    let c = &app.s.canvas;
    if let Some(gi) = c.group_of(id)
        && visible(&app.d, c, &c.groups[gi])
        && let Some(el) = card_el(c.groups[gi].gid)
    {
        dom::toggle_class(&el, "hover-target", true);
    }
}

pub fn active_item_of_card(app: &App, card: &Element) -> Option<String> {
    let gid = dom::data(card, "gid")?.parse::<u32>().ok()?;
    let gi = app.s.canvas.by_gid(gid)?;
    Some(app.s.canvas.groups[gi].active.clone())
}
