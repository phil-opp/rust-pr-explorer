//! Connectors between cards, and frames around the cards of a module.

use super::{Group, card_el, visible};
use crate::code::esc;
use crate::{App, dom};
use std::collections::{BTreeSet, HashMap};

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
    let mut out = String::new();
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
        let marker = if base == "call" || base == "use" {
            format!(r#"marker-end="url(#arrow-{base})""#)
        } else {
            String::new()
        };
        out.push_str(&format!(
            r#"<path class="edge {class}" d="{}" {marker}><title>{}</title></path>"#,
            edge_path(a, b),
            esc(&title)
        ));
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
    dom::set_html(
        "edges",
        &format!(
            r#"<defs>
            <marker id="arrow-call" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="var(--fn)"/></marker>
            <marker id="arrow-use" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="var(--type)"/></marker>
            </defs>{out}"#
        ),
    );
    draw_frames(app, &rects);
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
            r#"<g class="frame"><rect class="frame-bg" x="{x}" y="{y}" width="{w}" height="{h}" rx="12"/><rect x="{x}" y="{y}" width="{w}" height="{h}" rx="12"/><text x="{}" y="{}">{}{}</text>"#,
            b.x + 12.0,
            b.y + 20.0,
            esc(&d.mod_label(&m)),
            if it.info.vis_scope.is_some() {
                format!("  ({})", esc(&it.info.vis_label))
            } else {
                String::new()
            },
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
