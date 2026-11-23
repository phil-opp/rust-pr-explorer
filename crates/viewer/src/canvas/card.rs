//! The HTML of one card: its tabs, the active item and its links.

use super::{Canvas, Group};
use crate::code::{Links, code_html, docs_html, esc};
use crate::data::{Data, changed, kind_order};
use model::{Item, Kind, Status, UseCtx};
use std::collections::{BTreeMap, BTreeSet};

/// A badge for related items that are not visible from the canvas's scope:
/// a click views the canvas from the module that hides them.
fn hidden_badge(d: &Data, scope: &str, list: &[String], what: &str) -> String {
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
    let n = hidden.len();
    let title = format!(
        "{n} {what}{} private to {}, so not visible from {}.{}",
        if n > 1 { "s are" } else { " is" },
        mods.iter()
            .map(|m| d.mod_label(m))
            .collect::<Vec<_>>()
            .join(", "),
        d.mod_label(scope),
        mods.first()
            .map(|m| format!(" Click to view from {}.", d.mod_label(m)))
            .unwrap_or_default()
    );
    let drill = mods
        .first()
        .map(|m| format!(r#" data-drill="{}""#, esc(m)))
        .unwrap_or_default();
    format!(
        r#"<a class="card-badge private"{drill} title="{}">{n} private {what}{}</a>"#,
        esc(&title),
        if n > 1 { "s" } else { "" }
    )
}

pub(super) fn card_html(d: &Data, c: &Canvas, g: &Group) -> String {
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
        // Callers first: they open to the left of the card, callees to the
        // right.
        if !callers.is_empty() {
            acts.push_str(&format!(r#"<button data-act="callers" title="Show what calls this">◂ callers <span class="n">{}</span></button>"#, callers.len()));
        }
        if !callees.is_empty() {
            acts.push_str(&format!(r#"<button data-act="callees" title="Show what this calls">calls ▸ <span class="n">{}</span></button>"#, callees.len()));
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

    // Unchanged items (context, such as an unchanged callee) show only
    // their signature until expanded.
    let context = !changed(it) && !d.touched(it);
    let folded = context
        && it.info.kind != Kind::Mod
        && d.external(id).is_none()
        && !g.expanded.contains(id);
    if context && it.info.kind != Kind::Mod && d.external(id).is_none() {
        acts.push_str(if folded {
            r#"<button data-act="expand" title="Show the documentation, source and members">docs &amp; source ▾</button>"#
        } else {
            r#"<button data-act="fold" title="Show only the signature">signature only ▴</button>"#
        });
    }

    // Signature, docs and source, with changes marked.
    let body = if folded {
        crate::code::marked_html(d.sig_segs(it, scope))
    } else if d.external(id).is_some() {
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
            d.source_seg(
                it,
                g.open_src.get(id).copied(),
                true,
                g.unfolded.get(id).unwrap_or(&BTreeSet::new()),
            ),
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
    let members = if ms.is_empty() || folded {
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
    let context = match (context, folded) {
        (false, _) => "",
        (true, false) => "context",
        (true, true) => "context folded",
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
            // Footer: what can be opened next, and badges for what is not
            // shown (details in their tooltips).
            let badges = (if it.info.unresolved_calls > 0 {
                let n = it.info.unresolved_calls;
                let why = if d.out.meta.resolution.starts_with("rust-analyzer") {
                    "rust-analyzer could not determine their targets"
                } else {
                    "method calls and calls inside macros need rust-analyzer"
                };
                format!(
                    r#"<span class="card-badge" title="{n} call site{} not resolved ({why}), so not shown as {}.">{n} unresolved</span>"#,
                    if n > 1 { "s" } else { "" },
                    if n > 1 { "calls" } else { "a call" }
                )
            } else {
                String::new()
            } + &hidden_badge(d, scope, &callees, "callee")
                + &hidden_badge(d, scope, &callers, "caller"));
            if badges.is_empty() && acts.is_empty() {
                String::new()
            } else {
                let badges = if badges.is_empty() {
                    badges
                } else {
                    format!(r#"<span class="grow"></span>{badges}"#)
                };
                format!(
                    r#"<div class="card-foot"><div class="card-acts">{acts}{badges}</div></div>"#
                )
            }
        },
        match crate::review::control_html(id) {
            c if c.is_empty() => c,
            c => format!(r#"<div class="card-rv">{c}</div>"#),
        },
    )
}
