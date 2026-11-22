//! The 3D view: every module is a plane holding its items, with nested
//! modules below their parents. Drawn with a small perspective projection
//! on a 2D canvas.

use crate::code::esc;
use crate::data::{Data, changed};
use crate::{App, dom};
use model::{Item, Kind, Status};
use std::collections::{BTreeMap, HashMap};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{CanvasRenderingContext2d, Element, HtmlCanvasElement, PointerEvent, WheelEvent};

#[derive(Clone, Copy)]
pub struct Camera {
    yaw: f64,
    pitch: f64,
    dist: f64,
    zoom: f64,
    /// The point the camera looks at.
    cx: f64,
    cy: f64,
}

struct Node {
    id: String,
    /// Only shown as a neighbour of a changed item.
    context: bool,
    /// Exposed by its module (filled dot) rather than private (hollow).
    exposed: bool,
    plane: usize,
    x: f64,
    y: f64,
    z: f64,
    lz: f64,
}

struct Plane {
    module: String,
    depth: usize,
    x: f64,
    y: f64,
    w: f64,
    d: f64,
    nodes: Vec<usize>,
}

struct Edge {
    a: usize,
    b: usize,
    call: bool,
}

#[derive(Default)]
struct Model {
    planes: Vec<Plane>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

pub struct Space {
    cam: Camera,
    model: Model,
    hover: Option<usize>,
    focus: Option<String>,
    show_unchanged: bool,
    show_uses: bool,
    /// Screen positions and radii of the nodes from the last frame.
    projected: Vec<(usize, f64, f64, f64)>,
    /// Zoom that fits the whole model.
    fit_zoom: f64,
    drag: Option<(f64, f64, f64, f64, bool)>,
}

impl Space {
    pub fn new() -> Space {
        Space {
            cam: Camera {
                cx: 0.0,
                cy: 0.0,
                yaw: -0.45,
                pitch: 0.42,
                dist: 1800.0,
                zoom: 1.0,
            },
            model: Model::default(),
            hover: None,
            focus: None,
            show_unchanged: false,
            show_uses: false,
            projected: Vec::new(),
            fit_zoom: 1.0,
            drag: None,
        }
    }
}

fn build(d: &Data, sp: &Space) -> Model {
    // Changed items plus their direct neighbours as context.
    let mut wanted: BTreeMap<String, bool> = BTreeMap::new();
    let mut want = |id: &str, context: bool| {
        let Some(it) = d.item(id) else { return };
        if matches!(it.info.kind, Kind::Mod | Kind::Impl) {
            return;
        }
        let e = wanted.entry(id.to_string()).or_insert(context);
        *e = *e && context;
    };
    for it in d.items() {
        if matches!(it.info.kind, Kind::Mod | Kind::Impl) || !(changed(it) || sp.show_unchanged) {
            continue;
        }
        want(&it.info.id, false);
        for c in &it.info.calls {
            want(&c.to, true);
        }
        for f in d.callers.get(&it.info.id).into_iter().flatten() {
            want(f, true);
        }
        if sp.show_uses {
            for u in &it.info.uses {
                want(&u.to, true);
            }
        }
    }
    let mut model = Model::default();
    let mut plane_index: HashMap<String, usize> = HashMap::new();
    let mut ensure_plane = |model: &mut Model, m: &str| -> usize {
        *plane_index.entry(m.to_string()).or_insert_with(|| {
            model.planes.push(Plane {
                module: m.to_string(),
                depth: d.mod_depth(m),
                x: 0.0,
                y: 0.0,
                w: 0.0,
                d: 0.0,
                nodes: Vec::new(),
            });
            model.planes.len() - 1
        })
    };
    let mut node_index: HashMap<String, usize> = HashMap::new();
    for (id, context) in &wanted {
        let it = d.item(id).unwrap();
        let m = d.mod_of(it).to_string();
        for anc in d.mod_chain(&m) {
            ensure_plane(&mut model, &anc);
        }
        let p = ensure_plane(&mut model, &m);
        let exposed = !d.private_to(it, &m);
        model.nodes.push(Node {
            id: id.clone(),
            context: *context,
            exposed,
            plane: p,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            lz: 0.0,
        });
        let ni = model.nodes.len() - 1;
        model.planes[p].nodes.push(ni);
        node_index.insert(id.clone(), ni);
    }
    // Lay out nodes inside planes: exposed in front, private behind.
    const ROW: f64 = 85.0;
    for p in &mut model.planes {
        // Wide enough for the longest label (about 7.5 units per char).
        let longest = p
            .nodes
            .iter()
            .filter_map(|&n| d.item(&model.nodes[n].id))
            .map(|it| d.display_name(it).chars().count())
            .max()
            .unwrap_or(0);
        let col = (longest as f64 * 7.5 + 40.0).max(170.0);
        let mut exposed: Vec<usize> = p
            .nodes
            .iter()
            .copied()
            .filter(|&n| model.nodes[n].exposed)
            .collect();
        let mut private: Vec<usize> = p
            .nodes
            .iter()
            .copied()
            .filter(|&n| !model.nodes[n].exposed)
            .collect();
        let by_name = |a: &usize, b: &usize| model.nodes[*a].id.cmp(&model.nodes[*b].id);
        exposed.sort_by(by_name);
        private.sort_by(by_name);
        let cols = ((p.nodes.len() as f64 + 1.0).sqrt().ceil() as usize).max(2);
        p.w = (cols as f64 * col + 60.0).max(260.0);
        let rows_exposed = exposed.len().div_ceil(cols);
        let rows_private = private.len().div_ceil(cols);
        p.d = ((rows_exposed + rows_private) as f64 * ROW + 90.0).max(160.0);
        let front = p.d / 2.0 - 50.0;
        for (list, z0) in [
            (&exposed, front),
            (
                &private,
                front - rows_exposed as f64 * ROW - if rows_exposed > 0 { 20.0 } else { 0.0 },
            ),
        ] {
            for (i, &n) in list.iter().enumerate() {
                let node = &mut model.nodes[n];
                node.x = -p.w / 2.0 + 50.0 + (i % cols) as f64 * col;
                node.lz = z0 - (i / cols) as f64 * ROW;
            }
        }
    }
    // One row of planes per depth, siblings side by side.
    let mut by_depth: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, p) in model.planes.iter().enumerate() {
        by_depth.entry(p.depth).or_default().push(i);
    }
    for (depth, mut row) in by_depth {
        row.sort_by(|a, b| model.planes[*a].module.cmp(&model.planes[*b].module));
        let total: f64 =
            row.iter().map(|&p| model.planes[p].w).sum::<f64>() + (row.len() as f64 - 1.0) * 60.0;
        let mut x = -total / 2.0;
        for p in row {
            let plane = &mut model.planes[p];
            plane.x = x + plane.w / 2.0;
            x += plane.w + 60.0;
            plane.y = -(depth as f64) * 320.0;
        }
    }
    let ys: Vec<f64> = model.planes.iter().map(|p| p.y).collect();
    let center = (ys.iter().copied().fold(f64::MAX, f64::min)
        + ys.iter().copied().fold(f64::MIN, f64::max))
        / 2.0;
    for p in &mut model.planes {
        p.y -= center;
    }
    for n in &mut model.nodes {
        let p = &model.planes[n.plane];
        n.x += p.x;
        n.y = p.y;
        n.z = n.lz;
    }
    for (a, node) in model.nodes.iter().enumerate() {
        let it = d.item(&node.id).unwrap();
        for c in &it.info.calls {
            if let Some(&b) = node_index.get(&c.to) {
                model.edges.push(Edge { a, b, call: true });
            }
        }
        if sp.show_uses {
            for u in &it.info.uses {
                if let Some(&b) = node_index.get(&u.to) {
                    model.edges.push(Edge { a, b, call: false });
                }
            }
        }
    }
    model
}

