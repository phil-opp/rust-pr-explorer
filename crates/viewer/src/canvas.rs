//! The canvas: item cards arranged freely, with connectors, tabs and a
//! module scope that follows Rust's privacy rules.

use crate::code::{Links, code_html, docs_html, esc};
use crate::data::{Data, changed, kind_order, within};
use crate::{App, View, dom, toast};
use model::{Item, Kind, Status, UseCtx};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use web_sys::{Element, PointerEvent, WheelEvent};

const CARD_W: f64 = 580.0;
/// At most this many cards are opened by one "calls"/"callers" click.
const EXPAND_LIMIT: usize = 10;

pub struct Group {
    pub gid: u32,
    pub tabs: Vec<String>,
    pub active: String,
    pub x: f64,
    pub y: f64,
    /// Items whose source view is expanded.
    pub open_src: BTreeSet<String>,
}

#[derive(Clone, Copy)]
pub struct Pan {
    pub x: f64,
    pub y: f64,
    pub k: f64,
}

pub enum Drag {
    Card {
        gid: u32,
        sx: f64,
        sy: f64,
        gx: f64,
        gy: f64,
    },
    Pan {
        sx: f64,
        sy: f64,
        px: f64,
        py: f64,
    },
}

pub struct Canvas {
    /// The module the canvas looks from.
    pub scope: String,
    pub groups: Vec<Group>,
    /// Connectors recording where a card was opened from.
    pub links: Vec<(String, String)>,
    next_gid: u32,
    pub pan: Pan,
    pub frames: bool,
    pub drag: Option<Drag>,
}

impl Canvas {
    pub fn new(scope: String) -> Canvas {
        Canvas {
            scope,
            groups: Vec::new(),
            links: Vec::new(),
            next_gid: 1,
            pan: Pan {
                x: 60.0,
                y: 30.0,
                k: 1.0,
            },
            frames: false,
            drag: None,
        }
    }

    fn group_of(&self, id: &str) -> Option<usize> {
        self.groups
            .iter()
            .position(|g| g.tabs.iter().any(|t| t == id))
    }

    fn by_gid(&self, gid: u32) -> Option<usize> {
        self.groups.iter().position(|g| g.gid == gid)
    }

    fn add_group(&mut self, id: &str, x: f64, y: f64) -> usize {
        self.groups.push(Group {
            gid: self.next_gid,
            tabs: vec![id.to_string()],
            active: id.to_string(),
            x,
            y,
            open_src: BTreeSet::new(),
        });
        self.next_gid += 1;
        self.groups.len() - 1
    }

    fn add_link(&mut self, from: &str, to: &str) {
        if from != to
            && !self
                .links
                .iter()
                .any(|(a, b)| (a == from && b == to) || (a == to && b == from))
        {
            self.links.push((from.to_string(), to.to_string()));
        }
    }
}

fn visible(d: &Data, c: &Canvas, g: &Group) -> bool {
    d.visible_from(&g.active, &c.scope)
}

