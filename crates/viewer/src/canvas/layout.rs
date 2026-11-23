//! Arranging cards: by module, fitting the view, and module graphs.

use super::{
    CARD_W, Canvas, Pan, apply_pan,
    edges::{Rect, card_rect, draw_edges},
    height_of, render, visible,
};
use crate::data::{Data, changed, within};
use crate::{App, View, dom, toast};
use model::{Item, Kind, Status};
use std::collections::{BTreeSet, HashMap};

/// Lays out the visible cards in nested module regions: exposed items on
/// top, private items below them, submodules underneath.
pub fn arrange_by_module(app: &mut App) {
    app.s.canvas.frames = true;
    render(app);
    let d = app.d.clone();
    let c = &mut app.s.canvas;
    let heights: HashMap<u32, f64> = c.groups.iter().map(|g| (g.gid, height_of(&d, g))).collect();
    let mut by_mod: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, g) in c.groups.iter().enumerate() {
        if visible(&d, c, g)
            && let Some(it) = d.item(&g.active)
        {
            by_mod.entry(d.mod_of(it).to_string()).or_default().push(i);
        }
    }
    const COLS: usize = 2;
    const GAP: f64 = 40.0;
    struct Layout<'a> {
        d: &'a Data,
        c: &'a mut Canvas,
        by_mod: &'a HashMap<String, Vec<usize>>,
        heights: &'a HashMap<u32, f64>,
    }
    impl Layout<'_> {
        fn has_cards(&self, m: &str) -> bool {
            self.by_mod.keys().any(|k| within(k, m))
        }
        fn row(&mut self, mut list: Vec<usize>, x: f64, cy: &mut f64, w: &mut f64) {
            let d = self.d;
            let depth = call_depths(d, self.c, &list);
            list.sort_by(|&a, &b| {
                let (ia, ib) = (
                    d.item(&self.c.groups[a].active),
                    d.item(&self.c.groups[b].active),
                );
                let (ca, cb) = (ia.is_some_and(changed), ib.is_some_and(changed));
                // Callers before (left of and above) what they call, then
                // changed items first.
                depth[&a].cmp(&depth[&b]).then(cb.cmp(&ca)).then_with(|| {
                    let na = ia.map(|i| d.display_name(i)).unwrap_or_default();
                    let nb = ib.map(|i| d.display_name(i)).unwrap_or_default();
                    na.cmp(&nb)
                })
            });
            for row in list.chunks(COLS) {
                let mut rh: f64 = 0.0;
                for (j, &gi) in row.iter().enumerate() {
                    let g = &mut self.c.groups[gi];
                    g.x = x + 24.0 + j as f64 * (CARD_W + GAP);
                    g.y = *cy;
                    rh = rh.max(self.heights.get(&g.gid).copied().unwrap_or(200.0));
                }
                *w = w.max(row.len() as f64 * (CARD_W + GAP));
                *cy += rh + GAP;
            }
        }
        fn layout(&mut self, m: &str, x: f64, y: f64) -> (f64, f64) {
            let mut cy = y + 36.0;
            let mut w: f64 = 0.0;
            let own = self.by_mod.get(m).cloned().unwrap_or_default();
            let (private, public): (Vec<usize>, Vec<usize>) = own.into_iter().partition(|&gi| {
                self.d
                    .item(&self.c.groups[gi].active)
                    .is_some_and(|it| self.d.private_to(it, m))
            });
            self.row(public, x, &mut cy, &mut w);
            if !private.is_empty() {
                cy += 20.0;
                self.row(private, x, &mut cy, &mut w);
            }
            let mut subs: Vec<String> = self
                .d
                .mod_children_items(m)
                .iter()
                .filter(|c| c.info.kind == Kind::Mod && self.has_cards(&c.info.id))
                .map(|c| c.info.id.clone())
                .collect();
            subs.sort();
            let mut sx = x + 24.0;
            let mut max_h: f64 = 0.0;
            for s in &subs {
                let (sw, sh) = self.layout(s, sx, cy);
                sx += sw + GAP;
                max_h = max_h.max(sh);
            }
            w = w.max(sx - x);
            cy += max_h + if subs.is_empty() { 0.0 } else { GAP };
            (w + 24.0, cy - y)
        }
    }
    let roots: Vec<String> = d.roots().to_vec();
    let mut layout = Layout {
        d: &d,
        c,
        by_mod: &by_mod,
        heights: &heights,
    };
    let mut x = 0.0;
    for r in roots {
        if layout.has_cards(&r) {
            let (w, _) = layout.layout(&r, x, 0.0);
            x += w + 80.0;
        }
    }
}

