//! Opening and closing cards and tabs, and expanding an item's relatives.

use super::{
    CARD_W, EXPAND_LIMIT, apply_pan, card_el, edges::draw_edges, height_of, place, render,
};
use crate::data::changed;
use crate::{App, View, dom, toast};
use model::{Item, Kind};

/// Places `id` on the canvas, next to the card of `from` if given.
pub fn open(app: &mut App, id: &str, from: Option<&str>, switch_view: bool) {
    let d = app.d.clone();
    let Some(it) = d.item(id) else { return };
    let c = &mut app.s.canvas;
    if !d.visible_from(id, &c.scope) {
        let before = d.mod_label(&c.scope);
        c.scope = d.mod_of(it).to_string();
        toast(&format!(
            "Now viewing from {}: {} is not visible from {before}",
            d.mod_label(&c.scope),
            d.display_name(it)
        ));
    }
    let gi = match c.group_of(id) {
        Some(gi) => {
            c.groups[gi].active = id.to_string();
            gi
        }
        None => {
            let (x, y) = match from.and_then(|f| c.group_of(f)) {
                Some(fi) => (c.groups[fi].x + CARD_W + 80.0, c.groups[fi].y),
                None => {
                    let width = dom::by_id("viewport")
                        .map_or(1000.0, |v| v.get_bounding_client_rect().width());
                    (
                        (width / 2.0 - c.pan.x) / c.pan.k - CARD_W / 2.0,
                        (80.0 - c.pan.y) / c.pan.k,
                    )
                }
            };
            let gi = c.add_group(id, x, y);
            place(&d, c, gi);
            if let Some(f) = from {
                c.add_link(f, id);
            }
            gi
        }
    };
    let gid = c.groups[gi].gid;
    if switch_view && app.s.view != View::Canvas {
        crate::set_view(app, View::Canvas);
    } else {
        render(app);
    }
    flash(gid);
    settle(gid);
    if switch_view {
        ensure_visible(gid);
    }
}

/// After a new card is rendered, moves it if its real height makes it
/// overlap another card.
fn settle(gid: u32) {
    dom::raf(move || {
        crate::with_app(|app| {
            let d = app.d.clone();
            let c = &mut app.s.canvas;
            let Some(gi) = c.by_gid(gid) else { return };
            let (x, y) = (c.groups[gi].x, c.groups[gi].y);
            place(&d, c, gi);
            if (c.groups[gi].x, c.groups[gi].y) != (x, y) {
                render(app);
            }
        });
    });
}

fn flash(gid: u32) {
    dom::raf(move || {
        if let Some(el) = card_el(gid) {
            dom::toggle_class(&el, "flash", false);
            let _ = dom::offset_size(&el);
            dom::toggle_class(&el, "flash", true);
        }
    });
}

fn ensure_visible(gid: u32) {
    dom::raf(move || {
        crate::with_app(|app| {
            let c = &mut app.s.canvas;
            let Some(gi) = c.by_gid(gid) else { return };
            let Some(vp) = dom::by_id("viewport") else {
                return;
            };
            let r = vp.get_bounding_client_rect();
            let g = &c.groups[gi];
            let sx = g.x * c.pan.k + c.pan.x;
            let sy = g.y * c.pan.k + c.pan.y;
            if sx < 0.0 || sy < 0.0 || sx > r.width() - 100.0 || sy > r.height() - 100.0 {
                c.pan.x = r.width() / 2.0 - (g.x + CARD_W / 2.0) * c.pan.k;
                c.pan.y = 60.0 - g.y * c.pan.k;
                apply_pan(c);
                draw_edges(app);
            }
        });
    });
}