fn card_el(gid: u32) -> Option<Element> {
    dom::query(&format!(r#".card[data-gid="{gid}"]"#))
}

fn height_of(d: &Data, g: &Group) -> f64 {
    if let Some(el) = card_el(g.gid) {
        let (_, h) = dom::offset_size(&el);
        if h > 0.0 {
            return h;
        }
    }
    let it = d.item(&g.active);
    120.0
        + it.map_or(0.0, |it| it.info.sig.lines().count() as f64 * 18.0)
        + if it.is_some_and(|it| !it.info.docs.is_empty()) {
            40.0
        } else {
            0.0
        }
}

/// Moves a card down until it no longer overlaps another visible card.
fn free_spot(d: &Data, c: &Canvas, gi: usize, x: f64, mut y: f64) -> (f64, f64) {
    let h = height_of(d, &c.groups[gi]);
    for _ in 0..80 {
        let hit = c.groups.iter().enumerate().find(|(i, o)| {
            *i != gi && visible(d, c, o) && {
                let oh = height_of(d, o);
                x < o.x + CARD_W + 20.0
                    && x + CARD_W + 20.0 > o.x
                    && y < o.y + oh + 20.0
                    && y + h + 20.0 > o.y
            }
        });
        match hit {
            None => break,
            Some((_, o)) => y = o.y + height_of(d, o) + 30.0,
        }
    }
    (x, y)
}

fn place(d: &Data, c: &mut Canvas, gi: usize) {
    let (x, y) = free_spot(d, c, gi, c.groups[gi].x, c.groups[gi].y);
    c.groups[gi].x = x;
    c.groups[gi].y = y;
}

/// Places `id` on the canvas, next to the card of `from` if given.
pub fn open(app: &mut App, id: &str, from: Option<&str>, switch_view: bool) {
    let d = app.d.clone();
    let Some(it) = d.item(id) else { return };
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
            let (x, y) = match from.and_then(|f| c.group_of(f)) {
                Some(fi) => (c.groups[fi].x + CARD_W + 80.0, c.groups[fi].y),
                None => {
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
    if switch_view && app.s.view != View::Canvas {
        crate::set_view(app, View::Canvas);
    } else {
        render(app);
    }
    flash(gid);
    settle(gid);
    if switch_view {
        ensure_visible(gid);
    }
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

fn ensure_visible(gid: u32) {
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
            if sx < 0.0 || sy < 0.0 || sx > r.width() - 100.0 || sy > r.height() - 100.0 {
                c.pan.x = r.width() / 2.0 - (g.x + CARD_W / 2.0) * c.pan.k;
                c.pan.y = 60.0 - g.y * c.pan.k;
                apply_pan(c);
                draw_edges(app);
            }
        });
    });
}

fn close_tab(app: &mut App, gi: usize, id: &str) {
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
pub enum Related {
    Callees,
    Callers,
    UsedBy,
}

/// Opens callees, callers or users of an item next to its card.
pub fn expand(app: &mut App, id: &str, kind: Related) {
    let d = app.d.clone();
    let Some(it) = d.item(id) else { return };
    let mut targets: Vec<String> = match kind {
        Related::Callees => it.info.calls.iter().map(|c| c.to.clone()).collect(),
        Related::Callers => d.callers.get(id).map_or(Vec::new(), |v| v.to_vec()),
        Related::UsedBy => d
            .used_by
            .get(id)
            .map_or(Vec::new(), |v| v.iter().map(|(f, _)| f.clone()).collect()),
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
    for t in &vis {
        let c = &mut app.s.canvas;
        let (from, to) = if side > 0.0 {
            (id, t.as_str())
        } else {
            (t.as_str(), id)
        };
        if c.group_of(t).is_some() {
            c.add_link(from, to);
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
        opened += 1;
        // Render now, so the next card is placed using this card's real height.
        render(app);
        let c = &app.s.canvas;
        if let Some(gi) = c.by_gid(gid) {
            offset = c.groups[gi].y - fy + height_of(&d, &c.groups[gi]) + 30.0;
        }
    }
    render(app);
    let mut notes = Vec::new();
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

/// Opens all members of a type in one tabbed card.
fn members_as_tabs(app: &mut App, id: &str) {
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

fn hidden_note(d: &Data, scope: &str, list: &[String], label: &str) -> String {
    let hidden: Vec<&String> = list
        .iter()
        .filter(|t| d.item(t).is_some() && !d.visible_from(t, scope))
        .collect();
    if hidden.is_empty() {
        return String::new();
    }
    let mut mods: Vec<String> = hidden
        .iter()
        .filter_map(|t| d.hiding_module(t, scope))
        .collect();
    mods.sort();
    mods.dedup();
    format!(
        r#"<div class="hidden-note">{} {label} private to {} — not visible from {}</div>"#,
        hidden.len(),
        mods.iter()
            .map(|m| format!(r#"<a data-drill="{}">{}</a>"#, esc(m), esc(&d.mod_label(m))))
            .collect::<Vec<_>>()
            .join(", "),
        esc(&d.mod_label(scope))
    )
}

fn card_html(d: &Data, c: &Canvas, g: &Group) -> String {
    let Some(it) = d.item(&g.active) else {
        return String::new();
    };
    let scope = c.scope.as_str();
    let id = &it.info.id;
    let tabs = if g.tabs.len() > 1 {
        format!(
            r#"<div class="card-tabs">{}</div>"#,
            g.tabs
                .iter()
                .filter_map(|t| d.item(t))
                .map(|t| format!(
                    r#"<div class="card-tab {}" data-tab="{}"><span class="dot {}"></span>{}<span class="x" data-close="{}" title="Close tab">×</span></div>"#,
                    if t.info.id == g.active { "active" } else { "" },
                    esc(&t.info.id),
                    t.status.css(),
                    esc(&d.display_name(t)),
                    esc(&t.info.id)
                ))
                .collect::<String>()
        )
    } else {
        String::new()
    };
    let mut callees: Vec<String> = it.info.calls.iter().map(|c| c.to.clone()).collect();
    callees.dedup();
    let mut callers: Vec<String> = d.callers.get(id).map_or(Vec::new(), |v| v.to_vec());
    callers.sort();
    callers.dedup();
    let used_by = d.used_by.get(id).cloned().unwrap_or_default();
    let mut users: Vec<String> = used_by.iter().map(|(f, _)| f.clone()).collect();
    users.sort();
    users.dedup();

    let mut acts = String::new();
    if it.info.kind == Kind::Mod {
        acts.push_str(r#"<button data-act="drill">Drill in</button><button data-act="page">Module page</button>"#);
    } else {
        if !callees.is_empty() {
            acts.push_str(&format!(r#"<button data-act="callees" title="Show what this calls">calls ▸ <span class="n">{}</span></button>"#, callees.len()));
        }
        if !callers.is_empty() {
            acts.push_str(&format!(r#"<button data-act="callers" title="Show what calls this">◂ callers <span class="n">{}</span></button>"#, callers.len()));
        }
        if !users.is_empty() {
            let mut ctxs: BTreeMap<UseCtx, usize> = BTreeMap::new();
            for (_, ctx) in &used_by {
                *ctxs.entry(*ctx).or_default() += 1;
            }
            let wher = ctxs
                .iter()
                .map(|(c, n)| format!("{n} in {}", c.plural()))
                .collect::<Vec<_>>()
                .join(", ");
            acts.push_str(&format!(
                r#"<button data-act="uses" title="Show where this is used: {w}">◂ used by <span class="n">{} ({w})</span></button>"#,
                users.len(),
                w = esc(&wher)
            ));
        }
        if !it.info.children.is_empty() {
            acts.push_str(
                r#"<button data-act="tabs" title="Open members as tabs">members as tabs</button>"#,
            );
        }
    }
    if g.tabs.len() > 1 {
        acts.push_str(r#"<button data-act="detach" title="Move this tab into its own card">detach tab</button>"#);
    }

    // Signature, docs and source, with changes marked.
    let body = if d.external(id).is_some() {
        // External docs (e.g. the standard library's) can be long: first
        // paragraph, the rest expandable.
        let first = crate::code::first_paragraph(&it.info.docs);
        let more = if first.len() < it.info.docs.trim().len() {
            format!(
                r#"<details class="docs-old"><summary>full documentation</summary><div class="docs">{}</div></details>"#,
                docs_html(d, &it.info.docs, &BTreeMap::new())
            )
        } else {
            String::new()
        };
        crate::code::marked_html(d.sig_segs(it, scope)) + &d.docs_block_html(it, false) + &more
    } else {
        d.item_body_html(
            it,
            scope,
            true,
            d.source_seg(it, g.open_src.contains(id), true),
        )
    };
    let external = d.external(id).map(|e| {
        format!(
            r#"<div class="ext-note">Outside the analyzed crates, as rust-analyzer describes it.{}</div>"#,
            e.url
                .as_deref()
                // The URL comes from the dependency (`html_root_url`).
                .filter(|u| crate::code::web_url(u))
                .map(|u| format!(
                    r#" <a href="{}" target="_blank" rel="noopener">Documentation ↗</a>"#,
                    esc(u)
                ))
                .unwrap_or_default()
        )
    });
    // Members, like a rustdoc type or module page.
    let ms: Vec<&Item> = if it.info.kind == Kind::Mod {
        d.mod_children_items(id)
    } else {
        d.members(it)
    };
    let members = if ms.is_empty() {
        String::new()
    } else {
        let vis: Vec<&Item> = ms
            .iter()
            .copied()
            .filter(|m| d.visible_from(&m.info.id, scope))
            .collect();
        let mut groups: BTreeMap<String, Vec<&Item>> = BTreeMap::new();
        for m in &vis {
            let label = if m.info.kind == Kind::Impl {
                if it.info.kind == Kind::Trait {
                    "Implementors".to_string()
                } else {
                    "Trait implementations".to_string()
                }
            } else if it.info.kind == Kind::Mod {
                format!("{}s", m.info.kind.label())
            } else {
                "Methods & items".to_string()
            };
            groups.entry(label).or_default().push(m);
        }
        let mut html = String::from(r#"<div class="members">"#);
        for (label, mut list) in groups {
            list.sort_by(|a, b| kind_order(a, b));
            html.push_str(&format!(
                r#"<div class="mgroup"><div class="mlabel">{label}</div>"#
            ));
            for m in list {
                let dot = if m.status == Status::Unchanged && d.touched(m) {
                    "changed"
                } else {
                    m.status.css()
                };
                html.push_str(&format!(
                    r#"<div class="mitem {}"><span class="dot {dot}"></span><a class="ref" data-id="{}" data-kind="{}" data-path="{}">{}</a>{}</div>"#,
                    if m.info.vis_scope.is_some() { "private" } else { "" },
                    esc(&m.info.id),
                    m.info.kind.css(),
                    esc(&m.info.id),
                    esc(&m.info.name),
                    if m.info.vis_scope.is_some() { format!(r#"<span class="vis">{}</span>"#, esc(&m.info.vis_label)) } else { String::new() }
                ));
            }
            html.push_str("</div>");
        }
        if ms.len() > vis.len() {
            html.push_str(&format!(
                r#"<div class="hidden-note">{} more not visible from {}</div>"#,
                ms.len() - vis.len(),
                esc(&d.mod_label(scope))
            ));
        }
        html.push_str("</div>");
        html
    };

    let impl_header = it
        .info
        .impl_header
        .as_deref()
        .map(|h| {
            format!(
                r#"<div class="card-impl">{}</div>"#,
                code_html(d, h, Links::None)
            )
        })
        .unwrap_or_default();
    let context = if !changed(it) && !d.touched(it) {
        "context"
    } else {
        ""
    };
    format!(
        r#"<div class="card cs-{} {context}" data-gid="{}" style="left:{}px;top:{}px">
        {tabs}
        <div class="card-h"><span class="kind">{}</span>{} {}<span class="grow"></span>{}<span class="x" data-close="{}" title="Close">×</span></div>
        <div class="card-loc">{}</div>
        <div class="card-b">{impl_header}{body}{}</div>{members}{}{}</div>"#,
        it.status.css(),
        g.gid,
        g.x,
        g.y,
        it.info.kind.label(),
        d.title_html(it, false),
        d.vis_html(it),
        d.status_html(it),
        esc(id),
        if external.is_some() {
            String::new()
        } else if d.reexported(it) {
            format!(
                "{} · public as {}",
                esc(&d.mod_label(d.mod_of(it))),
                it.info
                    .public_paths
                    .iter()
                    .map(|p| format!("<code>{}</code>", esc(p)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            esc(&d.mod_label(d.mod_of(it)))
        },
        external.unwrap_or_default(),
        {
            // Footer: what is not shown, and what can be opened next.
            let notes = (if it.info.unresolved_calls > 0 {
                let n = it.info.unresolved_calls;
                let why = if d.out.meta.resolution.starts_with("rust-analyzer") {
                    "rust-analyzer could not determine their targets"
                } else {
                    "method calls and calls inside macros need rust-analyzer"
                };
                format!(
                    r#"<div class="hidden-note">{n} call site{} not resolved ({why}), so not shown as {}.</div>"#,
                    if n > 1 { "s" } else { "" },
                    if n > 1 { "calls" } else { "a call" }
                )
            } else {
                String::new()
            } + &hidden_note(d, scope, &callees, "callees are")
                + &hidden_note(d, scope, &callers, "callers are"));
            if notes.is_empty() && acts.is_empty() {
                String::new()
            } else {
                format!(
                    r#"<div class="card-foot">{notes}<div class="card-acts">{acts}</div></div>"#
                )
            }
        },
        match crate::review::control_html(id) {
            c if c.is_empty() => c,
            c => format!(r#"<div class="card-rv">{c}</div>"#),
        },
    )
}

pub fn render(app: &mut App) {
    let d = app.d.clone();
    let c = &app.s.canvas;
    let chain = d.mod_chain(&c.scope);
    let hidden = c.groups.iter().filter(|g| !visible(&d, c, g)).count();
    let bar = format!(
        r#"<span>Viewing from</span><span class="scope">{}</span>{}
        <span class="sep"></span>
        <button id="cv-arrange" title="Lay cards out inside their modules">Arrange by module</button>
        <button id="cv-frames" class="{}" title="Draw module boundaries">Module frames</button>
        <button id="cv-fit">Fit</button>
        <button id="cv-clear">Clear</button>"#,
        chain
            .iter()
            .map(|m| format!(
                r#"<a data-scope="{}" title="Look from {}">{}</a>"#,
                esc(m),
                esc(&d.mod_label(m)),
                esc(d.item(m).map_or("", |i| &i.info.name))
            ))
            .collect::<Vec<_>>()
            .join("::"),
        if hidden > 0 {
            format!(
                r#"<span class="chip" title="Private to submodules of the current scope">{hidden} card{} hidden at this scope</span>"#,
                if hidden > 1 { "s" } else { "" }
            )
        } else {
            String::new()
        },
        if c.frames { "on" } else { "" },
    );
    dom::set_html("canvas-bar", &bar);
    let cards: String = c
        .groups
        .iter()
        .filter(|g| visible(&d, c, g))
        .map(|g| card_html(&d, c, g))
        .collect();
    dom::set_html("cards", &cards);
    let any = c.groups.iter().any(|g| visible(&d, c, g));
    dom::set_html(
        "canvas-empty",
        if any {
            ""
        } else {
            "<div>Nothing on the canvas yet.<br>Open items from the <b>Overview</b> or a <b>module page</b>, or use <b>Find item</b>.<br>Click any name in a card to place its item next to it.</div>"
        },
    );
    apply_pan(c);
    mark_on_canvas(app);
    dom::raf(|| {
        crate::with_app(|app| {
            draw_edges(app);
            mark_on_canvas(app);
        });
    });
}

pub fn apply_pan(c: &Canvas) {
    if let Some(world) = dom::by_id("world") {
        dom::set_style(
            &world,
            "transform",
            &format!("translate({}px, {}px) scale({})", c.pan.x, c.pan.y, c.pan.k),
        );
    }
}

/// Highlights names that refer to items already on the canvas.
fn mark_on_canvas(app: &App) {
    let d = &app.d;
    let c = &app.s.canvas;
    let on: BTreeSet<&str> = c
        .groups
        .iter()
        .filter(|g| visible(d, c, g))
        .map(|g| g.active.as_str())
        .collect();
    let actives: HashMap<u32, &str> = c
        .groups
        .iter()
        .map(|g| (g.gid, g.active.as_str()))
        .collect();
    for a in dom::query_all("#cards a.ref[data-id]") {
        let id = dom::data(&a, "id").unwrap_or_default();
        let own = dom::closest(&a, ".card")
            .and_then(|card| dom::data(&card, "gid"))
            .and_then(|g| g.parse::<u32>().ok())
            .and_then(|g| actives.get(&g).copied());
        dom::toggle_class(
            &a,
            "on-canvas",
            on.contains(id.as_str()) && own != Some(id.as_str()),
        );
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

fn card_rect(g: &Group) -> Option<Rect> {
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
            list.sort_by(|&a, &b| {
                let (ia, ib) = (
                    d.item(&self.c.groups[a].active),
                    d.item(&self.c.groups[b].active),
                );
                let (ca, cb) = (ia.is_some_and(changed), ib.is_some_and(changed));
                cb.cmp(&ca).then_with(|| {
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

/// Opens a module's changed items and their direct neighbours in it.
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
    for it in &own {
        let neighbours = it
            .info
            .calls
            .iter()
            .map(|c| c.to.clone())
            .chain(d.callers.get(&it.info.id).into_iter().flatten().cloned());
        for n in neighbours {
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
    crate::set_view(app, View::Canvas);
    arrange_by_module(app);
    render(app);
    fit_view();
}

/// Handles a click inside the canvas. Returns whether it was consumed.
pub fn on_click(app: &mut App, el: &Element) -> bool {
    let gid = dom::closest(el, ".card")
        .and_then(|c| dom::data(&c, "gid"))
        .and_then(|g| g.parse::<u32>().ok());
    if let Some(x) = dom::closest(el, "[data-close]")
        && let Some(gi) = gid.and_then(|g| app.s.canvas.by_gid(g))
    {
        close_tab(app, gi, &dom::data(&x, "close").unwrap_or_default());
        render(app);
        return true;
    }
    if let Some(t) = dom::closest(el, "[data-tab]")
        && let Some(gi) = gid.and_then(|g| app.s.canvas.by_gid(g))
    {
        app.s.canvas.groups[gi].active = dom::data(&t, "tab").unwrap_or_default();
        render(app);
        return true;
    }
    if let Some(b) = dom::closest(el, "[data-act]")
        && let Some(gi) = gid.and_then(|g| app.s.canvas.by_gid(g))
    {
        let id = app.s.canvas.groups[gi].active.clone();
        match dom::data(&b, "act").as_deref() {
            Some("callees") => expand(app, &id, Related::Callees),
            Some("callers") => expand(app, &id, Related::Callers),
            Some("uses") => expand(app, &id, Related::UsedBy),
            Some("tabs") => members_as_tabs(app, &id),
            Some("drill") => {
                app.s.canvas.scope = id;
                render(app);
            }
            Some("page") => {
                app.s.module = id;
                crate::set_view(app, View::Module);
            }
            Some("detach") => {
                let d = app.d.clone();
                let c = &mut app.s.canvas;
                let g = &mut c.groups[gi];
                g.tabs.retain(|t| *t != id);
                g.active = g.tabs[0].clone();
                let (x, y) = (g.x + CARD_W + 60.0, g.y);
                let ni = c.add_group(&id, x, y);
                place(&d, c, ni);
                render(app);
            }
            _ => return false,
        }
        return true;
    }
    if let Some(a) = dom::closest(el, "[data-drill]") {
        let m = dom::data(&a, "drill").unwrap_or_default();
        toast(&format!("Now viewing from {}", app.d.mod_label(&m)));
        app.s.canvas.scope = m;
        render(app);
        return true;
    }
    if let Some(a) = dom::closest(el, "[data-scope]") {
        app.s.canvas.scope = dom::data(&a, "scope").unwrap_or_default();
        render(app);
        return true;
    }
    match el
        .closest("button")
        .ok()
        .flatten()
        .map(|b| b.id())
        .as_deref()
    {
        Some("cv-arrange") => {
            arrange_by_module(app);
            render(app);
            fit_view();
        }
        Some("cv-frames") => {
            app.s.canvas.frames = !app.s.canvas.frames;
            render(app);
        }
        Some("cv-fit") => fit_view(),
        Some("cv-clear") => {
            app.s.canvas.groups.clear();
            app.s.canvas.links.clear();
            render(app);
        }
        _ => return false,
    }
    true
}

/// Starts dragging a card (by its header or tabs) or panning the canvas.
pub fn on_pointer_down(app: &mut App, el: &Element, ev: &PointerEvent) -> bool {
    if dom::closest(el, "#viewport").is_none() {
        return false;
    }
    let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
    let c = &mut app.s.canvas;
    if let Some(card) = dom::closest(el, ".card") {
        let handle = dom::closest(el, ".card-h, .card-tabs").is_some();
        if !handle || dom::closest(el, "[data-close], button, a").is_some() {
            return false;
        }
        let Some(gi) = dom::data(&card, "gid")
            .and_then(|g| g.parse::<u32>().ok())
            .and_then(|g| c.by_gid(g))
        else {
            return false;
        };
        ev.prevent_default();
        let g = &c.groups[gi];
        c.drag = Some(Drag::Card {
            gid: g.gid,
            sx: x,
            sy: y,
            gx: g.x,
            gy: g.y,
        });
        dom::set_style(&card, "z-index", "10");
        dom::set_style(&card, "pointer-events", "none");
    } else {
        c.drag = Some(Drag::Pan {
            sx: x,
            sy: y,
            px: c.pan.x,
            py: c.pan.y,
        });
        if let Some(vp) = dom::by_id("viewport") {
            dom::toggle_class(&vp, "panning", true);
        }
    }
    true
}

/// The card whose header is under the pointer, if the pointer is close
/// enough to the top to mean "drop here to merge into tabs".
fn drop_target(x: f64, y: f64, dragged: u32) -> Option<Element> {
    let under = dom::document().element_from_point(x as f32, y as f32)?;
    let card = dom::closest(&under, ".card")?;
    let gid = dom::data(&card, "gid")?.parse::<u32>().ok()?;
    (gid != dragged && y - card.get_bounding_client_rect().top() < 60.0).then_some(card)
}

pub fn on_pointer_move(app: &mut App, ev: &PointerEvent) {
    let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
    let c = &mut app.s.canvas;
    match c.drag {
        Some(Drag::Card {
            gid,
            sx,
            sy,
            gx,
            gy,
        }) => {
            let Some(gi) = c.by_gid(gid) else { return };
            let g = &mut c.groups[gi];
            g.x = gx + (x - sx) / c.pan.k;
            g.y = gy + (y - sy) / c.pan.k;
            if let Some(el) = card_el(gid) {
                dom::set_style(&el, "left", &format!("{}px", g.x));
                dom::set_style(&el, "top", &format!("{}px", g.y));
            }
            draw_edges(app);
            for el in dom::query_all(".card.hover-target") {
                dom::toggle_class(&el, "hover-target", false);
            }
            if let Some(target) = drop_target(x, y, gid) {
                dom::toggle_class(&target, "hover-target", true);
            }
        }
        Some(Drag::Pan { sx, sy, px, py }) => {
            c.pan.x = px + x - sx;
            c.pan.y = py + y - sy;
            apply_pan(c);
        }
        None => {}
    }
}

pub fn on_pointer_up(app: &mut App, ev: &PointerEvent) {
    let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
    let Some(drag) = app.s.canvas.drag.take() else {
        return;
    };
    if let Some(vp) = dom::by_id("viewport") {
        dom::toggle_class(&vp, "panning", false);
    }
    for el in dom::query_all(".card.hover-target") {
        dom::toggle_class(&el, "hover-target", false);
    }
    if let Drag::Card { gid, .. } = drag {
        // Look for a drop target while the dragged card ignores the pointer.
        let target = drop_target(x, y, gid)
            .and_then(|t| dom::data(&t, "gid"))
            .and_then(|g| g.parse::<u32>().ok());
        let c = &mut app.s.canvas;
        if let (Some(tgid), Some(si)) = (target, c.by_gid(gid)) {
            let source = c.groups.remove(si);
            if let Some(ti) = c.by_gid(tgid) {
                let t = &mut c.groups[ti];
                for tab in source.tabs {
                    if !t.tabs.contains(&tab) {
                        t.tabs.push(tab);
                    }
                }
                t.active = source.active;
                toast("Merged into tabs");
            }
        }
        render(app);
    }
}

pub fn on_wheel(app: &mut App, el: &Element, ev: &WheelEvent) {
    if dom::closest(el, ".card").is_some() && !ev.ctrl_key() {
        return;
    }
    ev.prevent_default();
    let Some(vp) = dom::by_id("viewport") else {
        return;
    };
    let r = vp.get_bounding_client_rect();
    let (mx, my) = (
        ev.client_x() as f64 - r.left(),
        ev.client_y() as f64 - r.top(),
    );
    let c = &mut app.s.canvas;
    let k = (c.pan.k * (-ev.delta_y() * 0.0015).exp()).clamp(0.2, 2.0);
    c.pan.x = mx - (mx - c.pan.x) * k / c.pan.k;
    c.pan.y = my - (my - c.pan.y) * k / c.pan.k;
    c.pan.k = k;
    apply_pan(c);
}

/// Remembers which source views are expanded across re-renders.
pub fn on_details_toggle(app: &mut App, el: &Element) {
    let Some(details) = dom::closest(el, "details.src") else {
        return;
    };
    let Some(id) = dom::data(&details, "src") else {
        return;
    };
    let open = details.has_attribute("open");
    let gid = dom::closest(&details, ".card")
        .and_then(|c| dom::data(&c, "gid"))
        .and_then(|g| g.parse::<u32>().ok());
    if let Some(gi) = gid.and_then(|g| app.s.canvas.by_gid(g)) {
        let set = &mut app.s.canvas.groups[gi].open_src;
        if open {
            set.insert(id)
        } else {
            set.remove(&id)
        };
    }
    draw_edges(app);
}

/// Outlines the card of the item a hovered name refers to.
pub fn highlight_card_of(app: &App, id: &str) {
    let c = &app.s.canvas;
    if let Some(gi) = c.group_of(id)
        && visible(&app.d, c, &c.groups[gi])
        && let Some(el) = card_el(c.groups[gi].gid)
    {
        dom::toggle_class(&el, "hover-target", true);
    }
}

pub fn active_item_of_card(app: &App, card: &Element) -> Option<String> {
    let gid = dom::data(card, "gid")?.parse::<u32>().ok()?;
    let gi = app.s.canvas.by_gid(gid)?;
    Some(app.s.canvas.groups[gi].active.clone())
}
