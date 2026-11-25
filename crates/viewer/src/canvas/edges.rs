//! Connectors between cards, and frames around the cards of a module.

use super::{Group, card_el, visible};
use crate::code::esc;
use crate::{App, dom, route};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Copy)]
pub(super) struct Rect {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
}

pub(super) fn card_rect(g: &Group) -> Option<Rect> {
    let el = card_el(g.gid)?;
    let (w, h) = dom::offset_size(&el);
    Some(Rect {
        x: g.x,
        y: g.y,
        w,
        h,
    })
}

fn edge_path(a: Rect, b: Rect) -> String {
    let (acx, acy) = (a.x + a.w / 2.0, a.y + a.h / 2.0);
    let (bcx, bcy) = (b.x + b.w / 2.0, b.y + b.h / 2.0);
    if (bcx - acx).abs() > (a.w + b.w) / 2.0 - 40.0 {
        let dir = if bcx > acx { 1.0 } else { -1.0 };
        let p1 = (
            if dir > 0.0 { a.x + a.w } else { a.x },
            (a.y + 40.0).min(acy),
        );
        let p2 = (
            if dir > 0.0 { b.x } else { b.x + b.w },
            (b.y + 40.0).min(bcy),
        );
        let dx = ((p2.0 - p1.0).abs() / 2.0).max(40.0);
        format!(
            "M{},{} C{},{} {},{} {},{}",
            p1.0,
            p1.1,
            p1.0 + dir * dx,
            p1.1,
            p2.0 - dir * dx,
            p2.1,
            p2.0,
            p2.1
        )
    } else {
        let dir = if bcy > acy { 1.0 } else { -1.0 };
        let p1 = (acx, if dir > 0.0 { a.y + a.h } else { a.y });
        let p2 = (bcx, if dir > 0.0 { b.y } else { b.y + b.h });
        let dy = ((p2.1 - p1.1).abs() / 2.0).max(30.0);
        format!(
            "M{},{} C{},{} {},{} {},{}",
            p1.0,
            p1.1,
            p1.0,
            p1.1 + dir * dy,
            p2.0,
            p2.1 - dir * dy,
            p2.0,
            p2.1
        )
    }
}

/// A connector from a call site at height `y` in card `a` to card `b`: it
/// leaves `a` at the side facing `b`, level with the call.
fn site_path(a: Rect, y: f64, b: Rect) -> String {
    let dir = if b.x + b.w / 2.0 >= a.x + a.w / 2.0 {
        1.0
    } else {
        -1.0
    };
    let p1 = (if dir > 0.0 { a.x + a.w } else { a.x }, y);
    // Into the near side of `b` if it is beside `a`, else into its top.
    let (p2, c2) = if (dir > 0.0 && b.x > p1.0 + 20.0) || (dir < 0.0 && b.x + b.w < p1.0 - 20.0) {
        let p2 = (
            if dir > 0.0 { b.x } else { b.x + b.w },
            b.y + 40.0_f64.min(b.h / 2.0),
        );
        let dx = ((p2.0 - p1.0).abs() / 2.0).max(40.0);
        (p2, (p2.0 - dir * dx, p2.1))
    } else {
        let p2 = (b.x + b.w / 2.0, b.y);
        (p2, (p2.0, p2.1 - ((p2.1 - y).abs() / 2.0).max(40.0)))
    };
    let dx = ((p2.0 - p1.0).abs() / 2.0).max(40.0);
    format!(
        "M{},{} C{},{} {},{} {},{}",
        p1.0,
        p1.1,
        p1.0 + dir * dx,
        p1.1,
        c2.0,
        c2.1,
        p2.0,
        p2.1
    )
}

/// The heights, in canvas coordinates, of the call sites of `to` in the
/// expanded source of card `g`, top to bottom. A call in folded lines is at
/// the row that unfolds them. Calls on removed lines count only if no other
/// line calls `to`: the connectors show the current calls.
fn call_site_ys(g: &Group, to: &str, k: f64) -> Vec<f64> {
    let Some(card) = card_el(g.gid) else {
        return Vec::new();
    };
    let top = card.get_bounding_client_rect().top();
    let (removed, current): (Vec<_>, Vec<_>) =
        dom::query_all_in(&card, "details.src[open] a.ref[data-id]")
            .into_iter()
            .filter(|a| dom::data(a, "id").as_deref() == Some(to))
            .partition(|a| dom::closest(a, ".ln.del").is_some());
    let sites = if current.is_empty() { removed } else { current };
    let mut ys: Vec<f64> = sites
        .iter()
        .filter_map(|a| {
            let r = a.get_bounding_client_rect();
            if r.height() > 0.0 {
                return Some(r);
            }
            let fold = dom::closest(a, ".fold[hidden]")?;
            let r = fold.previous_element_sibling()?.get_bounding_client_rect();
            (r.height() > 0.0).then_some(r)
        })
        .map(|r| g.y + (r.top() + r.height() / 2.0 - top) / k)
        .collect();
    ys.sort_by(f64::total_cmp);
    // Calls on one line, or in one fold, share a connector.
    ys.dedup_by(|a, b| (*a - *b).abs() < 1.0);
    ys
}

/// The height of the first call site of `to` in card `g`, see
/// [`call_site_ys`].
pub(super) fn call_site_y(g: &Group, to: &str, k: f64) -> Option<f64> {
    call_site_ys(g, to, k).first().copied()
}

