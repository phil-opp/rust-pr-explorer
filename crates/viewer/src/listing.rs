//! The overview's list of every changed item with its diff, after the
//! summary of the changed call graphs: a linear review like GitHub's
//! "Files changed", but item by item.
//!
//! Each call graph gets a section with its functions in call order (as
//! the graph lists them), then the types and constants they use, then
//! the changed tests and test helpers that reach it. An item that several
//! graphs list (a shared type, a test of two graphs) is shown under the
//! first. The changes that no graph lists follow in sections of their
//! own: other tests, other types and constants, removed items and the
//! rest (such as trait impls and module docs).
//!
//! Impls and the members of a type follow the item they belong to when
//! that is listed, so that `impl Display for Token` comes right after
//! `Token`. Every review unit (see `review::is_unit_item`) is placed
//! somewhere, so that nothing is only reachable through other views.

use crate::code::esc;
use crate::data::{Data, changed};
use crate::graphs::{Graph, Graphs};
use model::{Item, Kind};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, IntersectionObserver, IntersectionObserverEntry};

/// What a section of the list holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The `i`th changed call graph.
    Graph(usize),
    /// Changed tests and test helpers that reach no graph.
    Tests,
    /// Changed types and constants that no graph uses.
    Types,
    /// Removed items outside tests.
    Removed,
    /// Everything else: impls of unlisted types, module docs, items of
    /// other crates.
    Other,
}

/// A section: groups of items, each an item followed by the impls and
/// members that belong to it.
#[derive(Debug)]
pub struct Section {
    pub part: Part,
    pub groups: Vec<Vec<String>>,
}

/// Where each changed item goes on the overview.
pub fn sections(d: &Data, gs: &Graphs) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    // Section and group of every placed item.
    let mut at: HashMap<String, (usize, usize)> = HashMap::new();
    for (i, g) in gs.graphs.iter().enumerate() {
        for id in graph_ids(d, g) {
            head(&mut out, &mut at, Part::Graph(i), id);
        }
    }
    // The rest: next to what it belongs to, if that is listed.
    let mut rest: Vec<(Part, &str)> = Vec::new();
    rest.extend(gs.tests.iter().map(|id| (Part::Tests, id.as_str())));
    rest.extend(gs.other.iter().map(|id| (Part::Types, id.as_str())));
    rest.extend(gs.removed.iter().map(|id| (Part::Removed, id.as_str())));
    let mut unlisted: Vec<&Item> = d
        .items()
        .filter(|it| shown(it))
        .filter(|it| !at.contains_key(&it.info.id))
        .filter(|it| !rest.iter().any(|(_, r)| *r == it.info.id))
        .collect();
    unlisted.sort_by(|a, b| d.reading_order(a, b));
    rest.extend(unlisted.iter().map(|it| (Part::Other, it.info.id.as_str())));
    for (part, id) in rest {
        if at.contains_key(id) {
            continue;
        }
        match owner(d, id, &at) {
            Some((s, g)) => {
                out[s].groups[g].push(id.to_string());
                at.insert(id.to_string(), (s, g));
            }
            None => head(&mut out, &mut at, part, id),
        }
    }
    // What belongs to an item in reading order, after it.
    for s in &mut out {
        for g in &mut s.groups {
            g[1..].sort_by(|a, b| match (d.item(a), d.item(b)) {
                (Some(a), Some(b)) => d.reading_order(a, b),
                _ => a.cmp(b),
            });
        }
    }
    out
}

/// Places `id` as the head of a new group of the last section, or of a
/// new section when that is of another part.
fn head(out: &mut Vec<Section>, at: &mut HashMap<String, (usize, usize)>, part: Part, id: &str) {
    if at.contains_key(id) {
        return;
    }
    if out.last().is_none_or(|s| s.part != part) {
        out.push(Section {
            part,
            groups: Vec::new(),
        });
    }
    let s = out.len() - 1;
    out[s].groups.push(vec![id.to_string()]);
    at.insert(id.to_string(), (s, out[s].groups.len() - 1));
}

/// What a graph's section lists, in order: its functions, the types and
/// constants they use, and its changed tests and test helpers.
fn graph_ids<'a>(d: &Data, g: &'a Graph) -> Vec<&'a str> {
    let tests = g
        .tests
        .iter()
        .chain(&g.helpers)
        .filter(|t| d.item(t).is_some_and(changed));
    g.rows
        .iter()
        .map(|r| &r.id)
        .chain(&g.types)
        .chain(tests)
        .map(String::as_str)
        .collect()
}