fn canvas() -> Option<(HtmlCanvasElement, CanvasRenderingContext2d)> {
    let c: HtmlCanvasElement = dom::by_id("space")?.dyn_into().ok()?;
    let ctx = c.get_context("2d").ok()??.dyn_into().ok()?;
    Some((c, ctx))
}

/// A projected point: screen x and y, perspective scale, and depth.
type Screen = (f64, f64, f64, f64);

/// Projects a point of the scene onto the screen.
fn project(cam: &Camera, w: f64, h: f64, x: f64, y: f64, z: f64) -> Screen {
    let (x, y) = (x - cam.cx, y - cam.cy);
    let (cy, sy) = (cam.yaw.cos(), cam.yaw.sin());
    let (cp, sp) = (cam.pitch.cos(), cam.pitch.sin());
    let xx = x * cy - z * sy;
    let zz = x * sy + z * cy;
    let yy = y * cp - zz * sp;
    let zz = y * sp + zz * cp;
    let s = cam.dist / (cam.dist - zz) * cam.zoom;
    (w / 2.0 + xx * s, h / 2.0 - yy * s, s, zz)
}

struct Colors {
    text: String,
    muted: String,
    bg: String,
    module: String,
    func: String,
    ty: String,
    added: String,
    changed: String,
    removed: String,
    private: String,
}

