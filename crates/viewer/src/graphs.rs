//! "Changed call graphs": the overview's summary of a PR as the call
//! graphs it adds or changes, instead of the files it touches.
//!
//! A graph starts at a changed function that no other changed function
//! calls (its root) and holds the changed functions the root reaches. A
//! call through one unchanged item still counts (`on_change → save →
//! encode`), so that a small unchanged wrapper doesn't split a graph.
//! Graphs that share a changed function are merged, so that every
//! function is listed once. Types and constants don't merge graphs: they
//! are shared vocabulary, and one widely used type would otherwise glue
//! the whole PR together. Tests don't start graphs of their own either;
//! they are listed under the graphs they reach.
//!
//! Only resolved calls are followed, so a function called only through a
//! trait or from inside a macro starts a graph of its own.

use crate::code::esc;
use crate::data::{Data, changed};
use model::{Item, Kind, Status};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

/// Rows of a graph shown before the rest is folded.
const SHOWN_ROWS: usize = 12;
/// Entry points named on a graph's "Reached from" line.
const SHOWN_ENTRIES: usize = 4;
/// Deeper calls are indented like this depth, so that a long chain
/// still fits on a phone.
const MAX_INDENT: usize = 5;
/// How far a test's calls are followed to find the graphs it tests.
const TEST_DEPTH: usize = 4;

/// One changed call graph.
#[derive(Debug, Default)]
pub struct Graph {
    /// Changed functions that no other changed function calls, in reading
    /// order.
    pub roots: Vec<String>,
    /// The changed functions in call order, roots first.
    pub rows: Vec<Row>,
    /// Changed types and constants that the functions use.
    pub types: Vec<String>,
    /// Test functions that reach the graph.
    pub tests: Vec<String>,
}

/// A changed function of a graph.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    /// Calls from its root, 0 for a root.
    pub depth: usize,
    /// The unchanged item through which its caller in the graph calls it.
    pub via: Option<String>,
}

/// The changed call graphs of the PR, and the changes they don't cover.
#[derive(Debug, Default)]
pub struct Graphs {
    /// Largest first.
    pub graphs: Vec<Graph>,
    /// Changed tests and test helpers not listed under a graph.
    pub tests: Vec<String>,
    /// Changed types and constants no graph uses.
    pub other: Vec<String>,
    /// Removed items outside tests.
    pub removed: Vec<String>,
}

fn is_test(it: &Item) -> bool {
    it.info.test || it.info.test_only
}

/// A changed item the summary lists: no modules and impls (their members
/// are listed), no items of other crates.
fn listed(d: &Data, it: &Item) -> bool {
    changed(it)
        && !matches!(it.info.kind, Kind::Mod | Kind::Impl)
        && d.external(&it.info.id).is_none()
}

/// A function that can be part of a graph.
fn node(d: &Data, it: &Item) -> bool {
    listed(d, it) && it.status != Status::Removed && it.info.kind.is_callable() && !is_test(it)
}

/// The items that call or use `id`, removed ones left out.
fn callers_of<'a>(d: &'a Data, id: &str) -> Vec<&'a str> {
    let callers = d.callers.get(id).into_iter().flatten();
    let users = d.used_by.get(id).into_iter().flatten().map(|(u, _)| u);
    let mut v: Vec<&str> = callers
        .chain(users)
        .map(String::as_str)
        .filter(|u| *u != id)
        .filter(|u| d.item(u).is_some_and(|it| it.status != Status::Removed))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// What an item calls, uses or names, in the order it does so in its
/// source; calls and uses it doesn't name there (e.g. from inside a macro)
/// come last.
fn callees_in_order(d: &Data, it: &Item) -> Vec<String> {
    let mut at: Vec<((u32, u32), &str)> = it
        .info
        .source_refs
        .iter()
        .filter_map(|(pos, r)| {
            let (l, c) = pos.split_once(':')?;
            Some(((l.parse().ok()?, c.parse().ok()?), r.id.as_deref()?))
        })
        .collect();
    at.sort();
    let named = at.into_iter().map(|(_, id)| id);
    let rest = it
        .info
        .calls
        .iter()
        .map(|c| c.to.as_str())
        .chain(it.info.uses.iter().map(|u| u.to.as_str()));
    let mut seen = HashSet::new();
    named
        .chain(rest)
        .filter(|id| *id != it.info.id && d.item(id).is_some() && seen.insert(*id))
        .map(str::to_string)
        .collect()
}

