//! Where a card opened from another card goes: the free spot that gives
//! the cleanest connectors to the cards already open that it calls or that
//! call it, without widening the canvas for nothing.
//!
//! Candidates are the columns of those cards and the columns just left and
//! right of them, each at the first free height at or below a few starting
//! heights. Each candidate is scored, lowest wins:
//!
//! - per connector to a related card: nothing if it runs left to right
//!   (caller left of callee), more if it runs down the same column, more
//!   still if it runs right to left or up; extra if a card sits in the gap
//!   it crosses, where it would run behind that card; for its length
//!   beyond one gap between columns; and a little for the height between
//!   its ends;
//! - for how far the cards would reach further left or right than now;
//! - a little for the height between the card and the one it was opened
//!   from, so that it stays close.
//!
//! So a card called only from the card it was opened from goes right of
//! that card, as before; one also called from a card further left goes
//! below the card it was opened from when that keeps its connectors short
//! and the canvas narrow.

use super::edges::Rect;

/// Horizontal gap between a card and the column it opens.
pub(super) const COL_GAP: f64 = 80.0;
/// Space kept free around cards.
const MARGIN: f64 = 20.0;
/// Gap left below a card that a new card is placed under.
const BELOW: f64 = 30.0;
/// Where connectors end on a card's side, below its top.
const PORT: f64 = 40.0;

/// Which way the connector between a related card and the new card runs.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Dir {
    /// From the related card into the new one: it calls or uses it.
    In,
    /// From the new card into the related one.
    Out,
}

/// A card already open that the new card will be connected to.
#[derive(Clone, Copy)]
pub(super) struct Related {
    pub(super) r: Rect,
    pub(super) dir: Dir,
    /// Height of the connector's end at this card: the call site, if known.
    pub(super) at: f64,
}

/// The top left corner for a new card of size `w` × `h`, opened from the
/// card `from`. `cards` are all visible cards; `related` the ones among
/// them connected to the new card (including `from`).
pub(super) fn best_spot(
    w: f64,
    h: f64,
    from: Rect,
    related: &[Related],
    cards: &[Rect],
) -> (f64, f64) {
    let mut xs = vec![from.x + from.w + COL_GAP];
    for n in related {
        xs.extend([n.r.x, n.r.x + n.r.w + COL_GAP, n.r.x - w - COL_GAP]);
    }
    let mut ys = vec![from.y];
    ys.extend(related.iter().map(|n| n.at - PORT));
    let mut best: Option<(f64, (f64, f64))> = None;
    for &x in &xs {
        for &y0 in &ys {
            let y = fit(x, y0, w, h, cards);
            let cost = cost(Rect { x, y, w, h }, from, related, cards);
            // Earlier candidates win ties: right of `from` comes first.
            if best.is_none_or(|(c, _)| cost < c - 0.5) {
                best = Some((cost, (x, y)));
            }
        }
    }
    best.map_or((xs[0], from.y), |(_, p)| p)
}

/// The first height at or below `y` where a card of size `w` × `h` at `x`
/// overlaps none of `cards`.
pub(super) fn fit(x: f64, mut y: f64, w: f64, h: f64, cards: &[Rect]) -> f64 {
    for _ in 0..80 {
        let hit = cards.iter().find(|o| {
            x < o.x + o.w + MARGIN
                && x + w + MARGIN > o.x
                && y < o.y + o.h + MARGIN
                && y + h + MARGIN > o.y
        });
        match hit {
            None => break,
            Some(o) => y = o.y + o.h + BELOW,
        }
    }
    y
}

fn cost(c: Rect, from: Rect, related: &[Related], cards: &[Rect]) -> f64 {
    let mut total = 0.0;
    for n in related {
        let mid = c.y + PORT.min(c.h / 2.0);
        total += match n.dir {
            Dir::In => connector(n.r, n.at, c, mid, cards),
            Dir::Out => connector(c, mid, n.r, n.at, cards),
        };
    }
    let left = cards.iter().map(|r| r.x).fold(from.x, f64::min);
    let right = cards
        .iter()
        .map(|r| r.x + r.w)
        .fold(from.x + from.w, f64::max);
    let wider = (c.x + c.w - right).max(0.0) + (left - c.x).max(0.0);
    total + 0.3 * wider + 0.1 * (c.y - from.y).abs()
}

/// What a connector from height `ay` of card `a` to height `by` of card
/// `b` costs.
fn connector(a: Rect, ay: f64, b: Rect, by: f64, cards: &[Rect]) -> f64 {
    let (forward, backward) = (b.x >= a.x + a.w + 16.0, b.x + b.w <= a.x - 16.0);
    let shape = if forward {
        0.0
    } else if backward {
        300.0
    } else if b.y >= a.y + a.h {
        350.0
    } else {
        450.0
    };
    // A card in the gap that the connector crosses, at a height it runs at,
    // would hide part of it.
    let (gap0, gap1) = if forward {
        (a.x + a.w, b.x)
    } else {
        (b.x + b.w, a.x)
    };
    let (top, bottom) = (ay.min(by), ay.max(by));
    let hidden = (forward || backward)
        && cards.iter().any(|o| {
            o.x < gap1 && o.x + o.w > gap0 && o.y < bottom + MARGIN && o.y + o.h > top - MARGIN
        });
    // Across more than one gap between columns, it is long and runs past
    // the cards in between.
    let long = if forward || backward {
        (gap1 - gap0 - COL_GAP).max(0.0)
    } else {
        0.0
    };
    shape + if hidden { 300.0 } else { 0.0 } + 0.3 * long + 0.1 * (ay - by).abs()
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f64 = 580.0;

    fn rect(x: f64, y: f64, h: f64) -> Rect {
        Rect { x, y, w: W, h }
    }

    fn caller(r: Rect, at: f64) -> Related {
        Related {
            r,
            dir: Dir::In,
            at,
        }
    }

    /// A card called only by the card it was opened from goes right of it.
    #[test]
    fn single_caller_opens_to_the_right() {
        let from = rect(0.0, 0.0, 600.0);
        let spot = best_spot(W, 300.0, from, &[caller(from, 300.0)], &[from]);
        assert_eq!(spot, (W + COL_GAP, 0.0));
    }

    /// Like `search` and `find` in dora-rs/dora#4304: `find` (left) calls
    /// `search` (middle), and both call the new card. Right of `search`, the
    /// connector from `find` would run behind `search` and the canvas would
    /// get wider; below `search` neither happens.
    #[test]
    fn card_called_from_two_columns_goes_below_the_middle_one() {
        let find = rect(0.0, 0.0, 1600.0);
        let search = rect(W + COL_GAP, 0.0, 620.0);
        let related = [caller(search, 350.0), caller(find, 1070.0)];
        let (x, y) = best_spot(W, 700.0, search, &related, &[find, search]);
        assert_eq!(x, search.x);
        assert_eq!(y, search.y + search.h + BELOW);
    }

    /// Free space in a column is used before going below the column.
    #[test]
    fn fills_a_gap_in_a_column() {
        let cards = [rect(0.0, 0.0, 200.0), rect(0.0, 700.0, 200.0)];
        assert_eq!(fit(0.0, 0.0, W, 300.0, &cards), 230.0);
        assert_eq!(fit(0.0, 0.0, W, 600.0, &cards), 930.0);
    }
}