/// Cards closer than this, side by side, are connected through their tops
/// and bottoms instead.
const MIN_GAP: f64 = 16.0;
/// Distance between connectors that end on one side of a card.
const PORT_GAP: f64 = 14.0;
/// Distance between connectors that run through one gap between cards.
const LANE_GAP: f64 = 12.0;
/// How far right of two cards a connector between their right sides runs.
const BESIDE: f64 = 24.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

/// Start and end point of a connector.
type Ends = ((f64, f64), (f64, f64));

#[derive(Clone, Copy)]
enum Route {
    /// Leaves `a` and enters `b` at right angles, through a lane in the
    /// gap between them: horizontal for left and right sides.
    Ortho(Side, Side),
    /// From a call site to the top of a card that is not beside the caller.
    Site(f64),
    /// The cards overlap: a plain curve between them.
    Curve,
}

struct Conn {
    a: (u32, Rect),
    b: (u32, Rect),
    class: String,
    title: String,
    /// Height of the call site in `a`, which the connector starts at.
    site_y: Option<f64>,
    /// Heights of further call sites in `a`, whose connectors join this
    /// one on the way to `b`.
    more_sites: Vec<f64>,
    route: Route,
}

fn choose_route(a: Rect, b: Rect, site_y: Option<f64>) -> Route {
    if b.x >= a.x + a.w + MIN_GAP {
        Route::Ortho(Side::Right, Side::Left)
    } else if b.x + b.w <= a.x - MIN_GAP {
        Route::Ortho(Side::Left, Side::Right)
    } else if site_y.is_some() && b.x >= a.x + a.w {
        // Closer than `MIN_GAP`, but still beside each other: a call site
        // has to leave from the side.
        Route::Ortho(Side::Right, Side::Left)
    } else if site_y.is_some() && b.x + b.w <= a.x {
        Route::Ortho(Side::Left, Side::Right)
    } else if site_y.is_some() && (b.y >= a.y + a.h || b.y + b.h <= a.y) {
        // Above or below the caller, in its column, however close: out of
        // the caller's right side level with the call, down (or up) beside
        // the cards, and into the right side of the callee. A curve into
        // the callee's top would run behind the caller whenever the callee
        // isn't clearly below it.
        Route::Ortho(Side::Right, Side::Right)
    } else if let Some(y) = site_y {
        Route::Site(y)
    } else if b.y >= a.y + a.h + MIN_GAP {
        Route::Ortho(Side::Bottom, Side::Top)
    } else if b.y + b.h <= a.y - MIN_GAP {
        Route::Ortho(Side::Top, Side::Bottom)
    } else {
        Route::Curve
    }
}

/// Where connectors end on the sides of the cards: spread along each side,
/// ordered by where their other end is, so that they neither share an
/// arrowhead nor cross next to the card. A connector that starts at a call
/// site keeps that height. Returns the start and end point of each
/// orthogonal connector.
fn ports(conns: &[Conn]) -> Vec<Ends> {
    // Per card side: (where the other end is, connector, whether it starts here).
    type Slots = BTreeMap<(u32, Side), Vec<(f64, usize, bool)>>;
    let mut slots: Slots = BTreeMap::new();
    let along = |r: Rect, side: Side, fixed: Option<f64>| match side {
        Side::Left | Side::Right => fixed.unwrap_or(r.y + r.h / 2.0),
        Side::Top | Side::Bottom => r.x + r.w / 2.0,
    };
    for (i, c) in conns.iter().enumerate() {
        let Route::Ortho(sa, sb) = c.route else {
            continue;
        };
        if c.site_y.is_none() {
            slots
                .entry((c.a.0, sa))
                .or_default()
                .push((along(c.b.1, sa, None), i, true));
        }
        slots
            .entry((c.b.0, sb))
            .or_default()
            .push((along(c.a.1, sb, c.site_y), i, false));
    }
    let mut out: Vec<Ends> = conns
        .iter()
        .map(|c| {
            let y = c.site_y.unwrap_or(0.0);
            let x = match c.route {
                Route::Ortho(Side::Left, _) => c.a.1.x,
                _ => c.a.1.x + c.a.1.w,
            };
            ((x, y), (0.0, 0.0))
        })
        .collect();
    for ((_, side), list) in &mut slots {
        list.sort_by(|x, y| x.0.total_cmp(&y.0));
        let n = list.len() as f64;
        for (k, &(_, i, from)) in list.iter().enumerate() {
            let r = if from { conns[i].a.1 } else { conns[i].b.1 };
            let k = k as f64;
            let p = match side {
                Side::Left | Side::Right => {
                    // From the title row down, closer together on short cards.
                    let first = 40.0_f64.min(r.h / 2.0);
                    let gap = PORT_GAP.min((r.h - first - 12.0) / n).max(2.0);
                    let y = r.y + first + k * gap;
                    let x = if *side == Side::Left { r.x } else { r.x + r.w };
                    (x, y)
                }
                Side::Top | Side::Bottom => {
                    let x = (r.x + r.w / 2.0 + (k - (n - 1.0) / 2.0) * 2.0 * PORT_GAP)
                        .clamp(r.x + 12.0, r.x + r.w - 12.0);
                    let y = if *side == Side::Top { r.y } else { r.y + r.h };
                    (x, y)
                }
            };
            if from {
                out[i].0 = p;
            } else {
                out[i].1 = p;
            }
        }
    }
    out
}