/// Groups the PR's changed functions into call graphs.
pub fn graphs(d: &Data) -> Graphs {
    let nodes: BTreeSet<&str> = d
        .items()
        .filter(|it| node(d, it))
        .map(|it| it.info.id.as_str())
        .collect();
    // The graph's edges, from caller to callee: direct, or through one
    // unchanged item.
    let mut kids: HashMap<&str, BTreeMap<&str, Option<&str>>> = HashMap::new();
    let mut parents: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for &c in &nodes {
        for p in callers_of(d, c) {
            let mut add = |p: &'_ str, via: Option<&'_ str>| {
                // Through the item's own id, to tie the lifetimes to `d`.
                let (Some(pi), Some(ci)) = (d.item(p), d.item(c)) else {
                    return;
                };
                let (p, c) = (pi.info.id.as_str(), ci.info.id.as_str());
                let via = via.and_then(|v| d.item(v)).map(|v| v.info.id.as_str());
                let e = kids.entry(p).or_default().entry(c).or_insert(via);
                if via.is_none() {
                    *e = None;
                }
                parents.entry(c).or_default().insert(p);
            };
            if nodes.contains(p) {
                add(p, None);
            } else if d.item(p).is_some_and(|it| !changed(it) && !is_test(it)) {
                for pp in callers_of(d, p) {
                    if nodes.contains(pp) && pp != c {
                        add(pp, Some(p));
                    }
                }
            }
        }
    }
    let reach = |r| {
        let mut seen: HashSet<&str> = HashSet::from([r]);
        let mut queue = VecDeque::from([r]);
        while let Some(x) = queue.pop_front() {
            for &k in kids.get(x).into_iter().flat_map(|m| m.keys()) {
                if seen.insert(k) {
                    queue.push_back(k);
                }
            }
        }
        seen
    };
    let reached: HashMap<&str, HashSet<&str>> = nodes.iter().map(|&n| (n, reach(n))).collect();
    // A root: every changed caller is one it reaches itself (a cycle).
    // Of a cycle, only the first in reading order.
    let mut sorted: Vec<&Item> = nodes.iter().filter_map(|n| d.item(n)).collect();
    sorted.sort_by(|a, b| d.reading_order(a, b));
    let mut roots: Vec<&str> = Vec::new();
    for it in &sorted {
        let n = it.info.id.as_str();
        let own = &reached[n];
        let top = parents
            .get(n)
            .is_none_or(|ps| ps.iter().all(|p| own.contains(p)));
        if top && !roots.iter().any(|r| reached[r].contains(n)) {
            roots.push(n);
        }
    }
    // Merge roots that reach a common function.
    let mut group_of: HashMap<&str, usize> = HashMap::new();
    let mut groups: Vec<Vec<&str>> = Vec::new();
    for &r in &roots {
        let mut hit: Vec<usize> = reached[r]
            .iter()
            .filter_map(|n| group_of.get(n).copied())
            .collect();
        hit.sort();
        hit.dedup();
        let g = match hit.first() {
            Some(&g) => {
                // Fold the other groups into the first.
                for &o in hit[1..].iter().rev() {
                    let moved = std::mem::take(&mut groups[o]);
                    groups[g].extend(moved);
                }
                g
            }
            None => {
                groups.push(Vec::new());
                groups.len() - 1
            }
        };
        groups[g].push(r);
        for &o in &groups[g] {
            for &n in &reached[o] {
                group_of.insert(n, g);
            }
        }
    }
    let order: HashMap<&str, usize> = roots.iter().enumerate().map(|(i, r)| (*r, i)).collect();
    let mut graphs: Vec<Graph> = groups
        .into_iter()
        .filter(|g| !g.is_empty())
        .map(|mut g| {
            g.sort_by_key(|r| order[r]);
            build(d, &g, &kids)
        })
        .collect();
    graphs.sort_by_key(|g| std::cmp::Reverse(g.rows.len()));

    // Types and constants, under every graph that uses them.
    let mut used = HashSet::new();
    for g in &mut graphs {
        let rows: Vec<&Item> = g.rows.iter().filter_map(|r| d.item(&r.id)).collect();
        let mut types: Vec<&Item> = rows
            .iter()
            .flat_map(|it| callees_in_order(d, it))
            .filter_map(|id| d.item(&id))
            .filter(|t| listed(d, t) && !t.info.kind.is_callable() && !is_test(t))
            .filter(|t| t.status != Status::Removed)
            .collect();
        types.sort_by(|a, b| d.reading_order(a, b));
        types.dedup_by(|a, b| a.info.id == b.info.id);
        g.types = types.iter().map(|t| t.info.id.clone()).collect();
        used.extend(g.types.iter().cloned());
    }

    // Tests, under every graph they reach.
    let mut tests: Vec<&Item> = d.items().filter(|it| it.info.test).collect();
    tests.sort_by(|a, b| d.reading_order(a, b));
    let mut shown_tests = HashSet::new();
    let graph_of: HashMap<String, usize> = graphs
        .iter()
        .enumerate()
        .flat_map(|(i, g)| g.rows.iter().map(move |r| (r.id.clone(), i)))
        .collect();
    for t in &tests {
        let mut hit: BTreeSet<usize> = BTreeSet::new();
        let mut seen = HashSet::from([t.info.id.as_str()]);
        let mut queue = VecDeque::from([(t.info.id.as_str(), 0)]);
        while let Some((x, depth)) = queue.pop_front() {
            if let Some(&g) = graph_of.get(x) {
                hit.insert(g);
            }
            let Some(it) = d.item(x) else { continue };
            if depth == TEST_DEPTH {
                continue;
            }
            for c in callees_in_order(d, it) {
                if let Some(ci) = d.item(&c)
                    && ci.info.kind.is_callable()
                    && seen.insert(ci.info.id.as_str())
                {
                    queue.push_back((ci.info.id.as_str(), depth + 1));
                }
            }
        }
        for g in hit {
            graphs[g].tests.push(t.info.id.clone());
            shown_tests.insert(t.info.id.as_str());
        }
    }

    let mut rest: Vec<&Item> = d
        .items()
        .filter(|it| listed(d, it))
        .filter(|it| !nodes.contains(it.info.id.as_str()))
        .collect();
    rest.sort_by(|a, b| d.reading_order(a, b));
    let mut out = Graphs {
        graphs,
        ..Default::default()
    };
    for it in rest {
        let id = it.info.id.clone();
        if is_test(it) {
            if !shown_tests.contains(id.as_str()) {
                out.tests.push(id);
            }
        } else if it.status == Status::Removed {
            out.removed.push(id);
        } else if !used.contains(&id) {
            out.other.push(id);
        }
    }
    out
}