/// Whether the overview shows an item in full: every review unit, and
/// modules whose docs changed.
fn shown(it: &Item) -> bool {
    if it.info.kind == Kind::Mod {
        return changed(it) && it.changes.docs;
    }
    crate::review::is_unit_item(it)
}

/// The group of the listed item that `id` belongs to: its impl, trait or
/// type, or theirs (a method of a trait impl goes with the type); for an
/// impl whose type isn't listed, the group of one of its members.
fn owner(d: &Data, id: &str, at: &HashMap<String, (usize, usize)>) -> Option<(usize, usize)> {
    let mut cur = d.item(id)?;
    for _ in 0..4 {
        let up = [&cur.info.parent, &cur.info.self_ty]
            .into_iter()
            .flatten()
            .filter(|o| *o != &cur.info.id)
            .filter_map(|o| d.item(o))
            .find(|o| o.info.kind != Kind::Mod);
        let Some(up) = up else { break };
        if let Some(&g) = at.get(&up.info.id) {
            return Some(g);
        }
        cur = up;
    }
    let it = d.item(id)?;
    if it.info.kind == Kind::Impl {
        return d
            .members(it)
            .iter()
            .find_map(|m| at.get(&m.info.id).copied());
    }
    None
}

/// Whether the overview lists item `id` in full.
pub fn lists(d: &Data, id: &str) -> bool {
    sections(d, &crate::graphs::graphs(d))
        .iter()
        .flat_map(|s| &s.groups)
        .any(|g| g.iter().any(|x| x == id))
}

/// The sections as HTML, for the overview. Empty without changed Rust
/// items.
pub fn overview_html(d: &Data, gs: &Graphs) -> String {
    let secs = sections(d, gs);
    if secs.is_empty() {
        return String::new();
    }
    let body: String = secs.iter().map(|s| section_html(d, gs, s)).collect();
    format!(
        r#"<h2>Changed items {}</h2>{body}"#,
        crate::pages::help_html(
            "Every changed item with its diff, by call graph: each graph's functions in call order, then the types and constants they use and the tests that reach it. An item that several graphs list is shown under the first. Changes that no graph lists come last. Impls and members follow the item they belong to."
        )
    )
}