fn colors() -> Colors {
    Colors {
        text: dom::css_var("--text"),
        muted: dom::css_var("--muted"),
        bg: dom::css_var("--bg"),
        module: dom::css_var("--mod"),
        func: dom::css_var("--fn"),
        ty: dom::css_var("--type"),
        added: dom::css_var("--added"),
        changed: dom::css_var("--changed"),
        removed: dom::css_var("--removed"),
        private: dom::css_var("--private"),
    }
}

fn dash(ctx: &CanvasRenderingContext2d, pattern: &[f64]) {
    let arr = js_sys::Array::new();
    for p in pattern {
        arr.push(&JsValue::from_f64(*p));
    }
    let _ = ctx.set_line_dash(&arr);
}

pub fn draw(app: &mut App) {
    let d = app.d.clone();
    let sp = &mut app.s.space;
    let Some((canvas, ctx)) = canvas() else {
        return;
    };
    let dpr = dom::window().device_pixel_ratio();
    let r = canvas.get_bounding_client_rect();
    let (w, h) = (r.width(), r.height());
    if canvas.width() != (w * dpr).round() as u32 {
        canvas.set_width((w * dpr).round() as u32);
        canvas.set_height((h * dpr).round() as u32);
    }
    let _ = ctx.set_transform(dpr, 0.0, 0.0, dpr, 0.0, 0.0);
    ctx.clear_rect(0.0, 0.0, w, h);
    let c = colors();
    let mono = dom::css_var("--mono");
    let cam = sp.cam;
    let p = |x: f64, y: f64, z: f64| project(&cam, w, h, x, y, z);
    let model = &sp.model;
    let status_color = |it: &Item| match it.status {
        Status::Added => c.added.as_str(),
        Status::Removed => c.removed.as_str(),
        Status::Changed => c.changed.as_str(),
        Status::Unchanged => c.muted.as_str(),
    };

    // Planes, far to near.
    let mut planes: Vec<(usize, [Screen; 4], f64)> = model
        .planes
        .iter()
        .enumerate()
        .map(|(i, pl)| {
            let corners = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)]
                .map(|(dx, dz)| p(pl.x + dx * pl.w / 2.0, pl.y, dz * pl.d / 2.0));
            let z = corners.iter().map(|c| c.3).sum::<f64>() / 4.0;
            (i, corners, z)
        })
        .collect();
    planes.sort_by(|a, b| a.2.total_cmp(&b.2));
    for (i, corners, _) in &planes {
        let pl = &model.planes[*i];
        let dimmed = sp.focus.as_ref().is_some_and(|f| *f != pl.module);
        ctx.begin_path();
        for (k, cr) in corners.iter().enumerate() {
            if k == 0 {
                ctx.move_to(cr.0, cr.1)
            } else {
                ctx.line_to(cr.0, cr.1)
            }
        }
        ctx.close_path();
        ctx.set_fill_style_str(&c.bg);
        ctx.set_global_alpha(if dimmed { 0.12 } else { 0.72 });
        ctx.fill();
        ctx.set_global_alpha(if dimmed { 0.25 } else { 1.0 });
        ctx.set_stroke_style_str(&c.module);
        ctx.set_line_width(1.4);
        let private_module = d
            .item(&pl.module)
            .is_some_and(|m| m.info.vis_scope.is_some());
        dash(&ctx, if private_module { &[6.0, 4.0] } else { &[] });
        ctx.stroke();
        dash(&ctx, &[]);
        // Divider between the exposed and the private part.
        let private_nodes: Vec<&Node> = pl
            .nodes
            .iter()
            .map(|&n| &model.nodes[n])
            .filter(|n| !n.exposed)
            .collect();
        if !private_nodes.is_empty() && pl.nodes.iter().any(|&n| model.nodes[n].exposed) {
            let z = private_nodes.iter().map(|n| n.lz).fold(f64::MIN, f64::max) + 35.0;
            let (a, b) = (p(pl.x - pl.w / 2.0, pl.y, z), p(pl.x + pl.w / 2.0, pl.y, z));
            dash(&ctx, &[3.0, 4.0]);
            ctx.set_stroke_style_str(&c.private);
            ctx.begin_path();
            ctx.move_to(a.0, a.1);
            ctx.line_to(b.0, b.1);
            ctx.stroke();
            dash(&ctx, &[]);
        }
        ctx.set_fill_style_str(&c.module);
        ctx.set_font(&format!("600 13px {mono}"));
        let lp = corners[0];
        let _ = ctx.fill_text(
            &format!("{}  · depth {}", d.mod_label(&pl.module), pl.depth),
            lp.0 + 6.0,
            lp.1 - 6.0,
        );
    }
    ctx.set_global_alpha(1.0);

    // Edges.
    for e in &model.edges {
        let (na, nb) = (&model.nodes[e.a], &model.nodes[e.b]);
        let (a, b) = (p(na.x, na.y, na.z), p(nb.x, nb.y, nb.z));
        let cross = na.plane != nb.plane;
        let focus = sp.focus.as_deref();
        let dimmed = focus.is_some_and(|f| {
            model.planes[na.plane].module != f && model.planes[nb.plane].module != f
        });
        let hot = sp.hover.is_some_and(|h| h == e.a || h == e.b);
        ctx.set_global_alpha(if dimmed {
            0.08
        } else if hot {
            1.0
        } else if cross {
            0.85
        } else {
            0.6
        });
        // Crossing into a module and landing on a private item is only legal
        // towards ancestors (children see their parents' private items).
        let color = if cross && !nb.exposed {
            &c.private
        } else if e.call {
            &c.func
        } else {
            &c.ty
        };
        ctx.set_stroke_style_str(color);
        ctx.set_line_width(if hot {
            2.6
        } else if cross {
            1.6
        } else {
            1.2
        });
        dash(
            &ctx,
            // Calls are solid, uses dotted, whether or not they cross planes.
            if !e.call { &[2.0, 4.0] } else { &[] },
        );
        ctx.begin_path();
        ctx.move_to(a.0, a.1);
        if cross {
            let (mx, my) = ((a.0 + b.0) / 2.0 + (b.1 - a.1) * 0.08, (a.1 + b.1) / 2.0);
            ctx.quadratic_curve_to(mx, my, b.0, b.1);
        } else {
            ctx.line_to(b.0, b.1);
        }
        ctx.stroke();
        dash(&ctx, &[]);
        let ang = (b.1 - a.1).atan2(b.0 - a.0);
        ctx.begin_path();
        ctx.move_to(
            b.0 - 9.0 * ang.cos() + 4.0 * ang.sin(),
            b.1 - 9.0 * ang.sin() - 4.0 * ang.cos(),
        );
        ctx.line_to(b.0 - 3.0 * ang.cos(), b.1 - 3.0 * ang.sin());
        ctx.line_to(
            b.0 - 9.0 * ang.cos() - 4.0 * ang.sin(),
            b.1 - 9.0 * ang.sin() + 4.0 * ang.cos(),
        );
        ctx.set_fill_style_str(color);
        ctx.fill();
    }
    ctx.set_global_alpha(1.0);

    // Nodes, far to near.
    let mut order: Vec<(usize, Screen)> = model
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (i, p(n.x, n.y, n.z)))
        .collect();
    order.sort_by(|a, b| a.1.3.total_cmp(&b.1.3));
    ctx.set_font(&format!("12px {mono}"));
    let crowded = model.nodes.len() > 60;
    sp.projected.clear();
    for (i, (x, y, s, _)) in order {
        let n = &model.nodes[i];
        let Some(it) = d.item(&n.id) else { continue };
        let plane = &model.planes[n.plane];
        let dimmed = sp.focus.as_ref().is_some_and(|f| *f != plane.module);
        ctx.set_global_alpha(if dimmed {
            0.2
        } else if n.context {
            0.55
        } else {
            1.0
        });
        let rad = (6.0 * s).max(3.0);
        ctx.begin_path();
        let _ = ctx.arc(x, y, rad, 0.0, std::f64::consts::TAU);
        if n.exposed {
            ctx.set_fill_style_str(status_color(it));
            ctx.fill();
        } else {
            ctx.set_fill_style_str(&c.bg);
            ctx.fill();
            ctx.set_stroke_style_str(status_color(it));
            ctx.set_line_width(2.0);
            ctx.stroke();
        }
        if sp.hover == Some(i) {
            ctx.set_stroke_style_str(&c.text);
            ctx.set_line_width(2.0);
            ctx.begin_path();
            let _ = ctx.arc(x, y, rad + 3.0, 0.0, std::f64::consts::TAU);
            ctx.stroke();
        }
        let show_label = !crowded
            || sp.hover == Some(i)
            || sp.focus.as_ref() == Some(&plane.module)
            || (!n.context && plane.nodes.len() <= 12);
        if show_label {
            ctx.set_fill_style_str(if n.context { &c.muted } else { &c.text });
            // The status as a sign too, not only as a color.
            let sign = match it.status {
                Status::Added => "+ ",
                Status::Removed => "− ",
                Status::Changed => "~ ",
                Status::Unchanged => "",
            };
            let label = format!("{sign}{}", d.display_name(it));
            let _ = ctx.fill_text(&label, x + rad + 4.0, y + 4.0);
        }
        sp.projected.push((i, x, y, rad));
    }
    ctx.set_global_alpha(1.0);
}

