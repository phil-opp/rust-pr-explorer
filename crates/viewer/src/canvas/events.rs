//! Clicks, drags, wheel and toggle events on the canvas.

use super::{
    CARD_W, Drag, apply_pan, card_el,
    edges::draw_edges,
    layout::{arrange_by_module, fit_view},
    open::{Related, close_tab, expand, members_as_tabs},
    place, render, visible,
};
use crate::data::within;
use crate::{App, View, dom, toast};
use web_sys::{Element, PointerEvent, WheelEvent};

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
            render(app);
        }
        _ => return false,
    }
    true
}

/// Starts dragging a card (by its header or tabs) or panning the canvas.
pub fn on_pointer_down(app: &mut App, el: &Element, ev: &PointerEvent) -> bool {
    if dom::closest(el, "#viewport").is_none() {
        return false;
    }
    let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
    let d = app.d.clone();
    let c = &mut app.s.canvas;
    if let Some(frame) = dom::closest(el, "[data-frame]") {
        let m = dom::data(&frame, "frame").unwrap_or_default();
        ev.prevent_default();
        let cards = c
            .groups
            .iter()
            .filter(|g| {
                visible(&d, c, g) && d.item(&g.active).is_some_and(|it| within(d.mod_of(it), &m))
            })
            .map(|g| (g.gid, g.x, g.y))
            .collect();
        c.drag = Some(Drag::Frame {
            sx: x,
            sy: y,
            cards,
        });
        return true;
    }
    if let Some(card) = dom::closest(el, ".card") {
        // Cards are moved by their header, tabs, location line, footer, or
        // border; not by code or text, which can be selected.
        let handle = el == &card
            || dom::closest(el, ".card-h, .card-tabs, .card-loc").is_some()
            || el
                .matches(".card-foot, .card-acts, .card-rv")
                .unwrap_or(false);
        if !handle || dom::closest(el, "[data-close], button, a, input, textarea").is_some() {
            return false;
        }
        let Some(gi) = dom::data(&card, "gid")
            .and_then(|g| g.parse::<u32>().ok())
            .and_then(|g| c.by_gid(g))
        else {
            return false;
        };
        ev.prevent_default();
        let g = &c.groups[gi];
        c.drag = Some(Drag::Card {
            gid: g.gid,
            sx: x,
            sy: y,
            gx: g.x,
            gy: g.y,
        });
        dom::set_style(&card, "z-index", "10");
        dom::set_style(&card, "pointer-events", "none");
    } else {
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
    true
}

/// The card whose header is under the pointer, if the pointer is close
/// enough to the top to mean "drop here to merge into tabs".
fn drop_target(x: f64, y: f64, dragged: u32) -> Option<Element> {
    let under = dom::document().element_from_point(x as f32, y as f32)?;
    let card = dom::closest(&under, ".card")?;
    let gid = dom::data(&card, "gid")?.parse::<u32>().ok()?;
    (gid != dragged && y - card.get_bounding_client_rect().top() < 60.0).then_some(card)
}

pub fn on_pointer_move(app: &mut App, ev: &PointerEvent) {
    let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
    let c = &mut app.s.canvas;
    match c.drag {
        Some(Drag::Card {
            gid,
            sx,
            sy,
            gx,
            gy,
        }) => {
            let Some(gi) = c.by_gid(gid) else { return };
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
            c.pan.x = px + x - sx;
            c.pan.y = py + y - sy;
            apply_pan(c);
        }
        None => {}
    }
}

pub fn on_pointer_up(app: &mut App, ev: &PointerEvent) {
    let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
    let Some(drag) = app.s.canvas.drag.take() else {
        return;
    };
    if let Some(vp) = dom::by_id("viewport") {
        dom::toggle_class(&vp, "panning", false);
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
}

pub fn on_wheel(app: &mut App, el: &Element, ev: &WheelEvent) {
    if dom::closest(el, ".card").is_some() && !ev.ctrl_key() {
        return;
    }
    ev.prevent_default();
    let Some(vp) = dom::by_id("viewport") else {
        return;
    };
    let r = vp.get_bounding_client_rect();
    let (mx, my) = (
        ev.client_x() as f64 - r.left(),
        ev.client_y() as f64 - r.top(),
    );
    let c = &mut app.s.canvas;
    let k = (c.pan.k * (-ev.delta_y() * 0.0015).exp()).clamp(0.2, 2.0);
    c.pan.x = mx - (mx - c.pan.x) * k / c.pan.k;
    c.pan.y = my - (my - c.pan.y) * k / c.pan.k;
    c.pan.k = k;
    apply_pan(c);
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
        let set = &mut app.s.canvas.groups[gi].open_src;
        if open {
            set.insert(id)
        } else {
            set.remove(&id)
        };
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
