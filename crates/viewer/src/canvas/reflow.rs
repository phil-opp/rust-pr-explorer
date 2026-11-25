//! Keeps cards from overlapping when one of them changes size, for
//! example when its source view is expanded, another tab is shown or the
//! reader resizes it.

use super::{Canvas, visible};
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
                let changes: Vec<(u32, f64, f64)> = entries
                    .iter()
                    .filter_map(|e| e.dyn_into::<ResizeObserverEntry>().ok())
                    .filter_map(|e| {
                        let el = e.target();
                        let gid = dom::data(&el, "gid")?.parse::<u32>().ok()?;
                        let (w, h) = dom::offset_size(&el);
                        Some((gid, w, h))
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

fn resized(app: &mut App, changes: &[(u32, f64, f64)]) {
    let d = app.d.clone();
    let c = &mut app.s.canvas;
    let mut moved = BTreeSet::new();
    for &(gid, w, h) in changes {
        // A hidden view or a removed card has no size.
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        let Some((old_w, old_h)) = c.sizes.insert(gid, (w, h)) else {
            continue;
        };
        if (h - old_h).abs() >= 1.0 {
            moved.extend(reflow(&d, c, gid, Axis::Y, old_h, h));
        }
        if (w - old_w).abs() >= 1.0 {
            moved.extend(reflow(&d, c, gid, Axis::X, old_w, w));
        }
    }
    if moved.is_empty() {
        return;
    }
    for g in c.groups.iter().filter(|g| moved.contains(&g.gid)) {
        if let Some(el) = super::card_el(g.gid) {
            dom::set_style(&el, "left", &format!("{}px", g.x));
            dom::set_style(&el, "top", &format!("{}px", g.y));
        }
    }
    super::draw_edges(app);
}

/// The direction in which a card changed size: `Y` for its height, `X`
/// for its width.
#[derive(Clone, Copy, PartialEq)]
enum Axis {
    X,
    Y,
}

/// Moves the cards after a card that changed size along `axis` from `old`
/// to `new` by the same amount, so that they keep their distance to it:
/// for a new height the cards below it, for a new width the cards right of
/// it. Cards are after it if they are in its column (or row), or after
/// such a card in theirs. Returns the cards that moved.
fn reflow(d: &Data, c: &mut Canvas, gid: u32, axis: Axis, old: f64, new: f64) -> Vec<u32> {
    let Some(ai) = c.by_gid(gid) else {
        return Vec::new();
    };
    let vis: Vec<usize> = (0..c.groups.len())
        .filter(|&i| visible(d, c, &c.groups[i]))
        .collect();
    // Each card's place and size before the change, as (start, length)
    // along `axis` and across it.
    let rects: Vec<((f64, f64), (f64, f64))> = (0..c.groups.len())
        .map(|i| {
            let g = &c.groups[i];
            let (w, h) = c
                .sizes
                .get(&g.gid)
                .copied()
                .unwrap_or_else(|| (super::width_of(g), super::height_of(d, g)));
            let (w, h) = match (i == ai, axis) {
                (true, Axis::X) => (old, h),
                (true, Axis::Y) => (w, old),
                (false, _) => (w, h),
            };
            match axis {
                Axis::X => ((g.x, w), (g.y, h)),
                Axis::Y => ((g.y, h), (g.x, w)),
            }
        })
        .collect();
    let start = |i: usize| rects[i].0.0;
    let len = |i: usize| rects[i].0.1;
    // In the same column (for heights) or row (for widths).
    let in_line = |i: usize, j: usize| {
        let ((a, al), (b, bl)) = (rects[i].1, rects[j].1);
        a < b + bl && b < a + al
    };
    // Before the change: the cards after the changed one.
    let mut after: Vec<usize> = Vec::new();
    let mut frontier = vec![ai];
    while let Some(s) = frontier.pop() {
        let end = start(s) + len(s);
        for &o in &vis {
            if o != ai && !after.contains(&o) && in_line(o, s) && start(o) >= end - 1.0 {
                after.push(o);
                frontier.push(o);
            }
        }
    }
    if after.is_empty() {
        return Vec::new();
    }
    let mut delta = new - old;
    if delta < 0.0 {
        // Move back only as far as the cards before allow: up to the
        // original distance to the changed card, at most `GAP` close to
        // the others.
        for &s in &after {
            let top = start(s);
            for &x in &vis {
                if x == s || after.contains(&x) || !in_line(x, s) || start(x) >= top {
                    continue;
                }
                let gap = top - (start(x) + len(x));
                if gap < 0.0 {
                    continue;
                }
                let end_now = start(x) + if x == ai { new } else { len(x) };
                let room = (top - end_now - gap.min(GAP)).max(0.0);
                delta = delta.max(-room);
            }
        }
        if delta > -1.0 {
            return Vec::new();
        }
    }
    after
        .into_iter()
        .map(|i| {
            let g = &mut c.groups[i];
            match axis {
                Axis::X => g.x += delta,
                Axis::Y => g.y += delta,
            }
            g.gid
        })
        .collect()
}