fn node_at(sp: &Space, x: f64, y: f64) -> Option<usize> {
    sp.projected
        .iter()
        .rev()
        .find(|(_, nx, ny, r)| (nx - x).powi(2) + (ny - y).powi(2) < (r + 4.0).powi(2))
        .map(|(i, ..)| *i)
}

fn render_bar(app: &App) {
    let d = &app.d;
    let sp = &app.s.space;
    let mut planes: Vec<&Plane> = sp.model.planes.iter().collect();
    planes.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.module.cmp(&b.module)));
    let options: String = planes
        .iter()
        .map(|p| {
            format!(
                r#"<option value="{}" {}>{}{}</option>"#,
                esc(&p.module),
                if sp.focus.as_ref() == Some(&p.module) {
                    "selected"
                } else {
                    ""
                },
                "· ".repeat(p.depth),
                esc(&d.mod_label(&p.module))
            )
        })
        .collect();
    dom::set_html(
        "space-bar",
        &format!(
            r#"<b>Module planes</b>
            <button data-preset="side">Side view</button><button data-preset="tilt">Tilted</button><button data-preset="top">From above</button>
            <label>focus <select id="space-focus"><option value="">all planes</option>{options}</select></label>
            <label><input type="checkbox" id="space-uses" {}> uses</label>
            <label><input type="checkbox" id="space-unchanged" {}> all items</label>
            <span style="flex:1"></span><span class="dim">drag to rotate · wheel to zoom · click a node to open it</span>"#,
            if sp.show_uses { "checked" } else { "" },
            if sp.show_unchanged { "checked" } else { "" },
        ),
    );
    dom::set_html(
        "space-legend",
        r#"+ added &nbsp; ~ changed &nbsp; − removed<br>● exposed item &nbsp; ○ private item &nbsp; <span style="color:var(--fn)">━</span> call &nbsp; <span style="color:var(--type)">┅</span> use<br>Edges between planes cross a module boundary and land on exposed items;<br>grey ones reach a private item of an ancestor module."#,
    );
}