/// The lane of each orthogonal connector: the x of its vertical run, or the
/// y of its horizontal run between a bottom and a top. Connectors through
/// one gap get lanes of their own, ordered so that they cross as little as
/// they can.
fn lanes(conns: &[Conn], ends: &[Ends]) -> Vec<f64> {
    // Per gap (whether horizontal, from, to): (order, connector).
    type Gaps = BTreeMap<(bool, i64, i64), Vec<((bool, f64), usize, Crossing)>>;
    let mut gaps: Gaps = BTreeMap::new();
    let mut out = vec![0.0; conns.len()];
    // Connectors between two cards' right sides run beside the cards: per
    // column edge, each in a lane of its own, further out the longer it is.
    let mut beside: BTreeMap<i64, Vec<(f64, usize)>> = BTreeMap::new();
    for (i, c) in conns.iter().enumerate() {
        let Route::Ortho(sa, sb) = c.route else {
            continue;
        };
        if sa == sb {
            let (p, q) = ends[i];
            beside
                .entry(p.0.max(q.0).round() as i64)
                .or_default()
                .push(((p.1 - q.1).abs(), i));
            continue;
        }
        let horizontal = matches!(sa, Side::Left | Side::Right);
        // In horizontal coordinates: `u` across the gap, `v` along it.
        let (p, q) = ends[i];
        let (p, q) = if horizontal {
            (p, q)
        } else {
            ((p.1, p.0), (q.1, q.0))
        };
        let (near, far) = if p.0 <= q.0 { (p, q) } else { (q, p) };
        // A connector that goes down from the near side to the far side
        // crosses none of the others that do if those that start higher
        // run closer to the far side; and the other way round for
        // connectors that go up.
        let down = near.1 <= far.1;
        let order = if down { -near.1 } else { near.1 };
        // Cards in between would hide a bend in the middle of a long
        // vertical connector: it bends just below the upper card instead.
        let far_u = if horizontal {
            far.0
        } else {
            far.0.min(near.0 + 40.0)
        };
        // Where the connector meets the cards, along the gap: its first
        // call site and any further ones are on `a`'s side.
        let a_near = p.0 <= q.0;
        let mut at_a = vec![p.1];
        at_a.extend(c.more_sites.iter().copied());
        let (near_vs, far_vs) = if a_near {
            (at_a, vec![q.1])
        } else {
            (vec![q.1], at_a)
        };
        gaps.entry((horizontal, near.0.round() as i64, far_u.round() as i64))
            .or_default()
            .push((
                (!down, order),
                i,
                Crossing {
                    near: near_vs,
                    far: far_vs,
                },
            ));
    }
    for (edge, mut list) in beside {
        list.sort_by(|x, y| x.0.total_cmp(&y.0));
        for (k, &(_, i)) in list.iter().enumerate() {
            out[i] = edge as f64 + BESIDE + k as f64 * LANE_GAP;
        }
    }
    for ((_, u0, u1), mut list) in gaps {
        list.sort_by(|x, y| x.0.0.cmp(&y.0.0).then(x.0.1.total_cmp(&y.0.1)));
        let shapes: Vec<Crossing> = list.iter().map(|x| x.2.clone()).collect();
        let order = lane_order(&shapes);
        let (u0, u1) = (u0 as f64, u1 as f64);
        let n = list.len() as f64;
        let gap = LANE_GAP.min((u1 - u0) / (n + 1.0));
        for (k, &o) in order.iter().enumerate() {
            out[list[o].1] = (u0 + u1) / 2.0 + (k as f64 - (n - 1.0) / 2.0) * gap;
        }
    }
    out
}

/// The SVG paths of a connector.
#[derive(Default)]
struct Paths {
    /// The connector itself, from the first call site.
    main: String,
    /// The connectors of further call sites, up to where they join `main`.
    branches: String,
    /// Where the branches join `main`.
    joins: Vec<(f64, f64)>,
    /// The corners of `main` and of each branch, for orthogonal connectors.
    lines: Vec<Vec<(f64, f64)>>,
}

/// Where a connector through a gap meets the cards on either side, along
/// the gap (`v` in [`lanes`]): it runs from each of these points across
/// to its lane, and along the lane between them.
#[derive(Clone)]
struct Crossing {
    near: Vec<f64>,
    far: Vec<f64>,
}

impl Crossing {
    fn span(&self) -> (f64, f64) {
        let all = self.near.iter().chain(&self.far).copied();
        let lo = all.clone().fold(f64::MAX, f64::min);
        (lo, all.fold(f64::MIN, f64::max))
    }
}

/// Up to this many connectors through one gap, [`lane_order`] tries every
/// order of their lanes.
const MAX_EXACT: usize = 7;