/// A graph's rows: depth first from each root, in call order, each
/// function once.
fn build(d: &Data, roots: &[&str], kids: &HashMap<&str, BTreeMap<&str, Option<&str>>>) -> Graph {
    fn visit(
        d: &Data,
        kids: &HashMap<&str, BTreeMap<&str, Option<&str>>>,
        id: &str,
        depth: usize,
        via: Option<&str>,
        seen: &mut HashSet<String>,
        rows: &mut Vec<Row>,
    ) {
        if !seen.insert(id.to_string()) {
            return;
        }
        rows.push(Row {
            id: id.to_string(),
            depth,
            via: via.map(str::to_string),
        });
        let Some(next) = kids.get(id) else { return };
        let Some(it) = d.item(id) else { return };
        // In the order the function calls them (or the item in between).
        let order: HashMap<String, usize> = callees_in_order(d, it)
            .into_iter()
            .enumerate()
            .map(|(i, c)| (c, i))
            .collect();
        let mut next: Vec<(&str, Option<&str>)> = next.iter().map(|(k, v)| (*k, *v)).collect();
        next.sort_by_key(|(k, via)| order.get(via.unwrap_or(k)).copied().unwrap_or(usize::MAX));
        for (k, v) in next {
            visit(d, kids, k, depth + 1, v, seen, rows);
        }
    }
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for r in roots {
        visit(d, kids, r, 0, None, &mut seen, &mut rows);
    }
    Graph {
        roots: roots.iter().map(|r| r.to_string()).collect(),
        rows,
        ..Default::default()
    }
}