pub fn show(app: &mut App) {
    let model = build(&app.d, &app.s.space);
    app.s.space.model = model;
    render_bar(app);
    let width = dom::by_id("space").map_or(1000.0, |c| c.get_bounding_client_rect().width());
    let sp = &mut app.s.space;
    let xs: Vec<f64> = sp
        .model
        .planes
        .iter()
        .flat_map(|p| [p.x - p.w / 2.0, p.x + p.w / 2.0])
        .collect();
    let span = (xs.iter().copied().fold(f64::MIN, f64::max)
        - xs.iter().copied().fold(f64::MAX, f64::min))
    .max(600.0)
    .max(sp.model.planes.len() as f64 * 120.0);
    sp.fit_zoom = (width * 0.85 / span).min(1.4);
    let target = camera_for(sp, sp.cam.yaw, sp.cam.pitch);
    sp.cam = target;
    draw(app);
}

/// Turns the camera to `yaw`/`pitch`. Takes the start from the caller,
/// which holds the app state already.
/// Moves the camera smoothly to `target`. Takes the start from the caller,
/// which holds the app state already.
fn animate_to(start: Camera, target: Camera) {
    let t0 = dom::window().performance().map_or(0.0, |p| p.now());
    fn step(start: Camera, target: Camera, t0: f64) {
        dom::raf(move || {
            let now = dom::window().performance().map_or(t0 + 1000.0, |p| p.now());
            let k = ((now - t0) / 450.0).min(1.0);
            let e = k * (2.0 - k);
            let mix = |a: f64, b: f64| a + (b - a) * e;
            crate::with_app(|app| {
                app.s.space.cam = Camera {
                    yaw: mix(start.yaw, target.yaw),
                    pitch: mix(start.pitch, target.pitch),
                    dist: start.dist,
                    zoom: mix(start.zoom, target.zoom),
                    cx: mix(start.cx, target.cx),
                    cy: mix(start.cy, target.cy),
                };
                draw(app);
            });
            if k < 1.0 {
                step(start, target, t0);
            }
        });
    }
    step(start, target, t0);
}