/// The order of the lanes in a gap, from the near side: the one with the
/// fewest crossings, then the one whose connectors cross others at the
/// fewest places of their own (a connector that crosses two others in one
/// run is easier to follow than one that crosses them on two), then the
/// one closest to the given order. With more than [`MAX_EXACT`]
/// connectors, the given order.
fn lane_order(shapes: &[Crossing]) -> Vec<usize> {
    let n = shapes.len();
    if n > MAX_EXACT {
        return (0..n).collect();
    }
    let spans: Vec<(f64, f64)> = shapes.iter().map(Crossing::span).collect();
    let inside = |v: f64, (lo, hi): (f64, f64)| v > lo + 0.5 && v < hi - 0.5;
    let count = |vs: &[f64], span| vs.iter().filter(|&&v| inside(v, span)).count();
    let score = |order: &[usize]| {
        let (mut total, mut places, mut moved) = (0, 0, 0);
        for (k, &i) in order.iter().enumerate() {
            // Crossings on the runs to the near side, the runs to the far
            // side, and the run along the lane.
            let (mut near, mut far, mut along) = (0, 0, 0);
            for (l, &j) in order.iter().enumerate() {
                if l < k {
                    near += count(&shapes[i].near, spans[j]);
                    along += count(&shapes[j].far, spans[i]);
                } else if l > k {
                    far += count(&shapes[i].far, spans[j]);
                    along += count(&shapes[j].near, spans[i]);
                    moved += usize::from(j < i);
                }
            }
            total += near + far;
            places += [near, far, along].iter().filter(|&&x| x > 0).count();
        }
        (total, places, moved)
    };
    let mut order: Vec<usize> = (0..n).collect();
    let mut best = (score(&order), order.clone());
    // Heap's algorithm: every permutation, one swap at a time.
    let mut c = vec![0; n];
    let mut k = 1;
    while k < n {
        if c[k] < k {
            order.swap(if k % 2 == 0 { 0 } else { c[k] }, k);
            let s = score(&order);
            if s < best.0 {
                best = (s, order.clone());
            }
            c[k] += 1;
            k = 1;
        } else {
            c[k] = 0;
            k += 1;
        }
    }
    best.1
}

fn conn_path(c: &Conn, ends: Ends, lane: f64) -> Paths {
    let mut out = Paths::default();
    match c.route {
        Route::Ortho(sa, sb) => {
            let (p, q) = ends;
            let points = match sa {
                _ if sa != sb && (p.0 == q.0 || p.1 == q.1) => vec![p, q],
                Side::Left | Side::Right => vec![p, (lane, p.1), (lane, q.1), q],
                Side::Top | Side::Bottom => vec![p, (p.0, lane), (q.0, lane), q],
            };
            out.main = route::path_d(&points);
            out.lines.push(points);
            // Further call sites run into the main connector's lane and
            // join it where its run along the lane is straight (its bends
            // are rounded).
            let (lo, hi) = (p.1.min(q.1), p.1.max(q.1));
            let r = 8.0_f64.min((hi - lo) / 2.0);
            let (lo, hi) = (lo + r, hi - r);
            for &y in &c.more_sites {
                let join = y.clamp(lo, hi);
                let end = if join == y {
                    // Beside the lane: a bend into it, towards `b`.
                    let s = if q.1 >= y { 1.0 } else { -1.0 };
                    (y + s * 12.0).clamp(lo, hi)
                } else {
                    join
                };
                let points = vec![(p.0, y), (lane, y), (lane, end)];
                out.branches.push_str(&route::path_d(&points));
                out.lines.push(points);
                out.branches.push(' ');
                out.joins.push((lane, join));
            }
        }
        Route::Site(y) => {
            out.main = site_path(c.a.1, y, c.b.1);
            for &y in &c.more_sites {
                out.branches.push_str(&site_path(c.a.1, y, c.b.1));
                out.branches.push(' ');
            }
        }
        Route::Curve => out.main = edge_path(c.a.1, c.b.1),
    }
    out
}