/// The entry points that reach a graph's roots, each with the shortest
/// chain from it: a public item, or an item nothing calls (`main`, a
/// callback). Tests are left out; they're listed apart.
pub fn entries(d: &Data, g: &Graph) -> Vec<Vec<String>> {
    let roots: HashSet<&str> = g.roots.iter().map(String::as_str).collect();
    let mut prev: HashMap<&str, &str> = HashMap::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for r in &g.roots {
        // A public root is an entry point itself.
        if d.item(r).is_some_and(|it| !d.in_public_api(it)) {
            prev.insert(r, r);
            queue.push_back(r);
        }
    }
    let mut found: Vec<&str> = Vec::new();
    while let Some(cur) = queue.pop_front() {
        let up: Vec<&str> = callers_of(d, cur)
            .into_iter()
            .filter(|c| {
                d.item(c).is_some_and(|it| {
                    !is_test(it) && !matches!(it.info.kind, Kind::Mod | Kind::Impl)
                })
            })
            .collect();
        if up.is_empty() && !roots.contains(cur) {
            found.push(cur);
        }
        for n in up {
            if prev.contains_key(n) {
                continue;
            }
            prev.insert(n, cur);
            if d.item(n).is_some_and(|it| d.in_public_api(it)) {
                found.push(n);
            } else {
                queue.push_back(n);
            }
        }
    }
    found
        .into_iter()
        .map(|e| {
            let mut path = vec![e.to_string()];
            let mut cur = e;
            while prev[cur] != cur {
                cur = prev[cur];
                path.push(cur.to_string());
            }
            path
        })
        .collect()
}

/// A link to an item, with a ✓ in review mode once it is reviewed.
fn item_link(d: &Data, id: &str) -> String {
    let kind = d.item(id).map_or("fn", |it| it.info.kind.css());
    format!(
        r#"<a class="ref" data-id="{}" data-kind="{kind}"><code>{}</code></a>"#,
        esc(id),
        crate::review::item_tag(id, &d.display_name_of(id)),
    )
}

/// A link to an item that is context, not a change.
fn plain_link(d: &Data, id: &str, title: &str) -> String {
    let kind = d.item(id).map_or("fn", |it| it.info.kind.css());
    format!(
        r#"<a class="ref" data-id="{}" data-kind="{kind}" title="{}"><code>{}</code></a>"#,
        esc(id),
        esc(title),
        esc(&d.display_name_of(id))
    )
}

/// A badge for a new or removed item. Changed items, the usual case,
/// have none, so that the new ones stand out.
fn badge(it: &Item) -> &'static str {
    match it.status {
        Status::Added => r#"<span class="st st-added badge">new</span>"#,
        Status::Removed => r#"<span class="st st-removed badge">removed</span>"#,
        _ => "",
    }
}