/// A camera looking at the whole model (`yaw`, `pitch`) or, with focus, at
/// one plane from above, zoomed to fit it.
fn camera_for(sp: &Space, yaw: f64, pitch: f64) -> Camera {
    let width = dom::by_id("space").map_or(1000.0, |c| c.get_bounding_client_rect().width());
    let focused = sp
        .focus
        .as_ref()
        .and_then(|f| sp.model.planes.iter().find(|p| &p.module == f));
    match focused {
        Some(p) => Camera {
            yaw: 0.0,
            pitch: 1.5,
            dist: sp.cam.dist,
            zoom: (width * 0.8 / (p.w + 300.0)).clamp(0.3, 2.5),
            cx: p.x,
            cy: p.y,
        },
        None => Camera {
            yaw,
            pitch,
            dist: sp.cam.dist,
            zoom: sp.fit_zoom,
            cx: 0.0,
            cy: 0.0,
        },
    }
}

pub fn on_click(app: &mut App, el: &Element) -> bool {
    let Some(b) = dom::closest(el, "[data-preset]") else {
        return false;
    };
    let sp = &mut app.s.space;
    let (yaw, pitch) = match dom::data(&b, "preset").as_deref() {
        Some("side") => (-0.25, 0.08),
        Some("tilt") => (-0.6, 0.55),
        Some("top") => (0.0, 1.5),
        _ => return false,
    };
    // A preset looks at the whole model again.
    sp.focus = None;
    if let Some(select) =
        dom::by_id("space-focus").and_then(|e| e.dyn_into::<web_sys::HtmlSelectElement>().ok())
    {
        select.set_value("");
    }
    let target = camera_for(sp, yaw, pitch);
    animate_to(sp.cam, target);
    true
}

