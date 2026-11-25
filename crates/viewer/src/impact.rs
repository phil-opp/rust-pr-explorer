//! Impact of internal changes: which public items reach a changed internal
//! item, through which chain of calls and uses.

use crate::code::esc;
use crate::data::Data;
use model::{Item, Kind, Status};
use std::collections::{HashMap, VecDeque};

/// Public items listed per changed item before "and N more".
const SHOWN: usize = 4;

/// How a changed internal item is reached from the public API.
pub struct Impact {
    /// One chain per public item that reaches the item, shortest first:
    /// the public item, the items in between, then the changed item.
    pub paths: Vec<Vec<String>>,
}

/// Whether the impact of an item is worth computing: a changed item
/// outside the public API that code can call or use. A removed item has
/// no callers left, and modules and impls are not called.
pub fn applies(d: &Data, it: &Item) -> bool {
    matches!(it.status, Status::Added | Status::Changed)
        && !d.in_public_api(it)
        && !matches!(it.info.kind, Kind::Mod | Kind::Impl)
        && d.external(&it.info.id).is_none()
}

/// The public items that call or use `id`, directly or through other
/// items, each with the shortest chain that leads there. The search stops
/// at the first public item of each chain: the public items beyond it are
/// reached through that one. Only resolved calls and uses are followed,
/// and items of the base version only (removed ones) are left out.
pub fn impact(d: &Data, id: &str) -> Impact {
    // Breadth first, so the first way to an item is a shortest one.
    let mut prev: HashMap<&str, &str> = HashMap::new();
    let mut queue = VecDeque::from([id]);
    prev.insert(id, id);
    let mut found: Vec<&str> = Vec::new();
    while let Some(cur) = queue.pop_front() {
        let callers = d.callers.get(cur).into_iter().flatten();
        let users = d.used_by.get(cur).into_iter().flatten().map(|(u, _)| u);
        let mut next: Vec<&str> = callers.chain(users).map(String::as_str).collect();
        next.sort();
        next.dedup();
        for n in next {
            if prev.contains_key(n) {
                continue;
            }
            let Some(it) = d.item(n) else { continue };
            if it.status == Status::Removed || matches!(it.info.kind, Kind::Mod | Kind::Impl) {
                continue;
            }
            prev.insert(n, cur);
            if it.info.public {
                found.push(n);
            } else {
                queue.push_back(n);
            }
        }
    }
    let paths = found
        .into_iter()
        .map(|p| {
            let mut path = vec![p.to_string()];
            let mut cur = p;
            while cur != id {
                cur = prev[cur];
                path.push(cur.to_string());
            }
            path
        })
        .collect();
    Impact { paths }
}

/// A chain as text, for a tooltip: `evaluate → apply → checked`.
fn chain_text(d: &Data, path: &[String]) -> String {
    path.iter()
        .map(|id| d.display_name_of(id))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// A link to an item that opens it on the canvas.
fn link(d: &Data, id: &str, title: &str) -> String {
    let kind = d.item(id).map_or("fn", |it| it.info.kind.css());
    format!(
        r#"<a class="ref" data-id="{}" data-kind="{kind}" title="{}"><code>{}</code></a>"#,
        esc(id),
        esc(title),
        esc(&d.display_name_of(id))
    )
}

/// The public items that reach `id`, as links: "`evaluate` via `apply`,
/// `calc`". Hovering one shows its whole chain.
fn reached_html(d: &Data, imp: &Impact) -> String {
    let mut parts: Vec<String> = imp
        .paths
        .iter()
        .take(SHOWN)
        .map(|path| {
            let title = chain_text(d, path);
            let mut s = link(d, &path[0], &title);
            // The items in between, without the changed item itself.
            let between = &path[1..path.len() - 1];
            if let Some(first) = between.first() {
                s.push_str(&format!(
                    r#" <span class="dim">via</span> {}"#,
                    link(d, first, &title)
                ));
                if between.len() > 1 {
                    s.push_str(&format!(
                        r#" <span class="dim">and {} more</span>"#,
                        between.len() - 1
                    ));
                }
            }
            s
        })
        .collect();
    if imp.paths.len() > SHOWN {
        parts.push(format!(
            r#"<span class="dim">{} more</span>"#,
            imp.paths.len() - SHOWN
        ));
    }
    parts.join(", ")
}

/// Why nothing public may reach an item although the call graph has no
/// path: calls through a trait aren't followed, and unresolved call sites
/// could hide one.
fn no_path_note(d: &Data, it: &Item) -> &'static str {
    let in_trait_impl = it
        .info
        .parent
        .as_deref()
        .and_then(|p| d.item(p))
        .is_some_and(|p| p.info.kind == Kind::Impl && p.info.trait_ref.is_some());
    if in_trait_impl {
        "No public item calls or uses it directly; calls through its trait are not followed."
    } else if d.out.items.values().any(|i| i.info.unresolved_calls > 0) {
        "No public item calls or uses it, through resolved calls and uses (some call sites are unresolved)."
    } else {
        "No public item calls or uses it, directly or through other items."
    }
}

