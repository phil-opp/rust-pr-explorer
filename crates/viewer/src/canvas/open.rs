//! Opening and closing cards and tabs, and expanding an item's relatives.

use super::{
    CARD_W, EXPAND_LIMIT, Expansion, apply_pan, card_el, edges::draw_edges, height_of, place,
    render, visible,
};
use crate::data::changed;
use crate::{App, View, dom, toast};
use model::{Item, Kind};

/// Places `id` on the canvas, next to the card of `from` if given.
pub fn open(app: &mut App, id: &str, from: Option<&str>, switch_view: bool) {
    open_at(app, id, from, switch_view, None);
}

/// Opens the next item to review, for a top-down walk through the call
/// graph: right of a card that calls it, in the next column, or else in a
/// new column right of every card. Then moves the view to it.
pub fn open_for_review(app: &mut App, id: &str, reviewed: Option<&str>) {
    let d = app.d.clone();
    let c = &app.s.canvas;
    let vis: Vec<&super::Group> = c.groups.iter().filter(|g| visible(&d, c, g)).collect();
    let caller = d.callers.get(id).and_then(|callers| {
        // Prefer the card just reviewed.
        let on = |f: &str| vis.iter().copied().find(|g| g.active == f);
        reviewed
            .filter(|r| callers.iter().any(|f| f == r))
            .and_then(on)
            .or_else(|| callers.iter().find_map(|f| on(f)))
    });
    let at = (c.group_of(id).is_none() && !vis.is_empty()).then(|| match caller {
        Some(g) => (g.x + CARD_W + 120.0, g.y),
        None => {
            let x = vis.iter().map(|g| g.x).fold(f64::MIN, f64::max) + CARD_W + 120.0;
            let y = reviewed
                .and_then(|r| vis.iter().find(|g| g.active == r))
                .map_or_else(|| vis.iter().map(|g| g.y).fold(f64::MAX, f64::min), |g| g.y);
            (x, y)
        }
    });
    let from = caller.map(|g| g.active.clone());
    if let Some(gid) = open_at(app, id, from.as_deref(), true, at) {
        // Also when the card is partly in view.
        reveal(gid, true);
    }
}