fn section_html(d: &Data, gs: &Graphs, s: &Section) -> String {
    let groups: String = s.groups.iter().map(|g| group_html(d, g)).collect();
    let (id, head) = match s.part {
        Part::Graph(i) => (format!("cgitems-{i}"), graph_head(d, i, &gs.graphs[i])),
        part => {
            let (key, title, what) = match part {
                Part::Tests => ("tests", "Other changed tests", "reach no graph"),
                Part::Types => (
                    "types",
                    "Other changed types and constants",
                    "used by no graph",
                ),
                Part::Removed => ("removed", "Removed", ""),
                _ => ("other", "Other changes", "impls, module docs and more"),
            };
            (
                format!("cgitems-{key}"),
                format!(
                    r#"<span class="cgi-title">{title}</span> <span class="dim">{what}</span>"#
                ),
            )
        }
    };
    format!(r#"<section class="cgi" id="{id}"><div class="cgi-h">{head}</div>{groups}</section>"#)
}

/// A graph section's heading: its roots, with links to the graph on the
/// canvas and in the call view, and back to its summary.
fn graph_head(d: &Data, i: usize, g: &Graph) -> String {
    let roots: String = g
        .roots
        .iter()
        .take(3)
        .map(|r| format!("<code>{}</code>", esc(&d.display_name_of(r))))
        .collect::<Vec<_>>()
        .join(r#"<span class="dim"> · </span>"#);
    let more = match g.roots.len() {
        n if n > 3 => format!(r#" <span class="dim">+{} more</span>"#, n - 3),
        _ => String::new(),
    };
    let canvas = if crate::narrow() {
        String::new()
    } else {
        format!(
            r#"<a class="btn" data-cgraph="{i}"{} title="Open the graph's functions on the canvas">Canvas ▸</a>"#,
            crate::nav::href(&crate::nav::Route::Canvas(Some(
                crate::nav::OnCanvas::Items(g.rows.iter().map(|r| r.id.clone()).collect())
            ))),
        )
    };
    let calls = g.roots.first().map_or(String::new(), |r| {
        format!(
            r#"<a class="btn" data-calls-focus="{}"{} title="Show the graph's root in the call view">Calls ▸</a>"#,
            esc(r),
            crate::nav::href(&crate::nav::Route::Calls(Some(r.clone()))),
        )
    });
    format!(
        r#"<span class="cgi-title"><span class="dim">Call graph</span> {roots}{more}</span><span class="grow"></span><span class="cgi-acts"><a class="btn" data-cgraph-summary="{i}" title="The graph's summary above">▴ Summary</a>{canvas}{calls}</span>"#
    )
}

/// An item and what belongs to it, as one card.
fn group_html(d: &Data, ids: &[String]) -> String {
    let rows: String = ids
        .iter()
        .enumerate()
        .filter_map(|(k, id)| Some((k, d.item(id)?)))
        .map(|(k, it)| row_html(d, it, k > 0))
        .collect();
    format!(r#"<div class="ientry">{rows}</div>"#)
}

thread_local! {
    /// Renders the bodies of the rows that come near the view.
    static OBSERVER: RefCell<Option<IntersectionObserver>> = const { RefCell::new(None) };
    /// The data the rows are rendered from.
    static DATA: RefCell<Option<Rc<Data>>> = const { RefCell::new(None) };
    /// The rows whose bodies were rendered: the next render of the
    /// overview renders them right away, so that the page has the same
    /// height and Back returns to the same place.
    static FILLED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Whether the overview showed the body of item `id`'s row before (see
/// `FILLED`).
pub fn was_filled(id: &str) -> bool {
    FILLED.with(|f| f.borrow().contains(id))
}

/// How far ahead of the view, above and below, row bodies are rendered.
const AHEAD: &str = "1500px 0px";

/// Renders the bodies of the overview's rows (signature, docs, diff) once
/// they come near the view, instead of all at once: on a PR with a
/// thousand changed items, rendering every body takes seconds and the
/// page holds hundreds of thousands of elements. Called after each render
/// of the overview, which replaces the rows.
pub fn observe(d: &Rc<Data>) {
    DATA.with(|x| *x.borrow_mut() = Some(d.clone()));
    let Some(root) = crate::dom::by_id("view-overview") else {
        return;
    };
    OBSERVER.with(|o| {
        let mut o = o.borrow_mut();
        if o.is_none() {
            let cb = Closure::<dyn FnMut(js_sys::Array, IntersectionObserver)>::new(
                |entries: js_sys::Array, obs: IntersectionObserver| {
                    for e in entries
                        .iter()
                        .filter_map(|e| e.dyn_into::<IntersectionObserverEntry>().ok())
                        .filter(|e| e.is_intersecting())
                    {
                        let el = e.target();
                        obs.unobserve(&el);
                        fill(&el);
                    }
                },
            );
            let init = web_sys::IntersectionObserverInit::new();
            init.set_root(Some(&root));
            init.set_root_margin(AHEAD);
            *o = IntersectionObserver::new_with_options(cb.as_ref().unchecked_ref(), &init).ok();
            cb.forget();
        }
        let Some(o) = o.as_ref() else { return };
        o.disconnect();
        for el in crate::dom::query_all_in(&root.clone().into(), ".lazy-body") {
            o.observe(&el);
        }
    });
}

/// Renders the bodies of the rows in `el` (or of `el` itself) that are
/// still waiting, e.g. before the page scrolls to a row.
pub fn fill_in(el: &Element) {
    let mut waiting = crate::dom::query_all_in(&el.clone().into(), ".lazy-body");
    if el.class_list().contains("lazy-body") {
        waiting.push(el.clone());
    }
    for w in waiting {
        OBSERVER.with(|o| {
            if let Some(o) = o.borrow().as_ref() {
                o.unobserve(&w);
            }
        });
        fill(&w);
    }
}

fn fill(placeholder: &Element) {
    let Some(id) = crate::dom::data(placeholder, "lazy") else {
        return;
    };
    let html = DATA.with(|x| {
        let d = x.borrow();
        let d = d.as_ref()?;
        let it = d.item(&id)?;
        Some(crate::pages::row_body(d, it, &it.info.module))
    });
    FILLED.with(|f| f.borrow_mut().insert(id));
    placeholder.set_outer_html(&html.unwrap_or_default());
}

fn row_html(d: &Data, it: &Item, member: bool) -> String {
    if it.info.kind == Kind::Impl {
        return crate::pages::impl_line(d, it, true);
    }
    crate::pages::overview_row(d, it, member)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Call, ItemInfo, Meta, Output, Status};

    fn item(id: &str, kind: Kind, status: Status, calls: &[&str]) -> (String, Item) {
        let info = ItemInfo {
            id: id.into(),
            kind,
            name: id.rsplit("::").next().unwrap().into(),
            module: "c".into(),
            calls: calls.iter().map(|t| Call { to: t.to_string() }).collect(),
            ..Default::default()
        };
        (
            id.into(),
            Item {
                info,
                status,
                ..Default::default()
            },
        )
    }

    fn data(items: Vec<(String, Item)>) -> Data {
        Data::new(Output {
            meta: Meta::default(),
            crates: vec!["c".into()],
            items: items.into_iter().collect(),
            externals: Default::default(),
            files: Vec::new(),
            warnings: Vec::new(),
            semver_checks: None,
        })
    }

    fn listed(secs: &[Section]) -> Vec<(Part, Vec<Vec<&str>>)> {
        secs.iter()
            .map(|s| {
                let groups = s
                    .groups
                    .iter()
                    .map(|g| g.iter().map(String::as_str).collect())
                    .collect();
                (s.part, groups)
            })
            .collect()
    }

    #[test]
    fn every_change_is_listed_once_by_graph() {
        use Status::*;
        let mut test = item("c::tests::t", Kind::Fn, Added, &["c::run"]);
        test.1.info.test = true;
        let mut lone = item("c::tests::lone", Kind::Fn, Added, &[]);
        lone.1.info.test = true;
        // A trait impl of a type that no graph uses, with a changed method.
        let mut imp = item("c::impl-Display-Tok", Kind::Impl, Added, &[]);
        imp.1.info.self_ty = Some("c::Tok".into());
        imp.1.info.children = vec!["c::impl-Display-Tok::fmt".into()];
        let mut fmt = item("c::impl-Display-Tok::fmt", Kind::Method, Added, &[]);
        fmt.1.info.parent = Some("c::impl-Display-Tok".into());
        fmt.1.info.self_ty = Some("c::Tok".into());
        // Uses a type that the second graph uses too.
        let mut run = item("c::run", Kind::Fn, Changed, &["c::step"]);
        run.1.info.uses.insert(model::Use {
            to: "c::Ty".into(),
            ctx: model::UseCtx::Body,
        });
        let mut other = item("c::other", Kind::Fn, Changed, &[]);
        other.1.info.uses = run.1.info.uses.clone();
        let d = data(vec![
            run,
            item("c::step", Kind::Fn, Added, &[]),
            other,
            item("c::Ty", Kind::Struct, Changed, &[]),
            item("c::Tok", Kind::Struct, Unchanged, &[]),
            imp,
            fmt,
            item("c::gone", Kind::Fn, Removed, &[]),
            test,
            lone,
        ]);
        let gs = crate::graphs::graphs(&d);
        let secs = sections(&d, &gs);
        assert_eq!(
            listed(&secs),
            vec![
                (
                    Part::Graph(0),
                    vec![
                        vec!["c::run"],
                        vec!["c::step"],
                        vec!["c::Ty"],
                        vec!["c::tests::t"]
                    ]
                ),
                // `fmt` is a graph of its own (nothing calls it), and the
                // impl goes with it, since its type isn't listed.
                (
                    Part::Graph(1),
                    vec![vec!["c::impl-Display-Tok::fmt", "c::impl-Display-Tok"]]
                ),
                // `Ty` is shown under the first graph only.
                (Part::Graph(2), vec![vec!["c::other"]]),
                (Part::Tests, vec![vec!["c::tests::lone"]]),
                (Part::Removed, vec![vec!["c::gone"]]),
            ]
        );
        let units = d
            .items()
            .filter(|it| crate::review::is_unit_item(it))
            .count();
        let placed: usize = secs.iter().flat_map(|s| &s.groups).map(Vec::len).sum();
        assert_eq!(placed, units);
    }

    #[test]
    fn impls_follow_their_listed_type() {
        use Status::*;
        let mut imp = item("c::impl-Tok", Kind::Impl, Changed, &[]);
        imp.1.info.self_ty = Some("c::Tok".into());
        let d = data(vec![item("c::Tok", Kind::Struct, Added, &[]), imp]);
        let gs = crate::graphs::graphs(&d);
        assert_eq!(
            listed(&sections(&d, &gs)),
            vec![(Part::Types, vec![vec!["c::Tok", "c::impl-Tok"]])]
        );
    }
}