/// The impact of a changed internal item, as one line for its row or card.
/// Empty for items it doesn't apply to.
pub fn line_html(d: &Data, it: &Item) -> String {
    if !applies(d, it) {
        return String::new();
    }
    let imp = impact(d, &it.info.id);
    if imp.paths.is_empty() {
        return format!(r#"<div class="impact none">{}</div>"#, no_path_note(d, it));
    }
    format!(
        r#"<div class="impact"><span class="impact-h">Affects public API:</span> {} <a class="btn" data-impact="{}"{} title="{}">{}</a></div>"#,
        reached_html(d, &imp),
        esc(&it.info.id),
        crate::nav::href(&crate::nav::Route::Canvas(Some(
            crate::nav::OnCanvas::Impact(it.info.id.clone())
        ))),
        impact_button().0,
        impact_button().1,
    )
}

/// The tooltip and label of the button that shows how the public API
/// reaches an item: every chain on the canvas, or on a phone the item in
/// the call view (see `crate::open_item`).
fn impact_button() -> (&'static str, &'static str) {
    if crate::narrow() {
        (
            "Show the item with its callers, which lead up to the public API",
            "Show callers ▸",
        )
    } else {
        ("Open every chain on the canvas", "Show on canvas ▸")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Call, ItemInfo, Meta, Output};
    use std::collections::BTreeMap;

    fn item(id: &str, public: bool, status: Status, calls: &[&str]) -> (String, Item) {
        let info = ItemInfo {
            id: id.into(),
            kind: Kind::Fn,
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

    fn data(items: Vec<(String, Item)>) -> Data {
        Data::new(Output {
            meta: Meta::default(),
            crates: vec!["c".into()],
            items: items.into_iter().collect(),
            externals: BTreeMap::new(),
            files: Vec::new(),
            warnings: Vec::new(),
            semver_checks: None,
        })
    }

    #[test]
    fn chains_stop_at_the_first_public_item() {
        use Status::*;
        let d = data(vec![
            item("c::run", true, Unchanged, &["c::evaluate"]),
            item("c::evaluate", true, Unchanged, &["c::apply"]),
            item("c::calc", true, Unchanged, &["c::step"]),
            item("c::step", false, Unchanged, &["c::apply"]),
            item("c::apply", false, Changed, &["c::checked"]),
            item("c::checked", false, Changed, &[]),
            // Only in the base version: not a way in any more.
            item("c::old", true, Removed, &["c::checked"]),
        ]);
        let paths = impact(&d, "c::checked").paths;
        assert_eq!(
            paths,
            vec![
                vec!["c::evaluate", "c::apply", "c::checked"],
                vec!["c::calc", "c::step", "c::apply", "c::checked"],
            ]
        );
        // `run` is reached through `evaluate`, so it's not listed.
        assert!(!paths.iter().any(|p| p[0] == "c::run"));
    }

    #[test]
    fn unreached_items_say_so() {
        use Status::*;
        let d = data(vec![
            item("c::main", false, Unchanged, &["c::helper"]),
            item("c::helper", false, Changed, &[]),
        ]);
        assert!(impact(&d, "c::helper").paths.is_empty());
        let html = line_html(&d, d.item("c::helper").unwrap());
        assert!(html.contains("No public item"), "{html}");
    }

    #[test]
    fn cycles_end() {
        use Status::*;
        let d = data(vec![
            item("c::a", false, Changed, &["c::b"]),
            item("c::b", false, Unchanged, &["c::a"]),
            item("c::api", true, Unchanged, &["c::b"]),
        ]);
        assert_eq!(
            impact(&d, "c::a").paths,
            vec![vec!["c::api", "c::b", "c::a"]]
        );
    }
}