fn open_at(
    app: &mut App,
    id: &str,
    from: Option<&str>,
    switch_view: bool,
    at: Option<(f64, f64)>,
) -> Option<u32> {
    let d = app.d.clone();
    let it = d.item(id)?;
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
            let (x, y) = match (at, from.and_then(|f| c.group_of(f))) {
                (Some(at), _) => at,
                (None, Some(fi)) => (c.groups[fi].x + CARD_W + 80.0, c.groups[fi].y),
                (None, None) => {
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
    if switch_view {
        // Opened from outside the canvas, or jumped to: a history entry.
        app.s.canvas_focus = Some(id.to_string());
    }
    if switch_view && app.s.view != View::Canvas {
        crate::set_view(app, View::Canvas);
    } else {
        render(app);
        if switch_view {
            crate::nav::record(app);
        }
    }
    flash(gid);
    settle(gid);
    if switch_view {
        reveal(gid, false);
    } else if from.is_some() {
        nudge_into_view(gid);
    }
    Some(gid)
}

/// After following a link from one card to another: pans only as far as
/// needed to show the new card's width, so that the card the link was
/// clicked in stays where it is as far as possible.
fn nudge_into_view(gid: u32) {
    const MARGIN: f64 = 24.0;
    dom::raf(move || {
        crate::with_app(|app| {
            let c = &mut app.s.canvas;
            let Some(gi) = c.by_gid(gid) else { return };
            let Some(vp) = dom::by_id("viewport") else {
                return;
            };
            let r = vp.get_bounding_client_rect();
            let g = &c.groups[gi];
            let left = g.x * c.pan.k + c.pan.x;
            let right = left + CARD_W * c.pan.k;
            let top = g.y * c.pan.k + c.pan.y;
            // Right edge first, but never past the left edge.
            let dx = if right > r.width() - MARGIN {
                -(right - (r.width() - MARGIN)).min(left - MARGIN).max(0.0)
            } else if left < MARGIN {
                MARGIN - left
            } else {
                0.0
            };
            // Vertically when the card's head is out of view: pushed below
            // other cards, or above the view when the link was far down in
            // a long card (then by at most a third of the view, so the
            // link stays in sight).
            let dy = if top > r.height() - 160.0 {
                r.height() - 160.0 - top
            } else if top < 0.0 {
                (MARGIN - top).min(r.height() / 3.0)
            } else {
                0.0
            };
            if dx != 0.0 || dy != 0.0 {
                c.pan.x += dx;
                c.pan.y += dy;
                apply_pan(c);
                draw_edges(app);
            }
        });
    });
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

/// Moves the view to a card: always if `force`, otherwise only if the card
/// is out of view.
fn reveal(gid: u32, force: bool) {
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
            if force || sx < 0.0 || sy < 0.0 || sx > r.width() - 100.0 || sy > r.height() - 100.0 {
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
pub(crate) enum Related {
    Callees,
    Callers,
    UsedBy,
    /// The chains of callers and users from the public API down to the
    /// item (see [`crate::impact`]).
    PublicApi,
}

/// Opens callees, callers or users of an item next to its card.
/// A second click closes what the first one opened.
pub(crate) fn expand(app: &mut App, id: &str, kind: Related) {
    let d = app.d.clone();
    let Some(it) = d.item(id) else { return };
    if let Some(i) = app.s.canvas.live_expansion(id, kind) {
        let n = app.s.canvas.collapse(i);
        render(app);
        toast(&format!("Closed {n} {}", kind.noun(n)));
        return;
    }
    if kind == Related::PublicApi {
        open_chains(app, id);
        return;
    }
    let mut targets: Vec<String> = match kind {
        Related::Callees => it.info.calls.iter().map(|c| c.to.clone()).collect(),
        Related::Callers => d.callers.get(id).map_or(Vec::new(), |v| v.to_vec()),
        Related::UsedBy => d
            .used_by
            .get(id)
            .map_or(Vec::new(), |v| v.iter().map(|(f, _)| f.clone()).collect()),
        Related::PublicApi => Vec::new(),
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
    let mut record = Expansion {
        item: id.to_string(),
        kind,
        cards: Vec::new(),
        links: Vec::new(),
    };
    for t in &vis {
        let c = &mut app.s.canvas;
        let (from, to) = if side > 0.0 {
            (id, t.as_str())
        } else {
            (t.as_str(), id)
        };
        if c.group_of(t).is_some() {
            if c.add_link(from, to) {
                record.links.push((from.to_string(), to.to_string()));
            }
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
        record.cards.push((gid, t.clone()));
        opened += 1;
        // Render now, so the next card is placed using this card's real height.
        render(app);
        let c = &app.s.canvas;
        if let Some(gi) = c.by_gid(gid) {
            offset = c.groups[gi].y - fy + height_of(&d, &c.groups[gi]) + 30.0;
        }
    }
    let added = !record.cards.is_empty() || !record.links.is_empty();
    if added {
        app.s.canvas.expansions.push(record);
    }
    render(app);
    let mut notes = Vec::new();
    if opened > 0 {
        notes.push(format!(
            "Opened {opened} {} (click again or press Undo to close them)",
            kind.noun(opened)
        ));
    }
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

impl Related {
    fn noun(self, n: usize) -> &'static str {
        match (self, n) {
            (Related::Callees, 1) => "callee",
            (Related::Callees, _) => "callees",
            (Related::Callers, 1) => "caller",
            (Related::Callers, _) => "callers",
            (Related::UsedBy, 1) => "item that uses it",
            (Related::UsedBy, _) => "items that use it",
            (Related::PublicApi, 1) => "card on the way from the public API",
            (Related::PublicApi, _) => "cards on the way from the public API",
        }
    }
}

/// Opens the chains from the public API down to `id` left of its card,
/// like "◂ callers" clicked again and again: each item a column further
/// left the more calls it is away, with connectors along the chains. Cards
/// already open are connected, not moved. If the chains go through items
/// that the canvas's scope can't see, the canvas is viewed from a module
/// that sees them.
fn open_chains(app: &mut App, id: &str) {
    let d = app.d.clone();
    let paths = crate::impact::impact(&d, id).paths;
    // Each item's distance from `id`. The chains come from one search, so
    // an item shared by several has the same distance in each.
    let mut dist: Vec<(String, usize)> = Vec::new();
    let mut links: Vec<(String, String)> = Vec::new();
    for path in &paths {
        let n = path.len();
        for (i, p) in path.iter().enumerate().take(n - 1) {
            if !dist.iter().any(|(q, _)| q == p) {
                dist.push((p.clone(), n - 1 - i));
            }
            let l = (p.clone(), path[i + 1].clone());
            if !links.contains(&l) {
                links.push(l);
            }
        }
    }
    if dist.is_empty() {
        return;
    }
    let c = &mut app.s.canvas;
    let all_visible = |scope: &str, c: &super::Canvas| {
        dist.iter().all(|(t, _)| d.visible_from(t, scope))
            && d.visible_from(id, scope)
            && c.groups
                .iter()
                .all(|g| !d.visible_from(&g.active, &c.scope) || d.visible_from(&g.active, scope))
    };
    let mut scope_note = String::new();
    if !all_visible(&c.scope, c) {
        // The modules of the items, innermost first: one of them sees the
        // chains and, if possible, the cards that are visible now.
        let mut scopes: Vec<String> = std::iter::once(id)
            .chain(dist.iter().map(|(t, _)| t.as_str()))
            .filter_map(|t| d.item(t))
            .map(|it| d.mod_of(it).to_string())
            .collect();
        scopes.dedup();
        let new_ids_visible =
            |s: &str| dist.iter().all(|(t, _)| d.visible_from(t, s)) && d.visible_from(id, s);
        if let Some(s) = scopes
            .iter()
            .find(|s| all_visible(s, c))
            .or_else(|| scopes.iter().find(|s| new_ids_visible(s)))
        {
            scope_note = format!(" · Now viewing from {}, which sees them", d.mod_label(s));
            c.scope = s.clone();
        }
    }
    let Some(fi) = c.group_of(id) else { return };
    let (fx, fy) = (c.groups[fi].x, c.groups[fi].y);
    let mut record = Expansion {
        item: id.to_string(),
        kind: Related::PublicApi,
        cards: Vec::new(),
        links: Vec::new(),
    };
    // The next free height in each column.
    let mut next_y: Vec<f64> = Vec::new();
    for (t, k) in &dist {
        let c = &mut app.s.canvas;
        if c.group_of(t).is_some() {
            continue;
        }
        if next_y.len() < *k {
            next_y.resize(*k, fy);
        }
        let gi = c.add_group(t, fx - *k as f64 * (CARD_W + 90.0), next_y[k - 1]);
        place(&d, c, gi);
        let gid = c.groups[gi].gid;
        record.cards.push((gid, t.clone()));
        // Render now, so the next card is placed using this card's real height.
        render(app);
        let c = &app.s.canvas;
        if let Some(gi) = c.by_gid(gid) {
            next_y[k - 1] = c.groups[gi].y + height_of(&d, &c.groups[gi]) + 30.0;
        }
    }
    let c = &mut app.s.canvas;
    let opened: Vec<&str> = record.cards.iter().map(|(_, t)| t.as_str()).collect();
    for (from, to) in &links {
        let new = opened.contains(&from.as_str()) || opened.contains(&to.as_str());
        if c.add_link(from, to) && !new {
            record.links.push((from.clone(), to.clone()));
        }
    }
    let n = record.cards.len();
    if n > 0 || !record.links.is_empty() {
        c.expansions.push(record);
    }
    render(app);
    let tops = paths.len();
    let from = format!(
        "{tops} public item{} reach{} it",
        if tops > 1 { "s" } else { "" },
        if tops > 1 { "" } else { "es" }
    );
    let what = if n > 0 {
        format!(
            "{from}: opened {n} {} (click again or press Undo to close them)",
            Related::PublicApi.noun(n)
        )
    } else {
        format!("{from}, all already open")
    };
    let c = &app.s.canvas;
    let hidden = dist
        .iter()
        .filter(|(t, _)| !d.visible_from(t, &c.scope))
        .count();
    let hidden_note = if hidden > 0 {
        // Private items of sibling modules: no module sees all of them.
        format!(
            " · {hidden} {} private to other modules and hidden at this scope",
            if hidden > 1 { "are" } else { "is" }
        )
    } else {
        String::new()
    };
    toast(&(what + &scope_note + &hidden_note));
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