pub fn on_change(app: &mut App, el: &Element) -> bool {
    match el.id().as_str() {
        "space-focus" => {
            let value = el
                .dyn_ref::<web_sys::HtmlSelectElement>()
                .map(|s| s.value())
                .unwrap_or_default();
            let sp = &mut app.s.space;
            sp.focus = (!value.is_empty()).then_some(value);
            let target = camera_for(sp, -0.45, 0.42);
            animate_to(sp.cam, target);
        }
        "space-uses" | "space-unchanged" => {
            let on = el
                .dyn_ref::<web_sys::HtmlInputElement>()
                .is_some_and(|i| i.checked());
            if el.id() == "space-uses" {
                app.s.space.show_uses = on
            } else {
                app.s.space.show_unchanged = on
            }
            show(app);
        }
        _ => return false,
    }
    true
}

pub fn on_pointer_down(app: &mut App, el: &Element, ev: &PointerEvent) -> bool {
    if el.id() != "space" {
        return false;
    }
    let sp = &mut app.s.space;
    sp.drag = Some((
        ev.client_x() as f64,
        ev.client_y() as f64,
        sp.cam.yaw,
        sp.cam.pitch,
        false,
    ));
    true
}

pub fn on_pointer_move(app: &mut App, ev: &PointerEvent) {
    let (x, y) = (ev.client_x() as f64, ev.client_y() as f64);
    if let Some((sx, sy, yaw, pitch, moved)) = app.s.space.drag {
        let (dx, dy) = (x - sx, y - sy);
        let sp = &mut app.s.space;
        sp.drag = Some((sx, sy, yaw, pitch, moved || dx.abs() + dy.abs() > 3.0));
        sp.cam.yaw = yaw + dx * 0.006;
        sp.cam.pitch = (pitch + dy * 0.005).clamp(-0.2, 1.55);
        draw(app);
        return;
    }
    let Some(canvas) = dom::by_id("space") else {
        return;
    };
    let r = canvas.get_bounding_client_rect();
    let over = ev
        .target()
        .and_then(|t| t.dyn_into::<Element>().ok())
        .is_some_and(|e| e.id() == "space");
    let hit = if over {
        node_at(&app.s.space, x - r.left(), y - r.top())
    } else {
        None
    };
    if hit != app.s.space.hover {
        app.s.space.hover = hit;
        draw(app);
        match hit {
            Some(n) => {
                let d = app.d.clone();
                if let Some(it) = d.item(&app.s.space.model.nodes[n].id) {
                    crate::show_tooltip(&format!(
                        r#"<div class="tp">{}</div><div>{} · {} {}</div><pre>{}</pre>"#,
                        esc(&it.info.id),
                        it.info.kind.label(),
                        d.status_html(it),
                        d.vis_html(it),
                        esc(&it.info.sig.lines().take(6).collect::<Vec<_>>().join("\n"))
                    ));
                }
            }
            None => crate::hide_tooltip(),
        }
    }
}

pub fn on_pointer_up(app: &mut App, ev: &PointerEvent) {
    let Some((.., moved)) = app.s.space.drag.take() else {
        return;
    };
    if moved {
        return;
    }
    let Some(canvas) = dom::by_id("space") else {
        return;
    };
    let r = canvas.get_bounding_client_rect();
    if let Some(n) = node_at(
        &app.s.space,
        ev.client_x() as f64 - r.left(),
        ev.client_y() as f64 - r.top(),
    ) {
        crate::hide_tooltip();
        let id = app.s.space.model.nodes[n].id.clone();
        crate::canvas::open(app, &id, None, true);
    }
}

pub fn on_wheel(app: &mut App, ev: &WheelEvent) {
    ev.prevent_default();
    let cam = &mut app.s.space.cam;
    cam.zoom = (cam.zoom * (-ev.delta_y() * 0.0012).exp()).clamp(0.3, 3.0);
    draw(app);
}
