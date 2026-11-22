//! Orthogonal edge routing around boxes, for the module map.
//!
//! Edges leave and enter boxes at right angles, at ports spread along the
//! box sides, and run on a grid around the boxes (never through them). A
//! shortest-path search on the grid prefers few bends and avoids cells
//! that earlier edges already use, so that parallel edges stay apart.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

/// x, y, width, height
pub type Rect = (f64, f64, f64, f64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

impl Side {
    /// Outward normal.
    fn normal(self) -> (f64, f64) {
        match self {
            Side::Top => (0.0, -1.0),
            Side::Bottom => (0.0, 1.0),
            Side::Left => (-1.0, 0.0),
            Side::Right => (1.0, 0.0),
        }
    }

    /// Grid direction of leaving through this side.
    fn dir(self) -> usize {
        match self {
            Side::Right => 0,
            Side::Bottom => 1,
            Side::Left => 2,
            Side::Top => 3,
        }
    }
}

const STEPS: [(i64, i64); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];

/// Grid cell size in pixels.
const G: f64 = 8.0;
/// Free space kept around boxes.
const MARGIN: f64 = 10.0;
/// Distance between neighboring ports on one side.
const PORT_GAP: f64 = 2.0 * G;

/// Which sides of which boxes edge `i` uses: (from box, side, to box, side).
fn sides(rects: &[Rect], a: usize, b: usize) -> (Side, Side) {
    let (ra, rb) = (rects[a], rects[b]);
    let below = rb.1 >= ra.1 + ra.3 - 1.0;
    let above = rb.1 + rb.3 <= ra.1 + 1.0;
    if below {
        (Side::Bottom, Side::Top)
    } else if above {
        (Side::Top, Side::Bottom)
    } else if rb.0 >= ra.0 + ra.2 / 2.0 {
        (Side::Right, Side::Left)
    } else {
        (Side::Left, Side::Right)
    }
}

/// A point on a box side, at `offset` from the side's middle.
fn port(r: Rect, side: Side, offset: f64) -> (f64, f64) {
    let (cx, cy) = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
    let snap = |v: f64| (v / G).floor() * G + G / 2.0;
    match side {
        Side::Top => (snap(cx + offset), r.1),
        Side::Bottom => (snap(cx + offset), r.1 + r.3),
        Side::Left => (r.0, snap(cy + offset)),
        Side::Right => (r.0 + r.2, snap(cy + offset)),
    }
}

struct Grid {
    w: usize,
    h: usize,
    blocked: Vec<bool>,
    used: Vec<u8>,
}

impl Grid {
    fn new(width: f64, height: f64, rects: &[Rect]) -> Grid {
        let (w, h) = (
            (width / G).ceil() as usize + 1,
            (height / G).ceil() as usize + 1,
        );
        let mut blocked = vec![false; w * h];
        for r in rects {
            let x0 = ((r.0 - MARGIN) / G).floor().max(0.0) as usize;
            let y0 = ((r.1 - MARGIN) / G).floor().max(0.0) as usize;
            let x1 = (((r.0 + r.2 + MARGIN) / G).ceil() as usize).min(w);
            let y1 = (((r.1 + r.3 + MARGIN) / G).ceil() as usize).min(h);
            for y in y0..y1 {
                for x in x0..x1 {
                    blocked[y * w + x] = true;
                }
            }
        }
        Grid {
            w,
            h,
            blocked,
            used: vec![0; w * h],
        }
    }

    fn cell(&self, p: (f64, f64)) -> Option<(usize, usize)> {
        let (x, y) = ((p.0 / G).floor(), (p.1 / G).floor());
        (x >= 0.0 && y >= 0.0 && (x as usize) < self.w && (y as usize) < self.h)
            .then_some((x as usize, y as usize))
    }

    fn free(&self, c: (usize, usize)) -> bool {
        !self.blocked[c.1 * self.w + c.0]
    }

    /// The first free cell outward from a port.
    fn exit(&self, p: (f64, f64), side: Side) -> Option<(usize, usize)> {
        let n = side.normal();
        (1..12)
            .map(|k| (p.0 + n.0 * G * k as f64, p.1 + n.1 * G * k as f64))
            .filter_map(|q| self.cell(q))
            .find(|&c| self.free(c))
    }

    /// Cheapest path from `s` (leaving in direction `sd`) to `t` (arriving
    /// in direction `td`): steps cost 10, bends 40, cells used by earlier
    /// edges a little more.
    fn search(
        &self,
        s: (usize, usize),
        sd: usize,
        t: (usize, usize),
        td: usize,
    ) -> Option<Vec<(usize, usize)>> {
        const STEP: u32 = 10;
        const BEND: u32 = 40;
        const CROWD: u32 = 6;
        let n = self.w * self.h * 4;
        let mut cost = vec![u32::MAX; n];
        let mut prev = vec![usize::MAX; n];
        let idx = |c: (usize, usize), d: usize| (c.1 * self.w + c.0) * 4 + d;
        let h = |c: (usize, usize)| (c.0.abs_diff(t.0) + c.1.abs_diff(t.1)) as u32 * STEP;
        let mut heap = BinaryHeap::new();
        let start = idx(s, sd);
        cost[start] = 0;
        heap.push(Reverse((h(s), start)));
        while let Some(Reverse((_, i))) = heap.pop() {
            let (cell, d) = (i / 4, i % 4);
            let c = (cell % self.w, cell / self.w);
            if c == t {
                if (d + 2) % 4 == td {
                    // Would arrive backwards.
                    continue;
                }
                let mut path = vec![c];
                let mut j = i;
                while prev[j] != usize::MAX {
                    j = prev[j];
                    let cell = j / 4;
                    path.push((cell % self.w, cell / self.w));
                }
                path.reverse();
                path.dedup();
                return Some(path);
            }
            for (nd, (dx, dy)) in STEPS.iter().enumerate() {
                if nd == (d + 2) % 4 {
                    continue;
                }
                let (nx, ny) = (c.0 as i64 + dx, c.1 as i64 + dy);
                if nx < 0 || ny < 0 || nx as usize >= self.w || ny as usize >= self.h {
                    continue;
                }
                let nc = (nx as usize, ny as usize);
                if !self.free(nc) {
                    continue;
                }
                let mut step = STEP + CROWD * self.used[ny as usize * self.w + nx as usize] as u32;
                if nd != d {
                    step += BEND;
                }
                if nc == t && nd != td {
                    step += BEND;
                }
                let j = idx(nc, nd);
                let new = cost[i].saturating_add(step);
                if new < cost[j] {
                    cost[j] = new;
                    prev[j] = i;
                    heap.push(Reverse((new + h(nc), j)));
                }
            }
        }
        None
    }

    fn mark(&mut self, path: &[(usize, usize)]) {
        for c in path {
            let u = &mut self.used[c.1 * self.w + c.0];
            *u = u.saturating_add(1);
        }
    }
}

/// Removes points in the middle of straight runs.
fn simplify(points: Vec<(f64, f64)>) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    for p in points {
        if out.last() == Some(&p) {
            continue;
        }
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            let straight = (a.0 == b.0 && b.0 == p.0) || (a.1 == b.1 && b.1 == p.1);
            if straight {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// Routes `edges` (indices into `rects`) in an area of `width` × `height`.
/// Returns one polyline per edge, from a port of the first box to a port
/// of the second, or `None` where no route exists.
/// `avoid` are lines that edges should not run along (they may cross
/// them), such as the lines between a module and its submodules.
pub fn route(
    width: f64,
    height: f64,
    rects: &[Rect],
    edges: &[(usize, usize)],
    avoid: &[Vec<(f64, f64)>],
) -> Vec<Option<Vec<(f64, f64)>>> {
    // Sides first, then spread the ports of each side, ordered by where
    // the edge goes, so that edges leaving one side don't cross there.
    let chosen: Vec<(Side, Side)> = edges.iter().map(|&(a, b)| sides(rects, a, b)).collect();
    // Per box side: (where the other end is, edge, whether it starts here).
    type Slots = BTreeMap<(usize, Side), Vec<(f64, usize, bool)>>;
    let mut slots: Slots = BTreeMap::new();
    for (i, (&(a, b), &(sa, sb))) in edges.iter().zip(&chosen).enumerate() {
        let along = |r: Rect, side: Side| match side {
            Side::Top | Side::Bottom => r.0 + r.2 / 2.0,
            Side::Left | Side::Right => r.1 + r.3 / 2.0,
        };
        slots
            .entry((a, sa))
            .or_default()
            .push((along(rects[b], sa), i, true));
        slots
            .entry((b, sb))
            .or_default()
            .push((along(rects[a], sb), i, false));
    }
    let mut offset = vec![(0.0, 0.0); edges.len()];
    for list in slots.values_mut() {
        list.sort_by(|x, y| x.0.total_cmp(&y.0));
        let n = list.len() as f64;
        for (k, &(_, i, from)) in list.iter().enumerate() {
            // Not the middle: tree lines use it.
            let o = (k as f64 - (n - 1.0) / 2.0) * PORT_GAP + PORT_GAP / 2.0;
            if from {
                offset[i].0 = o;
            } else {
                offset[i].1 = o;
            }
        }
    }
    let mut grid = Grid::new(width, height, rects);
    for line in avoid {
        for w in line.windows(2) {
            let steps = ((w[1].0 - w[0].0).abs() + (w[1].1 - w[0].1).abs()) / (G / 2.0);
            for k in 0..=steps.ceil() as usize {
                let t = k as f64 / steps.max(1.0);
                let p = (
                    w[0].0 + (w[1].0 - w[0].0) * t,
                    w[0].1 + (w[1].1 - w[0].1) * t,
                );
                if let Some(c) = grid.cell(p) {
                    grid.used[c.1 * grid.w + c.0] = 3;
                }
            }
        }
    }
    // Short edges first: they have the fewest alternatives.
    let mut order: Vec<usize> = (0..edges.len()).collect();
    let dist = |i: usize| {
        let (a, b) = (rects[edges[i].0], rects[edges[i].1]);
        ((a.0 - b.0).abs() + (a.1 - b.1).abs()) as i64
    };
    order.sort_by_key(|&i| dist(i));
    let mut out = vec![None; edges.len()];
    for i in order {
        let (a, b) = edges[i];
        let (sa, sb) = chosen[i];
        let p = port(rects[a], sa, offset[i].0);
        let q = port(rects[b], sb, offset[i].1);
        let (Some(s), Some(t)) = (grid.exit(p, sa), grid.exit(q, sb)) else {
            continue;
        };
        // Arrive moving into the target box: against its side's normal.
        let td = (sb.dir() + 2) % 4;
        let Some(cells) = grid.search(s, sa.dir(), t, td) else {
            continue;
        };
        grid.mark(&cells);
        let center = |c: &(usize, usize)| (c.0 as f64 * G + G / 2.0, c.1 as f64 * G + G / 2.0);
        let mut points = vec![p];
        points.extend(cells.iter().map(center));
        points.push(q);
        out[i] = Some(simplify(points));
    }
    out
}

/// An SVG path through `points` with rounded corners.
pub fn path_d(points: &[(f64, f64)]) -> String {
    const R: f64 = 8.0;
    let mut d = String::new();
    for (k, p) in points.iter().enumerate() {
        if k == 0 {
            d.push_str(&format!("M{:.1},{:.1}", p.0, p.1));
            continue;
        }
        if k + 1 == points.len() {
            d.push_str(&format!(" L{:.1},{:.1}", p.0, p.1));
            continue;
        }
        let (a, n) = (points[k - 1], points[k + 1]);
        let len = |u: (f64, f64), v: (f64, f64)| ((u.0 - v.0).powi(2) + (u.1 - v.1).powi(2)).sqrt();
        let r = R.min(len(a, *p) / 2.0).min(len(*p, n) / 2.0);
        let towards = |from: (f64, f64), to: (f64, f64), dist: f64| {
            let l = len(from, to).max(1e-9);
            (
                from.0 + (to.0 - from.0) / l * dist,
                from.1 + (to.1 - from.1) / l * dist,
            )
        };
        let (b1, b2) = (towards(*p, a, r), towards(*p, n, r));
        d.push_str(&format!(
            " L{:.1},{:.1} Q{:.1},{:.1} {:.1},{:.1}",
            b1.0, b1.1, p.0, p.1, b2.0, b2.1
        ));
    }
    d
}

/// Where to put an edge's label: the middle of its longest segment.
pub fn label_at(points: &[(f64, f64)]) -> (f64, f64) {
    points
        .windows(2)
        .max_by(|x, y| {
            let l = |w: &[(f64, f64)]| (w[0].0 - w[1].0).abs() + (w[0].1 - w[1].1).abs();
            l(x).total_cmp(&l(y))
        })
        .map_or((0.0, 0.0), |w| {
            ((w[0].0 + w[1].0) / 2.0, (w[0].1 + w[1].1) / 2.0)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(p: (f64, f64), r: Rect) -> bool {
        p.0 > r.0 + 0.5 && p.0 < r.0 + r.2 - 0.5 && p.1 > r.1 + 0.5 && p.1 < r.1 + r.3 - 0.5
    }

    /// Every segment is horizontal or vertical, and no part of it is
    /// inside a box.
    fn check(points: &[(f64, f64)], rects: &[Rect]) {
        for w in points.windows(2) {
            let (a, b) = (w[0], w[1]);
            assert!(a.0 == b.0 || a.1 == b.1, "diagonal {a:?} {b:?}");
            for k in 0..=20 {
                let t = k as f64 / 20.0;
                let p = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                for r in rects {
                    assert!(!inside(p, *r), "{p:?} inside {r:?}");
                }
            }
        }
    }

    #[test]
    fn routes_around_a_box_in_the_way() {
        // A on the left, C on the right, B between them.
        let rects = [
            (0.0, 100.0, 100.0, 60.0),
            (200.0, 80.0, 100.0, 100.0),
            (400.0, 100.0, 100.0, 60.0),
        ];
        let out = route(600.0, 400.0, &rects, &[(0, 2)], &[]);
        let points = out[0].as_ref().expect("routed");
        check(points, &rects);
        // Leaves A's right side and enters C's left side at right angles.
        assert_eq!(points[0].0, 100.0);
        assert_eq!(points.last().unwrap().0, 400.0);
        assert_eq!(points[0].1, points[1].1);
        let n = points.len();
        assert_eq!(points[n - 2].1, points[n - 1].1);
    }

    #[test]
    fn downward_edges_use_bottom_and_top_with_separate_ports() {
        let rects = [(100.0, 0.0, 200.0, 60.0), (100.0, 160.0, 200.0, 60.0)];
        let out = route(500.0, 300.0, &rects, &[(0, 1), (1, 0)], &[]);
        let (a, b) = (out[0].as_ref().unwrap(), out[1].as_ref().unwrap());
        check(a, &rects);
        check(b, &rects);
        // 0 → 1 leaves the bottom of 0; 1 → 0 leaves the top of 1.
        assert_eq!(a[0].1, 60.0);
        assert_eq!(b[0].1, 160.0);
        // The two edges don't share a port.
        assert_ne!(a[0], *b.last().unwrap());
    }

    #[test]
    fn rounded_path() {
        let d = path_d(&[(0.0, 0.0), (0.0, 50.0), (50.0, 50.0)]);
        assert_eq!(d, "M0.0,0.0 L0.0,42.0 Q0.0,50.0 8.0,50.0 L50.0,50.0");
        assert_eq!(
            label_at(&[(0.0, 0.0), (0.0, 10.0), (50.0, 10.0)]),
            (25.0, 10.0)
        );
    }
}