pub(super) fn close_tab(app: &mut App, gi: usize, id: &str) {
    let c = &mut app.s.canvas;
    c.groups[gi].tabs.retain(|t| t != id);
    c.links.retain(|(a, b)| a != id && b != id);
    if c.groups[gi].tabs.is_empty() {
        c.groups.remove(gi);
    } else if c.groups[gi].active == id {
        c.groups[gi].active = c.groups[gi].tabs[0].clone();
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Related {
    Callees,
    Callers,
    UsedBy,
}

/// Opens callees, callers or users of an item next to its card.
pub fn expand(app: &mut App, id: &str, kind: Related) {
    let d = app.d.clone();
    let Some(it) = d.item(id) else { return };
    let mut targets: Vec<String> = match kind {
        Related::Callees => it.info.calls.iter().map(|c| c.to.clone()).collect(),
        Related::Callers => d.callers.get(id).map_or(Vec::new(), |v| v.to_vec()),
        Related::UsedBy => d
            .used_by
            .get(id)
            .map_or(Vec::new(), |v| v.iter().map(|(f, _)| f.clone()).collect()),
    };
    targets.sort();
    targets.dedup();
    targets.retain(|t| d.item(t).is_some());
    let c = &mut app.s.canvas;
    let (mut vis, hidden): (Vec<String>, Vec<String>) = targets
        .into_iter()
        .partition(|t| d.visible_from(t, &c.scope));
    if vis.is_empty() {
        toast(&if hidden.is_empty() {
            "Nothing to show".to_string()
        } else {
            format!(
                "All {} are private to submodules of {}",
                hidden.len(),
                d.mod_label(&c.scope)
            )
        });
        return;
    }
    // Changed items first; unchanged ones are only context.
    vis.sort_by(|a, b| {
        let (ia, ib) = (d.item(a).unwrap(), d.item(b).unwrap());
        changed(ib).cmp(&changed(ia)).then_with(|| a.cmp(b))
    });
    let side = if kind == Related::Callees { 1.0 } else { -1.0 };
    let Some(fi) = c.group_of(id) else { return };
    let (fx, fy) = (c.groups[fi].x, c.groups[fi].y);
    let mut offset = 0.0;
    let mut opened = 0;
    let mut skipped = 0;
    for t in &vis {
        let c = &mut app.s.canvas;
        let (from, to) = if side > 0.0 {
            (id, t.as_str())
        } else {
            (t.as_str(), id)
        };
        if c.group_of(t).is_some() {
            c.add_link(from, to);
            continue;
        }
        if opened == EXPAND_LIMIT {
            skipped += 1;
            continue;
        }
        let gi = c.add_group(t, fx + side * (CARD_W + 90.0), fy + offset);
        place(&d, c, gi);
        c.add_link(from, to);
        let gid = c.groups[gi].gid;
        opened += 1;
        // Render now, so the next card is placed using this card's real height.
        render(app);
        let c = &app.s.canvas;
        if let Some(gi) = c.by_gid(gid) {
            offset = c.groups[gi].y - fy + height_of(&d, &c.groups[gi]) + 30.0;
        }
    }
    render(app);
    let mut notes = Vec::new();
    if skipped > 0 {
        notes.push(format!(
            "{skipped} more not opened (use Find item to open them)"
        ));
    }
    if !hidden.is_empty() {
        notes.push(format!(
            "{} {} private to submodules and hidden at this scope",
            hidden.len(),
            if hidden.len() > 1 { "are" } else { "is" }
        ));
    }
    if !notes.is_empty() {
        toast(&notes.join(" · "));
    }
}

/// Opens all members of a type in one tabbed card.
pub(super) fn members_as_tabs(app: &mut App, id: &str) {
    let d = app.d.clone();
    let Some(it) = d.item(id) else { return };
    let c = &mut app.s.canvas;
    let members: Vec<&Item> = d
        .members(it)
        .into_iter()
        .filter(|m| m.info.kind != Kind::Impl && d.visible_from(&m.info.id, &c.scope))
        .collect();
    if members.is_empty() {
        return;
    }
    let Some(fi) = c.group_of(id) else { return };
    let gi = match members.iter().find_map(|m| c.group_of(&m.info.id)) {
        Some(gi) => gi,
        None => {
            let (x, y) = (
                c.groups[fi].x,
                c.groups[fi].y + height_of(&d, &c.groups[fi]) + 40.0,
            );
            let gi = c.add_group(&members[0].info.id, x, y);
            place(&d, c, gi);
            c.add_link(id, &members[0].info.id);
            gi
        }
    };
    let gid = c.groups[gi].gid;
    for m in &members {
        let mid = &m.info.id;
        // Take the member out of any other card first.
        if let Some(oi) = c.group_of(mid)
            && c.groups[oi].gid != gid
        {
            c.groups[oi].tabs.retain(|t| t != mid);
            if c.groups[oi].tabs.is_empty() {
                c.groups.remove(oi);
            } else if &c.groups[oi].active == mid {
                c.groups[oi].active = c.groups[oi].tabs[0].clone();
            }
        }
        let gi = c.by_gid(gid).unwrap();
        if !c.groups[gi].tabs.contains(mid) {
            c.groups[gi].tabs.push(mid.clone());
        }
    }
    let gi = c.by_gid(gid).unwrap();
    c.groups[gi].active = members
        .iter()
        .find(|m| changed(m))
        .map_or_else(|| c.groups[gi].tabs[0].clone(), |m| m.info.id.clone());
    render(app);
}
