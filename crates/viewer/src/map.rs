//! Views of the module structure:
//!
//! - the *module map*: the module tree as nested boxes (parent above its
//!   children), each listing its changed items, exposed ones first, with
//!   one arrow per pair of modules whose items call each other;
//! - the *matrix*: modules against modules, each cell counting the calls
//!   from one module into another that involve a change.

use crate::code::esc;
use crate::data::{Data, changed};
use crate::{App, dom};
use model::{Item, Kind, Status};
use std::collections::{BTreeMap, BTreeSet};

/// Calls (and, optionally, uses) between items, grouped by the modules of
/// caller and callee. Only edges that touch a changed item are counted,
/// unless `all` is set.
/// A call or use: (from item, to item, is a call).
type Flow = (String, String, bool);

struct Flows {
    /// Flows by (from module, to module).
    pairs: BTreeMap<(String, String), Vec<Flow>>,
}

fn flows(d: &Data, all: bool, uses: bool) -> Flows {
    let mut pairs: BTreeMap<(String, String), Vec<Flow>> = BTreeMap::new();
    for it in d.items() {
        if matches!(it.info.kind, Kind::Mod | Kind::Impl) {
            continue;
        }
        let targets = it
            .info
            .calls
            .iter()
            .map(|c| (&c.to, true))
            .chain(it.info.uses.iter().filter(|_| uses).map(|u| (&u.to, false)));
        for (to, call) in targets {
            let Some(target) = d.item(to) else { continue };
            if matches!(target.info.kind, Kind::Mod | Kind::Impl) {
                continue;
            }
            if !(all || changed(it) || changed(target)) {
                continue;
            }
            pairs
                .entry((d.mod_of(it).to_string(), d.mod_of(target).to_string()))
                .or_default()
                .push((it.info.id.clone(), to.clone(), call));
        }
    }
    Flows { pairs }
}

/// The modules to show: those with shown items or flows, plus ancestors,
/// in tree order (depth first).
fn modules(d: &Data, items: &[&Item], f: &Flows) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    let mut add = |m: &str| {
        for a in d.mod_chain(m) {
            set.insert(a);
        }
    };
    for it in items {
        add(d.mod_of(it));
    }
    for (a, b) in f.pairs.keys() {
        add(a);
        add(b);
    }
    let mut out = Vec::new();
    fn walk(d: &Data, m: &str, set: &BTreeSet<String>, out: &mut Vec<String>) {
        if !set.contains(m) {
            return;
        }
        out.push(m.to_string());
        let mut kids: Vec<&Item> = d
            .mod_children_items(m)
            .into_iter()
            .filter(|k| k.info.kind == Kind::Mod)
            .collect();
        kids.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        for k in kids {
            walk(d, &k.info.id, set, out);
        }
    }
    for r in d.roots() {
        walk(d, r, &set, &mut out);
    }
    out
}

fn shown_items(d: &Data, all: bool) -> Vec<&Item> {
    d.items()
        .filter(|it| !matches!(it.info.kind, Kind::Mod | Kind::Impl) && (all || changed(it)))
        .collect()
}

fn chip(d: &Data, it: &Item) -> String {
    format!(
        r#"<a class="ref mm-chip {}" data-id="{}" data-kind="{}" data-path="{}"><span class="dot {}"></span><span class="nm" title="{}">{}</span></a>"#,
        it.status.css(),
        esc(&it.info.id),
        it.info.kind.css(),
        esc(&it.info.id),
        if it.status == Status::Unchanged && d.touched(it) {
            "changed"
        } else {
            it.status.css()
        },
        esc(&d.display_name(it)),
        esc(&d.display_name(it))
    )
}