/// "Reached from `start` via `load`, `on_click`".
fn entries_html(d: &Data, g: &Graph) -> String {
    let public_roots: Vec<&String> = g
        .roots
        .iter()
        .filter(|r| d.item(r).is_some_and(|it| d.in_public_api(it)))
        .collect();
    let mut paths = entries(d, g);
    paths.sort_by_key(Vec::len);
    let mut parts: Vec<String> = paths
        .iter()
        .take(SHOWN_ENTRIES)
        .map(|path| {
            let title = path
                .iter()
                .map(|id| d.display_name_of(id))
                .collect::<Vec<_>>()
                .join(" → ");
            let mut s = plain_link(d, &path[0], &title);
            // The items in between, without the root itself.
            if let Some(first) = path[1..path.len() - 1].first() {
                s.push_str(&format!(
                    r#" <span class="dim">via</span> {}"#,
                    plain_link(d, first, &title)
                ));
            }
            s
        })
        .collect();
    if paths.len() > SHOWN_ENTRIES {
        parts.push(format!("{} more", paths.len() - SHOWN_ENTRIES));
    }
    let public = match public_roots.len() {
        0 => String::new(),
        1 if g.roots.len() == 1 => "Public API".to_string(),
        _ => format!(
            "Public API: {}",
            public_roots
                .iter()
                .map(|r| format!("<code>{}</code>", esc(&d.display_name_of(r))))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let reached = if parts.is_empty() {
        String::new()
    } else {
        format!("Reached from {}", parts.join(", "))
    };
    let text = match (public.is_empty(), reached.is_empty()) {
        (false, false) => format!("{public} · {reached}"),
        (false, true) => public,
        (true, false) => reached,
        (true, true) => {
            "No resolved caller: called through a trait, from a macro, or not at all".to_string()
        }
    };
    format!(r#"<div class="cg-line dim">{text}</div>"#)
}

/// One graph as an overview entry.
fn graph_html(d: &Data, i: usize, g: &Graph) -> String {
    let new = g
        .rows
        .iter()
        .filter(|r| d.item(&r.id).is_some_and(|it| it.status == Status::Added))
        .count();
    let count = match (g.rows.len(), new) {
        (1, 1) => "new function".to_string(),
        (1, _) => "1 function".to_string(),
        (n, 0) => format!("{n} functions"),
        (n, k) if k == n => format!("{n} new functions"),
        (n, k) => format!("{n} functions, {k} new"),
    };
    let head: String = g
        .roots
        .iter()
        .take(3)
        .map(|r| format!("<code>{}</code>", esc(&d.display_name_of(r))))
        .collect::<Vec<_>>()
        .join(r#"<span class="dim"> · </span>"#);
    let more_roots = match g.roots.len() {
        n if n > 3 => format!(r#" <span class="dim">+{} more</span>"#, n - 3),
        _ => String::new(),
    };
    let row = |r: &Row| {
        let Some(it) = d.item(&r.id) else {
            return String::new();
        };
        let via = r.via.as_deref().map_or(String::new(), |v| {
            format!(
                r#" <span class="dim">via</span> {}"#,
                plain_link(d, v, "Unchanged item in between")
            )
        });
        format!(
            r#"<li style="--depth:{}">{}{via} {}</li>"#,
            r.depth.min(MAX_INDENT),
            item_link(d, &r.id),
            badge(it),
        )
    };
    let shown: String = g.rows.iter().take(SHOWN_ROWS).map(row).collect();
    let folded = if g.rows.len() > SHOWN_ROWS {
        let rest: String = g.rows.iter().skip(SHOWN_ROWS).map(row).collect();
        format!(
            r#"<details class="cg-fold"><summary>{} more</summary><ul class="cg-tree">{rest}</ul></details>"#,
            g.rows.len() - SHOWN_ROWS
        )
    } else {
        String::new()
    };
    let list = |label: &str, ids: &[String]| {
        if ids.is_empty() {
            return String::new();
        }
        let links: Vec<String> = ids
            .iter()
            .map(|id| {
                let b = d
                    .item(id)
                    .filter(|it| changed(it))
                    .map_or("", |it| badge(it));
                format!("{} {b}", item_link(d, id)).trim_end().to_string()
            })
            .collect();
        format!(
            r#"<div class="cg-line"><span class="dim">{label}:</span> {}</div>"#,
            links.join(", ")
        )
    };
    let tests: Vec<String> = g.tests.clone();
    let ids = g
        .rows
        .iter()
        .map(|r| r.id.as_str())
        .chain(g.types.iter().map(String::as_str))
        .chain(
            g.tests
                .iter()
                .filter(|t| d.item(t).is_some_and(changed))
                .map(String::as_str),
        );
    format!(
        r#"<div class="irow cgraph"><div class="ihead"><span class="kind">fn</span><span class="cg-roots">{head}{more_roots}</span><span class="dim">{count}</span><span class="rv-group">{}</span></div>
        <div class="iacts"><button data-cgraph="{i}" title="{}">{}</button></div>
        {}<ul class="cg-tree">{shown}</ul>{folded}{}{}</div>"#,
        crate::review::group_html(ids),
        if crate::narrow() {
            "Show the root in the call view"
        } else {
            "Open the graph's functions on the canvas"
        },
        if crate::narrow() {
            "Calls ▸"
        } else {
            "Canvas ▸"
        },
        entries_html(d, g),
        list("Types and constants", &g.types),
        list_tests(d, &tests),
    )
}

/// "Tested by: …": the changed tests by name, the unchanged ones as a
/// count with their names on hover, since a widely used function can be
/// reached by dozens of them.
fn list_tests(d: &Data, tests: &[String]) -> String {
    if tests.is_empty() {
        return String::new();
    }
    let (changed_tests, unchanged): (Vec<&String>, Vec<&String>) =
        tests.iter().partition(|id| d.item(id).is_some_and(changed));
    let mut links: Vec<String> = changed_tests
        .iter()
        .map(|id| {
            let b = d.item(id).map_or("", badge);
            format!("{} {b}", item_link(d, id)).trim_end().to_string()
        })
        .collect();
    match unchanged.len() {
        0 => {}
        1 => links.push(plain_link(d, unchanged[0], "Unchanged test function")),
        n => links.push(format!(
            r#"<span class="cg-more" title="{}">{n} unchanged tests</span>"#,
            esc(&unchanged
                .iter()
                .map(|id| d.display_name_of(id))
                .collect::<Vec<_>>()
                .join(", "))
        )),
    }
    format!(
        r#"<div class="cg-line"><span class="dim">Tested by:</span> {}</div>"#,
        links.join(", ")
    )
}

/// The overview's section. Empty without changed Rust items.
pub fn overview_html(d: &Data) -> String {
    let gs = graphs(d);
    let fns: usize = gs.graphs.iter().map(|g| g.rows.len()).sum();
    if gs.graphs.is_empty() && gs.tests.is_empty() && gs.other.is_empty() && gs.removed.is_empty() {
        return String::new();
    }
    let new = gs
        .graphs
        .iter()
        .flat_map(|g| &g.rows)
        .filter(|r| d.item(&r.id).is_some_and(|it| it.status == Status::Added))
        .count();
    let note = match gs.graphs.len() {
        0 => "no function changed".to_string(),
        n => format!(
            "{n} graph{} · {fns} function{} changed{}",
            if n == 1 { "" } else { "s" },
            if fns == 1 { "" } else { "s" },
            if new > 0 {
                format!(", {new} new")
            } else {
                String::new()
            }
        ),
    };
    let graphs: String = gs
        .graphs
        .iter()
        .enumerate()
        .map(|(i, g)| graph_html(d, i, g))
        .collect();
    let extra = |label: &str, ids: &[String], what: &str| {
        if ids.is_empty() {
            return String::new();
        }
        let links: Vec<String> = ids
            .iter()
            .map(|id| {
                let b = d.item(id).map_or("", badge);
                format!("{} {b}", item_link(d, id)).trim_end().to_string()
            })
            .collect();
        format!(
            r#"<div class="irow cgraph cg-extra"><div class="ihead"><span class="title-name">{label}</span><span class="dim">{what}</span><span class="rv-group">{}</span></div><div class="cg-line">{}</div></div>"#,
            crate::review::group_html(ids.iter().map(String::as_str)),
            links.join(", ")
        )
    };
    format!(
        r#"<h2>Changed call graphs <span class="h-note">{note}</span> {}</h2>{graphs}{}{}{}"#,
        crate::pages::help_html(
            "Each graph starts at a changed function that no other changed function calls, and lists the changed functions it reaches, in call order (indented by call depth). Graphs that share a function are merged. Only resolved calls are followed. Functions without a badge changed; new ones are marked."
        ),
        extra("Other changed tests", &gs.tests, "reach no graph"),
        extra(
            "Other changed types and constants",
            &gs.other,
            "used by no graph"
        ),
        extra("Removed", &gs.removed, ""),
    )
}

/// The functions of the `i`th graph, for the canvas.
pub fn graph_items(d: &Data, i: usize) -> Vec<String> {
    graphs(d)
        .graphs
        .into_iter()
        .nth(i)
        .map_or(Vec::new(), |g| g.rows.into_iter().map(|r| r.id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Call, ItemInfo, Meta, Output};

    fn item(id: &str, kind: Kind, public: bool, status: Status, calls: &[&str]) -> (String, Item) {
        let info = ItemInfo {
            id: id.into(),
            kind,
            name: id.rsplit("::").next().unwrap().into(),
            module: "c".into(),
            public,
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

    fn f(id: &str, status: Status, calls: &[&str]) -> (String, Item) {
        item(id, Kind::Fn, false, status, calls)
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

    fn roots(g: &Graphs) -> Vec<Vec<&str>> {
        g.graphs
            .iter()
            .map(|g| g.roots.iter().map(String::as_str).collect())
            .collect()
    }

    #[test]
    fn graphs_start_at_the_topmost_changed_function() {
        use Status::*;
        let mut test = f("c::tests::t", Added, &["c::save"]);
        test.1.info.test = true;
        let d = data(vec![
            item(
                "c::start",
                Kind::Fn,
                true,
                Unchanged,
                &["c::on_change", "c::lone"],
            ),
            f("c::on_change", Changed, &["c::from_name", "c::save"]),
            f("c::from_name", Added, &["c::name"]),
            f("c::name", Added, &[]),
            // Unchanged in between: still one graph.
            f("c::save", Unchanged, &["c::encode"]),
            f("c::encode", Changed, &["c::name"]),
            f("c::lone", Changed, &[]),
            test,
        ]);
        let gs = graphs(&d);
        assert_eq!(roots(&gs), vec![vec!["c::on_change"], vec!["c::lone"]]);
        let g = &gs.graphs[0];
        let rows: Vec<(&str, usize, Option<&str>)> = g
            .rows
            .iter()
            .map(|r| (r.id.as_str(), r.depth, r.via.as_deref()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("c::on_change", 0, None),
                ("c::from_name", 1, None),
                ("c::name", 2, None),
                ("c::encode", 1, Some("c::save")),
            ]
        );
        assert_eq!(g.tests, vec!["c::tests::t"]);
        assert_eq!(entries(&d, g), vec![vec!["c::start", "c::on_change"]]);
        assert!(gs.tests.is_empty());
    }

    #[test]
    fn graphs_sharing_a_function_merge_but_types_dont_merge_them() {
        use Status::*;
        let d = data(vec![
            f("c::a", Changed, &["c::shared"]),
            f("c::b", Changed, &["c::shared"]),
            f("c::shared", Changed, &[]),
            f("c::x", Changed, &[]),
            f("c::y", Changed, &[]),
            item("c::Ty", Kind::Struct, false, Changed, &[]),
            item("c::Unused", Kind::Struct, false, Added, &[]),
        ]);
        let mut d = d;
        for id in ["c::x", "c::y"] {
            let it = d.out.items.get_mut(id).unwrap();
            it.info.uses.insert(model::Use {
                to: "c::Ty".into(),
                ctx: model::UseCtx::Body,
            });
        }
        let d = data(d.out.items.clone().into_iter().collect());
        let gs = graphs(&d);
        assert_eq!(
            roots(&gs),
            vec![vec!["c::a", "c::b"], vec!["c::x"], vec!["c::y"]]
        );
        assert_eq!(gs.graphs[1].types, vec!["c::Ty"]);
        assert_eq!(gs.graphs[2].types, vec!["c::Ty"]);
        assert_eq!(gs.other, vec!["c::Unused"]);
    }

    #[test]
    fn a_cycle_has_one_root() {
        use Status::*;
        let d = data(vec![
            f("c::a", Changed, &["c::b"]),
            f("c::b", Changed, &["c::a"]),
        ]);
        let gs = graphs(&d);
        assert_eq!(roots(&gs), vec![vec!["c::a"]]);
        assert_eq!(gs.graphs[0].rows.len(), 2);
    }
}