/// For each card of `list`, the length of the longest chain of calls that
/// leads to it from the other cards of `list`: 0 for a card none of them
/// calls. In a cycle, depths stop growing after `list.len()` rounds.
fn call_depths(d: &Data, c: &Canvas, list: &[usize]) -> HashMap<usize, usize> {
    let mut depth: HashMap<usize, usize> = list.iter().map(|&i| (i, 0)).collect();
    let edges: Vec<(usize, usize)> = list
        .iter()
        .flat_map(|&a| list.iter().map(move |&b| (a, b)))
        .filter(|&(a, b)| {
            a != b
                && d.item(&c.groups[a].active).is_some_and(|it| {
                    it.info
                        .calls
                        .iter()
                        .any(|call| call.to == c.groups[b].active)
                })
        })
        .collect();
    // Longest paths; at most `list.len()` rounds, which also ends cycles.
    for _ in 0..list.len() {
        let mut moved = false;
        for &(a, b) in &edges {
            let da = depth[&a];
            if depth[&b] < da + 1 && da + 1 < list.len() {
                depth.insert(b, da + 1);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    depth
}

pub fn fit_view() {
    dom::raf(|| {
        crate::with_app(|app| {
            let d = app.d.clone();
            let c = &mut app.s.canvas;
            let rects: Vec<Rect> = c
                .groups
                .iter()
                .filter(|g| visible(&d, c, g))
                .filter_map(card_rect)
                .collect();
            if rects.is_empty() {
                return;
            }
            let x1 = rects.iter().map(|r| r.x).fold(f64::MAX, f64::min) - 60.0;
            let y1 = rects.iter().map(|r| r.y).fold(f64::MAX, f64::min)
                - if c.frames { 110.0 } else { 60.0 };
            let x2 = rects.iter().map(|r| r.x + r.w).fold(f64::MIN, f64::max) + 60.0;
            let y2 = rects.iter().map(|r| r.y + r.h).fold(f64::MIN, f64::max) + 60.0;
            let Some(vp) = dom::by_id("viewport") else {
                return;
            };
            let r = vp.get_bounding_client_rect();
            let k = (r.width() / (x2 - x1))
                .min(r.height() / (y2 - y1))
                .clamp(0.25, 1.0);
            c.pan = Pan {
                k,
                x: -x1 * k + ((r.width() - (x2 - x1) * k) / 2.0).max(0.0),
                y: -y1 * k,
            };
            apply_pan(c);
            draw_edges(app);
        });
    });
}

/// Opens a module's changed items, and the unchanged items in it that
/// their changed lines call.
pub fn show_module_graph(app: &mut App, m: &str) {
    let d = app.d.clone();
    let own: Vec<&Item> = d
        .changes_in(m, false)
        .into_iter()
        .filter(|it| it.info.kind != Kind::Impl)
        .collect();
    if own.is_empty() {
        // Keep the user's cards.
        toast(&format!("No changed items directly in {}", d.mod_label(m)));
        return;
    }
    let mut ids: Vec<String> = own.iter().map(|it| it.info.id.clone()).collect();
    // Changed neighbours are among `own` already. Unchanged ones are added
    // only where a changed line calls them: callers are unchanged, so their
    // calls are too, and so are calls in unchanged parts of a changed body.
    for it in &own {
        for n in called_on_changed_lines(it) {
            if let Some(ni) = d.item(&n)
                && d.mod_of(ni) == m
                && ni.info.kind != Kind::Impl
                && !ids.contains(&n)
            {
                ids.push(n);
            }
        }
    }
    let c = &mut app.s.canvas;
    c.scope = m.to_string();
    c.groups.clear();
    c.links.clear();
    for id in &ids {
        c.add_group(id, 0.0, 0.0);
    }
    app.s.canvas_focus = None;
    crate::set_view(app, View::Canvas);
    arrange_by_module(app);
    render(app);
    fit_view();
}

/// The items that `it` calls on added or changed lines of its source.
fn called_on_changed_lines(it: &Item) -> BTreeSet<String> {
    if it.status == Status::Removed {
        // Calls are only known for the head version.
        return BTreeSet::new();
    }
    let calls: BTreeSet<&str> = it.info.calls.iter().map(|c| c.to.as_str()).collect();
    let mut line = it.info.line;
    let mut lines = BTreeSet::new();
    for l in &it.source {
        match l.0 {
            '-' => continue,
            '+' => {
                lines.insert(line);
            }
            _ => {}
        }
        line += 1;
    }
    it.info
        .source_refs
        .iter()
        .filter(|(pos, _)| {
            pos.split_once(':')
                .and_then(|(l, _)| l.parse::<usize>().ok())
                .is_some_and(|l| lines.contains(&l))
        })
        .filter_map(|(_, r)| r.id.as_deref())
        .filter(|id| calls.contains(id))
        .map(str::to_string)
        .collect()
}