/// The module map: the module tree as boxes, with flows drawn as arrows
/// once the boxes are laid out (see [`draw_map_edges`]).
pub fn render_map(app: &App) {
    let d = &app.d;
    let sp = &app.s.space;
    let items = shown_items(d, sp.show_unchanged);
    let f = flows(d, sp.show_unchanged, sp.show_uses);
    let mods = modules(d, &items, &f);
    let shown: BTreeSet<&str> = mods.iter().map(String::as_str).collect();

    fn node(d: &Data, m: &str, items: &[&Item], shown: &BTreeSet<&str>) -> String {
        let Some(module) = d.item(m) else {
            return String::new();
        };
        let mut mine: Vec<&&Item> = items.iter().filter(|it| d.mod_of(it) == m).collect();
        mine.sort_by_key(|it| d.display_name(it));
        let (exposed, private): (Vec<&&Item>, Vec<&&Item>) =
            mine.into_iter().partition(|it| !d.private_to(it, m));
        // Long lists are cut; the module page has them all.
        const MAX: usize = 8;
        let list = |v: &[&&Item]| {
            let mut html: String = v.iter().take(MAX).map(|it| chip(d, it)).collect();
            if v.len() > MAX {
                html.push_str(&format!(
                    r#"<a class="mm-more" data-mod="{}">+{} more</a>"#,
                    esc(m),
                    v.len() - MAX
                ));
            }
            html
        };
        let body = match (exposed.is_empty(), private.is_empty()) {
            (true, true) => r#"<div class="mm-none">no changes</div>"#.to_string(),
            _ => format!(
                r#"<div class="mm-items">{}</div>{}"#,
                list(&exposed),
                if private.is_empty() {
                    String::new()
                } else {
                    format!(
                        r#"<div class="mm-private" title="Not visible outside this module">{}</div>"#,
                        list(&private)
                    )
                }
            ),
        };
        let mut kids: Vec<&Item> = d
            .mod_children_items(m)
            .into_iter()
            .filter(|k| k.info.kind == Kind::Mod && shown.contains(k.info.id.as_str()))
            .collect();
        kids.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        let kids: String = kids
            .iter()
            .map(|k| node(d, &k.info.id, items, shown))
            .collect();
        let private_mod = module.info.vis_scope.is_some();
        format!(
            r#"<div class="mm-node"><div class="mm-box {} {}" data-mm="{}"><div class="mm-h"><span class="dot {}"></span><a data-mod="{}">{}</a>{}</div>{body}</div>{}</div>"#,
            if private_mod { "priv" } else { "" },
            module.status.css(),
            esc(m),
            if module.status == Status::Unchanged && d.touched(module) {
                "changed"
            } else {
                module.status.css()
            },
            esc(m),
            esc(&d.mod_label(m)),
            if private_mod {
                r#"<span class="vis">priv</span>"#
            } else {
                ""
            },
            if kids.is_empty() {
                String::new()
            } else {
                format!(r#"<div class="mm-kids">{kids}</div>"#)
            }
        )
    }
    let trees: String = d
        .roots()
        .iter()
        .filter(|r| shown.contains(r.as_str()))
        .map(|r| node(d, r, &items, &shown))
        .collect();
    let html = format!(
        r#"<div class="mm-wrap"><svg id="mm-edges"></svg><div class="mm-tree">{trees}</div></div>"#
    );
    dom::set_html("space-alt", &html);
    dom::raf(|| {
        crate::with_app(|app| draw_map_edges(app));
    });
}

/// Arrows between module boxes, one per pair of modules, with the number
/// of calls; and thin lines from each module to its submodules.
pub fn draw_map_edges(app: &App) {
    let d = &app.d;
    let sp = &app.s.space;
    let Some(wrap) = dom::query(".mm-wrap") else {
        return;
    };
    let origin = wrap.get_bounding_client_rect();
    let rect = |m: &str| {
        dom::query(&format!(r#".mm-box[data-mm="{}"]"#, m.replace('"', "")))
            .map(|e| e.get_bounding_client_rect())
            .map(|r| {
                (
                    r.left() - origin.left(),
                    r.top() - origin.top(),
                    r.width(),
                    r.height(),
                )
            })
    };
    let mut svg = String::from(
        r#"<defs><marker id="mm-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="9" markerHeight="9" markerUnits="userSpaceOnUse" orient="auto-start-reverse"><path d="M0,0 L10,5 L0,10 z" fill="currentColor"/></marker></defs>"#,
    );
    // Tree lines; arrows avoid running along them.
    let mut tree_lines: Vec<Vec<(f64, f64)>> = Vec::new();
    for it in d.items().filter(|it| it.info.kind == Kind::Mod) {
        if d.is_root(it) {
            continue;
        }
        if let (Some(p), Some(c)) = (rect(&it.info.module), rect(&it.info.id)) {
            let (x1, y1) = (p.0 + p.2 / 2.0, p.1 + p.3);
            let (x2, y2) = (c.0 + c.2 / 2.0, c.1);
            let my = (y1 + y2) / 2.0;
            tree_lines.push(vec![(x1, y1), (x1, my), (x2, my), (x2, y2)]);
            svg.push_str(&format!(
                r#"<path class="mm-tree-line" d="M{x1:.1},{y1:.1} V{my:.1} H{x2:.1} V{y2:.1}"/>"#
            ));
        }
    }
    // Flows between modules.
    let f = flows(d, sp.show_unchanged, sp.show_uses);
    let between = f.pairs.keys().filter(|(a, b)| a != b).count();
    if let Some(w) = dom::query(".mm-wrap") {
        // With many arrows, they stay faint until a module is hovered.
        dom::toggle_class(&w, "busy", between > 8);
    }
    // Every box is an obstacle; arrows run around them.
    let boxes: Vec<(String, crate::route::Rect)> = dom::query_all(".mm-box[data-mm]")
        .iter()
        .filter_map(|e| {
            let m = dom::data(e, "mm")?;
            rect(&m).map(|r| (m, r))
        })
        .collect();
    let index = |m: &str| boxes.iter().position(|(n, _)| n == m);
    type Pair<'a> = (&'a (String, String), &'a Vec<Flow>, (usize, usize));
    let pairs: Vec<Pair> = f
        .pairs
        .iter()
        .filter(|((a, b), _)| a != b)
        .filter_map(|(k, list)| Some((k, list, (index(&k.0)?, index(&k.1)?))))
        .collect();
    let rects: Vec<crate::route::Rect> = boxes.iter().map(|(_, r)| *r).collect();
    let edges: Vec<(usize, usize)> = pairs.iter().map(|p| p.2).collect();
    let routes = crate::route::route(
        wrap.scroll_width() as f64,
        wrap.scroll_height() as f64,
        &rects,
        &edges,
        &tree_lines,
    );
    for ((ab, list, _), points) in pairs.iter().zip(routes) {
        let (a, b) = (&ab.0, &ab.1);
        let Some(points) = points else { continue };
        let calls = list.iter().filter(|e| e.2).count();
        let uses = list.len() - calls;
        let label = match (calls, uses) {
            (c, 0) => format!("{c} call{}", if c == 1 { "" } else { "s" }),
            (0, u) => format!("{u} use{}", if u == 1 { "" } else { "s" }),
            (c, u) => format!("{c} calls · {u} uses"),
        };
        let title: String = list
            .iter()
            .map(|(x, y, call)| {
                format!(
                    "{} {} {}",
                    d.display_name_of(x),
                    if *call { "calls" } else { "uses" },
                    d.display_name_of(y)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let width = 1.2 + (list.len() as f64).ln_1p() * 0.8;
        let (lx, ly) = crate::route::label_at(&points);
        svg.push_str(&format!(
            r#"<g class="mm-flow" data-a="{}" data-b="{}"><title>{}</title><path d="{}" style="stroke-width:{width:.1}" marker-end="url(#mm-arrow)"/><text x="{lx:.1}" y="{ly:.1}">{}</text></g>"#,
            esc(a),
            esc(b),
            esc(&title),
            crate::route::path_d(&points),
            esc(&label)
        ));
    }
    if let Some(el) = dom::by_id("mm-edges") {
        dom::set_style(&el, "width", &format!("{}px", wrap.scroll_width()));
        dom::set_style(&el, "height", &format!("{}px", wrap.scroll_height()));
        el.set_inner_html(&svg);
    }
}

/// Highlights the flows of the hovered module box.
pub fn hover(el: &web_sys::Element) {
    let m = dom::closest(el, ".mm-box").and_then(|b| dom::data(&b, "mm"));
    for g in dom::query_all(".mm-flow") {
        let hot = m.as_deref().is_some_and(|m| {
            dom::data(&g, "a").as_deref() == Some(m) || dom::data(&g, "b").as_deref() == Some(m)
        });
        dom::toggle_class(&g, "hot", hot);
        dom::toggle_class(&g, "dim", m.is_some() && !hot);
    }
}

/// The matrix: rows call columns.
pub fn render_matrix(app: &App) {
    let d = &app.d;
    let sp = &app.s.space;
    let items = shown_items(d, sp.show_unchanged);
    let f = flows(d, sp.show_unchanged, sp.show_uses);
    let mods = modules(d, &items, &f);
    let max = f.pairs.values().map(Vec::len).max().unwrap_or(1).max(1) as f64;
    let changes = |m: &str| items.iter().filter(|it| d.mod_of(it) == m).count();
    let head: String = mods
        .iter()
        .enumerate()
        .map(|(i, m)| {
            format!(
                r#"<th class="mx-col" title="{}">{}</th>"#,
                esc(&d.mod_label(m)),
                i + 1
            )
        })
        .collect();
    let rows: String = mods
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let private = d.item(a).is_some_and(|m| m.info.vis_scope.is_some());
            let cells: String = mods
                .iter()
                .map(|b| {
                    let list = f.pairs.get(&(a.clone(), b.clone()));
                    let n = list.map_or(0, Vec::len);
                    let diag = if a == b { "diag" } else { "" };
                    if n == 0 {
                        return format!(r#"<td class="mx-cell {diag}"></td>"#);
                    }
                    let title: String = list
                        .into_iter()
                        .flatten()
                        .map(|(x, y, call)| {
                            format!(
                                "{} {} {}",
                                d.display_name_of(x),
                                if *call { "calls" } else { "uses" },
                                d.display_name_of(y)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let k = 15.0 + 55.0 * (n as f64).ln_1p() / max.ln_1p();
                    format!(
                        r#"<td class="mx-cell hit {diag}" data-mx-a="{}" data-mx-b="{}" title="{}" style="background:color-mix(in srgb, var(--link) {k:.0}%, transparent)">{n}</td>"#,
                        esc(a),
                        esc(b),
                        esc(&title)
                    )
                })
                .collect();
            let depth = d.mod_depth(a);
            format!(
                r#"<tr><th class="mx-row {}" style="padding-left:{}px"><span class="mx-i">{}</span> <a data-mod="{}">{}</a> <span class="dim">{}</span></th>{cells}</tr>"#,
                if private { "priv" } else { "" },
                8 + depth * 14,
                i + 1,
                esc(a),
                esc(&d.mod_label(a)),
                match changes(a) {
                    0 => String::new(),
                    n => format!("{n} changed"),
                }
            )
        })
        .collect();
    let html = format!(
        r#"<div class="mx-wrap"><table class="mx"><tr><th>{}</th>{head}</tr>{rows}</table></div>"#,
        crate::pages::help_html(&format!(
            "Changed calls{} from the row's module to the column's.",
            if sp.show_uses { " and uses" } else { "" }
        ))
    );
    dom::set_html("space-alt", &html);
}

/// Opens the calls of a matrix cell on the canvas.
pub fn on_click(app: &mut App, el: &web_sys::Element) -> bool {
    let Some(cell) = dom::closest(el, "[data-mx-a]") else {
        return false;
    };
    let (Some(a), Some(b)) = (dom::data(&cell, "mx-a"), dom::data(&cell, "mx-b")) else {
        return false;
    };
    let f = flows(&app.d, app.s.space.show_unchanged, app.s.space.show_uses);
    let list = f.pairs.get(&(a, b)).cloned().unwrap_or_default();
    for (x, y, _) in list.iter().take(12) {
        crate::canvas::open(app, x, None, false);
        crate::canvas::open(app, y, Some(x), false);
    }
    crate::set_view(app, crate::View::Canvas);
    true
}
