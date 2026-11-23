//! Keeps cards from overlapping when one of them changes height, for
//! example when its source view is expanded or another tab is shown.

use super::{CARD_W, Canvas, visible};
use crate::data::Data;
use crate::{App, dom};
use std::cell::RefCell;
use std::collections::BTreeSet;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{ResizeObserver, ResizeObserverEntry};

/// The smallest gap left between a card and one above it when cards move
/// back up after a card shrinks.
const GAP: f64 = 30.0;

thread_local! {
    static OBSERVER: RefCell<Option<ResizeObserver>> = const { RefCell::new(None) };
}

/// Watches the size of the rendered cards. Called after each render, which
/// replaces all card elements.
pub(super) fn observe_cards() {
    OBSERVER.with(|o| {
        let mut o = o.borrow_mut();
        if o.is_none() {
            let cb = Closure::<dyn FnMut(js_sys::Array)>::new(|entries: js_sys::Array| {
                let changes: Vec<(u32, f64)> = entries
                    .iter()
                    .filter_map(|e| e.dyn_into::<ResizeObserverEntry>().ok())
                    .filter_map(|e| {
                        let el = e.target();
                        let gid = dom::data(&el, "gid")?.parse::<u32>().ok()?;
                        Some((gid, dom::offset_size(&el).1))
                    })
                    .collect();
                crate::with_app(|app| resized(app, &changes));
            });
            *o = ResizeObserver::new(cb.as_ref().unchecked_ref()).ok();
            cb.forget();
        }
        let Some(o) = o.as_ref() else { return };
        o.disconnect();
        for el in dom::query_all("#cards .card") {
            o.observe(&el);
        }
    });
}

fn resized(app: &mut App, changes: &[(u32, f64)]) {
    let d = app.d.clone();
    let c = &mut app.s.canvas;
    let mut moved = BTreeSet::new();
    for &(gid, h) in changes {
        // A hidden view or a removed card has no height.
        if h <= 0.0 {
            continue;
        }
        if let Some(old) = c.heights.insert(gid, h)
            && (h - old).abs() >= 1.0
        {
            moved.extend(reflow(&d, c, gid, old, h));
        }
    }
    if moved.is_empty() {
        return;
    }
    for g in c.groups.iter().filter(|g| moved.contains(&g.gid)) {
        if let Some(el) = super::card_el(g.gid) {
            dom::set_style(&el, "top", &format!("{}px", g.y));
        }
    }
    super::draw_edges(app);
}

/// Moves the cards below a card that changed from height `old` to `new` by
/// the same amount, so that they keep their distance to it. Cards are below
/// it if they are in its column, or below such a card in theirs. Returns
/// the cards that moved.
fn reflow(d: &Data, c: &mut Canvas, gid: u32, old: f64, new: f64) -> Vec<u32> {
    let Some(ai) = c.by_gid(gid) else {
        return Vec::new();
    };
    let vis: Vec<usize> = (0..c.groups.len())
        .filter(|&i| visible(d, c, &c.groups[i]))
        .collect();
    let height = |i: usize| -> f64 {
        let g = &c.groups[i];
        if i == ai {
            old
        } else {
            c.heights
                .get(&g.gid)
                .copied()
                .unwrap_or_else(|| super::height_of(d, g))
        }
    };
    let side_by_side = |i: usize, j: usize| (c.groups[i].x - c.groups[j].x).abs() < CARD_W;
    // Before the change: the cards below the changed one.
    let mut below: Vec<usize> = Vec::new();
    let mut frontier = vec![ai];
    while let Some(s) = frontier.pop() {
        let bottom = c.groups[s].y + height(s);
        for &o in &vis {
            if o != ai && !below.contains(&o) && side_by_side(o, s) && c.groups[o].y >= bottom - 1.0
            {
                below.push(o);
                frontier.push(o);
            }
        }
    }
    if below.is_empty() {
        return Vec::new();
    }
    let mut dy = new - old;
    if dy < 0.0 {
        // Move up only as far as the cards above allow: up to the original
        // distance to the changed card, at most `GAP` close to the others.
        for &s in &below {
            let top = c.groups[s].y;
            for &x in &vis {
                if x == s || below.contains(&x) || !side_by_side(x, s) || c.groups[x].y >= top {
                    continue;
                }
                let gap = top - (c.groups[x].y + height(x));
                if gap < 0.0 {
                    continue;
                }
                let bottom_now = c.groups[x].y + if x == ai { new } else { height(x) };
                let room = (top - bottom_now - gap.min(GAP)).max(0.0);
                dy = dy.max(-room);
            }
        }
        if dy > -1.0 {
            return Vec::new();
        }
    }
    below
        .into_iter()
        .map(|i| {
            c.groups[i].y += dy;
            c.groups[i].gid
        })
        .collect()
}