/// Which connectors cross a longer one: those are drawn over it, a bit
/// bolder, with a gap cut into the longer one. Only orthogonal connectors
/// are checked, by their straight runs.
fn over_longer(lines: &[&[Vec<(f64, f64)>]]) -> Vec<bool> {
    type Seg = ((f64, f64), (f64, f64));
    let segs: Vec<Vec<Seg>> = lines
        .iter()
        .map(|ls| {
            ls.iter()
                .flat_map(|l| l.windows(2).map(|w| (w[0], w[1])))
                .collect()
        })
        .collect();
    let len = |i: usize| -> f64 {
        lines[i]
            .first()
            .map(|l| {
                l.windows(2)
                    .map(|w| (w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs())
                    .sum()
            })
            .unwrap_or(0.0)
    };
    let between = |v: f64, a: f64, b: f64| v > a.min(b) + 1.0 && v < a.max(b) - 1.0;
    let cross = |(a, b): Seg, (c, d): Seg| {
        let horizontal = |p: (f64, f64), q: (f64, f64)| p.1 == q.1 && p.0 != q.0;
        let vertical = |p: (f64, f64), q: (f64, f64)| p.0 == q.0 && p.1 != q.1;
        let hv = |(a, b): Seg, (c, d): Seg| {
            horizontal(a, b) && vertical(c, d) && between(c.0, a.0, b.0) && between(a.1, c.1, d.1)
        };
        hv((a, b), (c, d)) || hv((c, d), (a, b))
    };
    let n = lines.len();
    let mut out = vec![false; n];
    for i in 0..n {
        for j in 0..n {
            if i != j
                && len(i) < len(j)
                && segs[i]
                    .iter()
                    .any(|&x| segs[j].iter().any(|&y| cross(x, y)))
            {
                out[i] = true;
            }
        }
    }
    out
}

/// Number of colors for call connectors (`--call-0` … in the CSS).
const CALL_COLORS: usize = 5;

/// The bounding box of a connector: (x0, y0, x1, y1).
fn conn_box(c: &Conn, ends: Ends, lane: f64) -> (f64, f64, f64, f64) {
    let (p, q) = ends;
    let mut xs = vec![c.a.1.x, c.a.1.x + c.a.1.w];
    let mut ys = vec![];
    match c.route {
        Route::Ortho(sa, _) => {
            xs = vec![p.0, q.0];
            ys = vec![p.1, q.1];
            if matches!(sa, Side::Left | Side::Right) {
                xs.push(lane);
            } else {
                ys.push(lane);
            }
        }
        Route::Site(y) => {
            ys.push(y);
            xs.extend([c.b.1.x, c.b.1.x + c.b.1.w]);
            ys.push(c.b.1.y);
        }
        Route::Curve => {
            xs.extend([c.b.1.x, c.b.1.x + c.b.1.w]);
            ys.extend([c.a.1.y, c.b.1.y]);
        }
    }
    ys.extend(c.more_sites.iter().copied());
    let min = |v: &[f64]| v.iter().copied().fold(f64::MAX, f64::min);
    let max = |v: &[f64]| v.iter().copied().fold(f64::MIN, f64::max);
    (min(&xs), min(&ys), max(&xs), max(&ys))
}

/// A color for each call connector, one per callee: callees whose
/// connectors run near each other get different colors where there are
/// enough, so that one can tell neighboring connectors apart.
fn call_colors(conns: &[Conn], ends: &[Ends], lanes: &[f64]) -> Vec<usize> {
    const NEAR: f64 = 24.0;
    let calls: Vec<usize> = (0..conns.len())
        .filter(|&i| conns[i].class.split(' ').next() == Some("call"))
        .collect();
    let boxes: HashMap<usize, (f64, f64, f64, f64)> = calls
        .iter()
        .map(|&i| (i, conn_box(&conns[i], ends[i], lanes[i])))
        .collect();
    let near = |i: usize, j: usize| {
        let (a, b) = (boxes[&i], boxes[&j]);
        a.0 <= b.2 + NEAR && b.0 <= a.2 + NEAR && a.1 <= b.3 + NEAR && b.1 <= a.3 + NEAR
    };
    // Callees in reading order, top to bottom.
    let mut targets: Vec<(u32, Rect)> = calls.iter().map(|&i| conns[i].b).collect();
    targets.sort_by(|x, y| x.1.y.total_cmp(&y.1.y).then(x.1.x.total_cmp(&y.1.x)));
    targets.dedup_by_key(|t| t.0);
    let mut color: HashMap<u32, usize> = HashMap::new();
    for (t, _) in targets {
        let mine: Vec<usize> = calls
            .iter()
            .copied()
            .filter(|&i| conns[i].b.0 == t)
            .collect();
        let mut taken = [0usize; CALL_COLORS];
        for &j in &calls {
            if let Some(&cj) = color.get(&conns[j].b.0)
                && mine.iter().any(|&i| near(i, j))
            {
                taken[cj] += 1;
            }
        }
        // The first free color, or else the one used least nearby.
        let pick = (0..CALL_COLORS).min_by_key(|&k| (taken[k], k)).unwrap_or(0);
        color.insert(t, pick);
    }
    conns
        .iter()
        .map(|c| color.get(&c.b.0).copied().unwrap_or(0))
        .collect()
}

/// Draws call, use, member and link connectors between visible cards.
pub fn draw_edges(app: &App) {
    let d = &app.d;
    let c = &app.s.canvas;
    let vis: Vec<&Group> = c.groups.iter().filter(|g| visible(d, c, g)).collect();
    let rects: HashMap<u32, Rect> = vis
        .iter()
        .filter_map(|g| Some((g.gid, card_rect(g)?)))
        .collect();
    let mut seen: BTreeSet<(u32, u32)> = BTreeSet::new();
    let mut conns: Vec<Conn> = Vec::new();
    let k = c.pan.k;
    let mut add = |ga: &Group, gb: &Group, class: &str, title: String| {
        let base = class.split(' ').next().unwrap_or("");
        let reverse_seen = base == "link" && seen.contains(&(gb.gid, ga.gid));
        if ga.gid == gb.gid || seen.contains(&(ga.gid, gb.gid)) || reverse_seen {
            return;
        }
        let (Some(&a), Some(&b)) = (rects.get(&ga.gid), rects.get(&gb.gid)) else {
            return;
        };
        seen.insert((ga.gid, gb.gid));
        let mut sites = match base {
            "call" => call_site_ys(ga, &gb.active, k),
            _ => Vec::new(),
        };
        let site_y = (!sites.is_empty()).then(|| sites.remove(0));
        conns.push(Conn {
            a: (ga.gid, a),
            b: (gb.gid, b),
            class: class.to_string(),
            title,
            site_y,
            more_sites: sites,
            route: choose_route(a, b, site_y),
        });
    };
    for ga in &vis {
        let Some(a) = d.item(&ga.active) else {
            continue;
        };
        for gb in &vis {
            if let Some(call) = a.info.calls.iter().find(|c| c.to == gb.active) {
                let _ = call;
                let class = "call";
                add(
                    ga,
                    gb,
                    class,
                    format!(
                        "{} calls {}",
                        d.display_name(a),
                        d.display_name_of(&gb.active)
                    ),
                );
            }
        }
    }
    for ga in &vis {
        let Some(a) = d.item(&ga.active) else {
            continue;
        };
        for gb in &vis {
            if let Some(u) = a.info.uses.iter().find(|u| u.to == gb.active) {
                add(
                    ga,
                    gb,
                    "use",
                    format!(
                        "{} uses {} ({:?})",
                        d.display_name(a),
                        d.display_name_of(&gb.active),
                        u.ctx
                    ),
                );
            }
        }
    }
    for ga in &vis {
        for gb in &vis {
            if d.item(&gb.active).and_then(|b| b.info.parent.as_deref()) == Some(ga.active.as_str())
            {
                add(
                    ga,
                    gb,
                    "member",
                    format!(
                        "{} belongs to {}",
                        d.display_name_of(&gb.active),
                        d.display_name_of(&ga.active)
                    ),
                );
            }
        }
    }
    for (from, to) in &c.links {
        let (Some(ia), Some(ib)) = (c.group_of(from), c.group_of(to)) else {
            continue;
        };
        let (ga, gb) = (&c.groups[ia], &c.groups[ib]);
        if visible(d, c, ga) && visible(d, c, gb) {
            add(ga, gb, "link", "opened from here".into());
        }
    }
    let ends = ports(&conns);
    let lanes = lanes(&conns, &ends);
    let colors = call_colors(&conns, &ends, &lanes);
    let ids: HashMap<u32, &str> = vis.iter().map(|g| (g.gid, g.active.as_str())).collect();
    mark_callees(&conns, &colors, &ids);
    let paths: Vec<Paths> = conns
        .iter()
        .enumerate()
        .map(|(i, c)| conn_path(c, ends[i], lanes[i]))
        .collect();
    let lines: Vec<&[Vec<(f64, f64)>]> = paths.iter().map(|p| p.lines.as_slice()).collect();
    let over = over_longer(&lines);
    // Those drawn over others last.
    let mut order: Vec<usize> = (0..conns.len()).collect();
    order.sort_by_key(|&i| over[i]);
    let mut out = String::new();
    for i in order {
        let c = &conns[i];
        let base = c.class.split(' ').next().unwrap_or("");
        let (mut class, marker) = match base {
            "call" => (
                format!("{} c{}", c.class, colors[i]),
                format!(r#"marker-end="url(#arrow-call-{})""#, colors[i]),
            ),
            "use" => (c.class.clone(), r#"marker-end="url(#arrow-use)""#.into()),
            _ => (c.class.clone(), String::new()),
        };
        let paths = &paths[i];
        if over[i] {
            class.push_str(" over");
            // A gap in the connectors below, along this one.
            out.push_str(&format!(
                r#"<path class="edge-gap" d="{} {}"/>"#,
                paths.branches, paths.main
            ));
        }
        let title = esc(&c.title);
        if !paths.branches.is_empty() {
            out.push_str(&format!(
                r#"<path class="edge {class} branch" d="{}"><title>{title}</title></path>"#,
                paths.branches
            ));
        }
        out.push_str(&format!(
            r#"<path class="edge {class}" d="{}" {marker}><title>{title}</title></path>"#,
            paths.main
        ));
        for &(x, y) in &paths.joins {
            out.push_str(&format!(
                r#"<circle class="join c{}" cx="{x:.1}" cy="{y:.1}" r="3"/>"#,
                colors[i]
            ));
        }
    }
    let call_markers: String = (0..CALL_COLORS)
        .map(|k| {
            format!(
                r#"<marker id="arrow-call-{k}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="var(--call-{k})"/></marker>"#
            )
        })
        .collect();
    dom::set_html(
        "edges",
        &format!(
            r#"<defs>{call_markers}
            <marker id="arrow-use" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="var(--type)"/></marker>
            </defs>{out}"#
        ),
    );
    draw_frames(app, &rects);
}

/// Gives each callee card, and the names that call it in the source of
/// its callers' cards, the color of the connectors that reach it.
fn mark_callees(conns: &[Conn], colors: &[usize], ids: &HashMap<u32, &str>) {
    for el in dom::query_all("#cards [data-callc]") {
        let _ = el.remove_attribute("data-callc");
    }
    for (c, &color) in conns.iter().zip(colors) {
        if c.class.split(' ').next() != Some("call") {
            continue;
        }
        let color = color.to_string();
        let (Some(caller), Some(callee)) = (card_el(c.a.0), card_el(c.b.0)) else {
            continue;
        };
        let _ = callee.set_attribute("data-callc", &color);
        let Some(&id) = ids.get(&c.b.0) else {
            continue;
        };
        for a in dom::query_all_in(&caller, "a.ref[data-id]") {
            if dom::data(&a, "id").as_deref() == Some(id) {
                let _ = a.set_attribute("data-callc", &color);
            }
        }
    }
}

/// Module boundaries around the cards they contain, with a private zone.
fn draw_frames(app: &App, rects: &HashMap<u32, Rect>) {
    let d = &app.d;
    let c = &app.s.canvas;
    if !c.frames {
        dom::set_html("frames", "");
        return;
    }
    #[derive(Default)]
    struct Entry {
        public: Vec<Rect>,
        private: Vec<Rect>,
        frames: Vec<Rect>,
    }
    let mut by_mod: HashMap<String, Entry> = HashMap::new();
    for g in c.groups.iter().filter(|g| visible(d, c, g)) {
        let Some(it) = d.item(&g.active) else {
            continue;
        };
        let m = d.mod_of(it).to_string();
        for anc in d.mod_chain(&m) {
            by_mod.entry(anc).or_default();
        }
        let Some(&r) = rects.get(&g.gid) else {
            continue;
        };
        // Items outside the analyzed crates have no module frame.
        let Some(e) = by_mod.get_mut(&m) else {
            continue;
        };
        if d.private_to(it, &m) {
            e.private.push(r)
        } else {
            e.public.push(r)
        }
    }
    let union = |rs: &[Rect], pad: f64| -> Option<Rect> {
        if rs.is_empty() {
            return None;
        }
        let x1 = rs.iter().map(|r| r.x).fold(f64::MAX, f64::min) - pad;
        let y1 = rs.iter().map(|r| r.y).fold(f64::MAX, f64::min) - pad;
        let x2 = rs.iter().map(|r| r.x + r.w).fold(f64::MIN, f64::max) + pad;
        let y2 = rs.iter().map(|r| r.y + r.h).fold(f64::MIN, f64::max) + pad;
        Some(Rect {
            x: x1,
            y: y1,
            w: x2 - x1,
            h: y2 - y1,
        })
    };
    let mut mods: Vec<String> = by_mod.keys().cloned().collect();
    // Innermost first, so parents can enclose their children's frames.
    mods.sort_by_key(|m| std::cmp::Reverse(d.mod_depth(m)));
    let mut out: Vec<String> = Vec::new();
    for m in mods {
        let e = by_mod.remove(&m).unwrap();
        let private_box = union(&e.private, 14.0);
        let mut inner = e.public.clone();
        inner.extend(private_box);
        inner.extend(e.frames.iter().copied());
        let Some(mut b) = union(&inner, 22.0) else {
            continue;
        };
        let extra = if private_box.is_some() && e.public.is_empty() {
            26.0
        } else {
            12.0
        };
        b.y -= extra;
        b.h += extra;
        let Some(it) = d.item(&m) else { continue };
        if !d.is_root(it)
            && let Some(parent) = by_mod.get_mut(&it.info.module)
        {
            parent.frames.push(b);
        }
        let mut s = format!(
            r#"<g class="frame"><rect class="frame-bg" x="{x}" y="{y}" width="{w}" height="{h}" rx="12"/><rect x="{x}" y="{y}" width="{w}" height="{h}" rx="12"/><rect class="frame-grip" data-frame="{m}" x="{x}" y="{y}" width="{w}" height="{h}" rx="12"><title>Drag to move this module's cards</title></rect><text data-frame="{m}" x="{}" y="{}">{}{}</text>"#,
            b.x + 12.0,
            b.y + 20.0,
            esc(&d.mod_label(&m)),
            if it.info.vis_scope.is_some() {
                format!("  ({})", esc(&it.info.vis_label))
            } else {
                String::new()
            },
            m = esc(&m),
            x = b.x,
            y = b.y,
            w = b.w,
            h = b.h
        );
        if let Some(p) = private_box {
            s.push_str(&format!(
                r#"<rect class="priv" x="{}" y="{}" width="{}" height="{}" rx="8"/><text class="priv" x="{}" y="{}">private to {}</text>"#,
                p.x,
                p.y,
                p.w,
                p.h,
                p.x + 8.0,
                p.y - 4.0,
                esc(&it.info.name)
            ));
        }
        s.push_str("</g>");
        out.push(s);
    }
    // Outer frames first so inner ones are drawn on top.
    out.reverse();
    dom::set_html("frames", &out.concat());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, h: f64) -> Rect {
        Rect { x, y, w: 580.0, h }
    }

    fn conn(a: (u32, Rect), b: (u32, Rect)) -> Conn {
        Conn {
            a,
            b,
            class: "use".into(),
            title: String::new(),
            site_y: None,
            more_sites: Vec::new(),
            route: choose_route(a.1, b.1, None),
        }
    }

    /// Two cards in the left column connected to one card in the right
    /// column, like an arranged module: each connector runs in its own
    /// lane and ends at its own point on the shared card.
    #[test]
    fn connectors_through_one_gap_stay_apart() {
        let top = (1, rect(0.0, 0.0, 400.0));
        let low = (2, rect(0.0, 1000.0, 300.0));
        let right = (3, rect(660.0, 500.0, 300.0));
        let conns = vec![conn(top, right), conn(low, right)];
        let ends = ports(&conns);
        let lanes = lanes(&conns, &ends);
        assert_ne!(ends[0].1, ends[1].1);
        assert_ne!(lanes[0], lanes[1]);
        for l in &lanes {
            assert!(*l > 580.0 && *l < 660.0, "lane {l} outside the gap");
        }
        // The lower connector enters the right card lower down.
        assert!(ends[0].1.1 < ends[1].1.1);
    }

    /// A call from a card's source to a card below it in the same column
    /// leaves at the call, runs down right of both cards and enters the
    /// callee's right side, instead of looping into its top.
    #[test]
    fn call_to_a_card_below_runs_beside_the_column() {
        let a = (1, rect(0.0, 0.0, 600.0));
        let b = (2, rect(20.0, 650.0, 700.0));
        let site = 350.0;
        let c = Conn {
            site_y: Some(site),
            route: choose_route(a.1, b.1, Some(site)),
            ..conn(a, b)
        };
        assert!(matches!(c.route, Route::Ortho(Side::Right, Side::Right)));
        let conns = vec![c];
        let ends = ports(&conns);
        let lanes = lanes(&conns, &ends);
        assert_eq!(ends[0].0, (580.0, site));
        assert_eq!(ends[0].1, (600.0, 690.0));
        assert_eq!(lanes[0], 600.0 + BESIDE);
    }

    /// A call into a card just below the caller, closer than `MIN_GAP`,
    /// still runs beside the column rather than in a curve from the side
    /// of the caller facing the callee into the callee's top, which ran
    /// behind the caller.
    #[test]
    fn call_to_a_card_just_below_does_not_run_behind_the_caller() {
        let a = rect(0.0, 0.0, 600.0);
        for b in [
            rect(-20.0, 605.0, 300.0),
            rect(20.0, 600.0, 300.0),
            Rect {
                w: 900.0,
                ..rect(-200.0, 610.0, 300.0)
            },
        ] {
            assert!(matches!(
                choose_route(a, b, Some(350.0)),
                Route::Ortho(Side::Right, Side::Right)
            ));
        }
        // Beside, however close.
        assert!(matches!(
            choose_route(a, rect(585.0, 300.0, 300.0), Some(350.0)),
            Route::Ortho(Side::Right, Side::Left)
        ));
        assert!(matches!(
            choose_route(a, rect(-585.0, 300.0, 300.0), Some(350.0)),
            Route::Ortho(Side::Left, Side::Right)
        ));
    }

    /// A connector between cards of one column that skips a card bends
    /// just below the upper card, not behind the card in between.
    #[test]
    fn vertical_connector_bends_next_to_a_card() {
        let top = (1, rect(0.0, 0.0, 400.0));
        let low = (2, rect(0.0, 1000.0, 300.0));
        let conns = vec![conn(low, top), conn((3, rect(0.0, 440.0, 520.0)), top)];
        let ends = ports(&conns);
        let lanes = lanes(&conns, &ends);
        assert!(lanes[0] > 400.0 && lanes[0] < 440.0, "lane {}", lanes[0]);
    }

    /// A card that calls another one from two lines gets a connector from
    /// each line; the second joins the first in its lane, so that both end
    /// in one arrow head on the callee.
    #[test]
    fn every_call_site_gets_a_connector() {
        let a = (1, rect(0.0, 0.0, 600.0));
        let b = (2, rect(660.0, 100.0, 300.0));
        let mut c = conn(a, b);
        c.class = "call".into();
        c.site_y = Some(120.0);
        c.more_sites = vec![480.0];
        c.route = choose_route(a.1, b.1, c.site_y);
        let conns = vec![c];
        let ends = ports(&conns);
        let lanes = lanes(&conns, &ends);
        let paths = conn_path(&conns[0], ends[0], lanes[0]);
        let (q, lane) = (ends[0].1, lanes[0]);
        // The main connector starts at the first call and ends at `b`.
        assert!(paths.main.starts_with("M580.0,120.0"), "{}", paths.main);
        let end = format!("L{:.1},{:.1}", q.0, q.1);
        assert!(paths.main.ends_with(&end), "{}", paths.main);
        // The second one runs into the main one's lane and ends there.
        let b = &paths.branches;
        assert_eq!(b.matches('M').count(), 1, "{b}");
        assert!(b.starts_with("M580.0,480.0"), "{b}");
        assert_eq!(paths.joins.len(), 1);
        assert_eq!(paths.joins[0].0, lane);
        assert!(paths.joins[0].1 > 120.0 && paths.joins[0].1 < q.1.max(480.0));
    }

    /// A short connector that crosses a long one is drawn over it.
    #[test]
    fn short_connectors_cross_over_long_ones() {
        let long = vec![vec![(0.0, 0.0), (50.0, 0.0), (50.0, 500.0), (100.0, 500.0)]];
        let short = vec![vec![
            (0.0, 200.0),
            (40.0, 200.0),
            (40.0, 100.0),
            (100.0, 100.0),
        ]];
        let over = over_longer(&[long.as_slice(), short.as_slice()]);
        assert_eq!(over, vec![false, true]);
    }

    /// Calls into two callees whose connectors run through one gap get
    /// different colors; a third, far away, may reuse one.
    #[test]
    fn neighboring_call_connectors_differ_in_color() {
        let a = (1, rect(0.0, 0.0, 600.0));
        let call = |b: (u32, Rect), y: f64| {
            let mut c = conn(a, b);
            c.class = "call".into();
            c.site_y = Some(y);
            c.route = choose_route(a.1, b.1, c.site_y);
            c
        };
        let far = (
            9,
            Rect {
                x: 5000.0,
                y: 5000.0,
                w: 100.0,
                h: 100.0,
            },
        );
        let conns = vec![
            // Crossing in the gap: down from the first call, up from the
            // second.
            call((2, rect(660.0, 400.0, 300.0)), 100.0),
            call((3, rect(660.0, 0.0, 300.0)), 200.0),
            call(far, 300.0),
        ];
        let ends = ports(&conns);
        let lanes = lanes(&conns, &ends);
        let colors = call_colors(&conns, &ends, &lanes);
        assert_ne!(colors[0], colors[1]);
    }
}
