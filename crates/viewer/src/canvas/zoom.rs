//! Zooming to a card by double-clicking (or double-tapping) its header:
//! the first time to a comfortable reading size, then one step closer each
//! time, and after the closest step back to the reading size.

use super::{Pan, apply_pan, card_el, edges::draw_edges, events::SLOP};
use crate::{App, dom, toast};
use std::cell::Cell;
use web_sys::{Element, PointerEvent};

/// The code font size, in screen pixels, at each zoom step.
const STEPS: [f64; 3] = [16.0, 20.0, 25.0];
/// The zoom range of the canvas, as for the wheel and a pinch.
const MIN_K: f64 = 0.2;
const MAX_K: f64 = 2.0;
/// The longest press that still counts as a click, and the longest pause
/// between the two clicks of a double click, in milliseconds.
const CLICK_MS: f64 = 450.0;
const DOUBLE_MS: f64 = 400.0;
/// The space kept between the card and the edge of the view.
const MARGIN: f64 = 24.0;

#[derive(Clone, Copy)]
struct Press {
    gid: u32,
    x: f64,
    y: f64,
    t: f64,
}

thread_local! {
    /// The pointer pressed on a card's header and not yet lifted.
    static PRESS: Cell<Option<Press>> = const { Cell::new(None) };
    /// The last click on a card's header, which a second one makes a
    /// double click.
    static CLICK: Cell<Option<Press>> = const { Cell::new(None) };
    /// The card the last double click zoomed to, and the zoom it set.
    static ZOOMED: Cell<Option<(u32, f64)>> = const { Cell::new(None) };
}

/// Remembers a press on a card's header (not on its close button or
/// another control), which may be half of a double click.
pub fn on_press(el: &Element, ev: &PointerEvent) {
    let gid = (ev.is_primary() && ev.button() == 0)
        .then(|| dom::closest(el, ".card-h"))
        .flatten()
        .filter(|_| dom::closest(el, "[data-close], button, a, input, textarea").is_none())
        .and_then(|h| dom::closest(&h, ".card"))
        .and_then(|card| dom::data(&card, "gid"))
        .and_then(|g| g.parse::<u32>().ok());
    PRESS.set(gid.map(|gid| Press {
        gid,
        x: ev.client_x() as f64,
        y: ev.client_y() as f64,
        t: js_sys::Date::now(),
    }));
}

/// A second finger: a pinch, not a click.
pub fn cancel_press() {
    PRESS.set(None);
    CLICK.set(None);
}

/// When the pointer is lifted: the card whose header it double clicked,
/// if it did.
pub fn on_release(ev: &PointerEvent) -> Option<u32> {
    let press = PRESS.take()?;
    if ev.type_() == "pointercancel" {
        CLICK.set(None);
        return None;
    }
    let (x, y, t) = (
        ev.client_x() as f64,
        ev.client_y() as f64,
        js_sys::Date::now(),
    );
    if (x - press.x).hypot(y - press.y) > SLOP || t - press.t > CLICK_MS {
        CLICK.set(None);
        return None;
    }
    match CLICK.take() {
        Some(first)
            if first.gid == press.gid
                && press.t - first.t < DOUBLE_MS
                && (press.x - first.x).hypot(press.y - first.y) < 2.0 * SLOP =>
        {
            Some(press.gid)
        }
        _ => {
            CLICK.set(Some(press));
            None
        }
    }
}

/// The zoom of each step, with the code font at `code` pixels.
fn steps(code: f64) -> Vec<f64> {
    let mut ks: Vec<f64> = STEPS
        .iter()
        .map(|s| (s / code).clamp(MIN_K, MAX_K))
        .collect();
    ks.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    ks
}

/// The zoom after a double click on a card: one step closer than `k` if
/// the last double click zoomed to the same card and the zoom wasn't
/// changed since, else (and after the closest step) the first step.
fn next_zoom(steps: &[f64], k: f64, again: bool) -> (f64, bool) {
    let next = steps
        .iter()
        .copied()
        .find(|&s| again && s > k + 0.01)
        .unwrap_or(steps[0]);
    (next, next == steps[steps.len() - 1])
}

/// The view that shows a card at `x`, `y` (`w` by `h`, in canvas
/// coordinates) at zoom `k` in a view `vw` by `vh`: centered, but with its
/// left edge or top in the view where it doesn't fit, since that's where
/// its name and code start.
fn view_of(x: f64, y: f64, w: f64, h: f64, k: f64, vw: f64, vh: f64) -> Pan {
    let place = |pos: f64, size: f64, view: f64| {
        if size * k <= view - 2.0 * MARGIN {
            (view - size * k) / 2.0 - pos * k
        } else {
            MARGIN - pos * k
        }
    };
    Pan {
        x: place(x, w, vw),
        y: place(y, h, vh),
        k,
    }
}

/// Centers the card `gid` and zooms to the next step.
pub fn zoom_to(app: &mut App, gid: u32) {
    let c = &mut app.s.canvas;
    let (Some(gi), Some(card), Some(vp)) = (c.by_gid(gid), card_el(gid), dom::by_id("viewport"))
    else {
        return;
    };
    let code = crate::settings::get().size as f64;
    let steps = steps(code);
    let again = ZOOMED.get().is_some_and(|(g, k)| g == gid && k == c.pan.k);
    let (k, closest) = next_zoom(&steps, c.pan.k, again);
    let (w, h) = dom::offset_size(&card);
    let r = vp.get_bounding_client_rect();
    let g = &c.groups[gi];
    c.pan = view_of(g.x, g.y, w, h, k, r.width(), r.height());
    ZOOMED.set(Some((gid, k)));
    if let Some(world) = dom::by_id("world") {
        dom::toggle_class(&world, "zooming", true);
        // Connectors are placed by measuring the cards on screen, which
        // is off while the zoom glides; draw them again once it ended.
        dom::timeout(300, move || {
            dom::toggle_class(&world, "zooming", false);
            crate::with_app(|app| draw_edges(app));
        });
    }
    apply_pan(c);
    if closest && steps.len() > 1 {
        toast("Closest zoom. Double-click again to zoom back out, or use Fit");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_follow_the_code_font() {
        assert_eq!(steps(16.0), vec![1.0, 1.25, 1.5625]);
        // A small font would need more than the closest zoom.
        assert_eq!(steps(10.0), vec![1.6, 2.0]);
    }

    #[test]
    fn double_clicks_step_closer_then_start_over() {
        let s = [1.0, 1.25, 1.5];
        assert_eq!(next_zoom(&s, 0.6, false), (1.0, false));
        assert_eq!(next_zoom(&s, 1.0, true), (1.25, false));
        assert_eq!(next_zoom(&s, 1.25, true), (1.5, true));
        assert_eq!(next_zoom(&s, 1.5, true), (1.0, false));
        // Another card, or a zoom changed since: the first step, also when
        // that zooms out.
        assert_eq!(next_zoom(&s, 1.8, false), (1.0, false));
    }

    #[test]
    fn a_card_that_fits_is_centered() {
        let p = view_of(100.0, 50.0, 200.0, 100.0, 2.0, 1000.0, 600.0);
        assert_eq!((p.x, p.y), (300.0 - 200.0, 200.0 - 100.0));
    }

    #[test]
    fn a_card_too_big_shows_its_top_left() {
        let p = view_of(100.0, 50.0, 600.0, 800.0, 1.5, 800.0, 600.0);
        assert_eq!((p.x, p.y), (MARGIN - 150.0, MARGIN - 75.0));
    }
}
