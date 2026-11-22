//! The sidebar, the overview and the module pages.

use crate::App;
use crate::code::{code_html, docs_html, esc, first_paragraph};
use crate::data::{Data, changed, kind_order};
use crate::dom;
use model::{Item, Kind, Status};
use std::collections::BTreeMap;

pub fn render_sidebar(app: &App) {
    let d = &app.d;
    fn tree(app: &App, m: &str) -> String {
        let d = &app.d;
        let Some(it) = d.item(m) else {
            return String::new();
        };
        let mut subs: Vec<&Item> = d
            .mod_children_items(m)
            .into_iter()
            .filter(|c| c.info.kind == Kind::Mod)
            .collect();
        subs.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        let n = d.changes_in(m, false).len();
        let private = !d.is_root(it) && it.info.vis_scope.is_some();
        let sel = app.s.view == crate::View::Module && app.s.module == m;
        let dot = if it.status == Status::Unchanged {
            if d.touched(it) {
                "changed"
            } else {
                "unchanged"
            }
        } else {
            it.status.css()
        };
        format!(
            r#"<li><div class="tree-row {} {}" data-mod="{}" title="{} {}"><span class="dot {dot}"></span><span class="nm">{}</span>{}<span class="cnt">{}</span></div>{}</li>"#,
            if private { "priv" } else { "" },
            if sel { "sel" } else { "" },
            esc(m),
            esc(&it.info.vis_label),
            esc(m),
            esc(&it.info.name),
            if private {
                r#"<span class="vis">priv</span>"#
            } else {
                ""
            },
            if n > 0 { n.to_string() } else { String::new() },
            if subs.is_empty() {
                String::new()
            } else {
                format!(
                    r#"<ul class="tree">{}</ul>"#,
                    subs.iter()
                        .map(|s| tree(app, &s.info.id))
                        .collect::<String>()
                )
            }
        )
    }
    let roots: String = d.roots().iter().map(|r| tree(app, r)).collect();
    let warnings: String = d
        .out
        .warnings
        .iter()
        .map(|w| format!(r#"<div class="side-warn">⚠ {}</div>"#, esc(w)))
        .collect();
    dom::set_html(
        "sidebar",
        &format!(
            r#"<div class="side-h">Modules</div><ul class="tree">{roots}</ul>
            <div class="side-legend">
              <div><span class="dot added"></span> added &nbsp; <span class="dot changed"></span> changed &nbsp; <span class="dot removed"></span> removed</div>
              <div>Numbers: changed items defined directly in the module.</div>
              <div>Calls resolved with: {}</div>
            </div>{warnings}"#,
            esc(&d.out.meta.resolution)
        ),
    );
}

/// One item in a listing: kind, name, status, signature and first docs paragraph.
fn item_row(
    d: &Data,
    it: &Item,
    scope: &str,
    context: bool,
    member: bool,
    with_module: bool,
) -> String {
    let sig = if it.info.kind == Kind::Mod {
        String::new()
    } else {
        format!(
            r#"<pre class="sig">{}</pre>"#,
            code_html(d, d.sig_for(it, scope), &it.info.sig_refs)
        )
    };
    let first = first_paragraph(&it.info.docs);
    let docs = if first.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div class="docs">{}</div>"#,
            docs_html(d, first, &it.info.doc_refs)
        )
    };
    format!(
        r#"<div class="irow {} {} {}" data-id="{}">
        <div class="ihead"><span class="kind">{}</span>{} {} {}</div>
        <div class="iacts"><button data-open="{}" title="Open on the canvas">Open ▸</button></div>
        {sig}{docs}</div>"#,
        it.status.css(),
        if context { "context" } else { "" },
        if member { "member" } else { "" },
        esc(&it.info.id),
        it.info.kind.label(),
        d.title_html(it, with_module),
        d.vis_html(it),
        d.status_html(it),
        esc(&it.info.id),
    )
}

/// Rows for an item plus its members that pass `filter` (e.g. a type with
/// changed methods). Empty if neither the item nor a member passes.
fn item_with_members(
    d: &Data,
    it: &Item,
    scope: &str,
    filter: &dyn Fn(&Item) -> bool,
    with_module: bool,
) -> String {
    let mut shown: Vec<&Item> = Vec::new();
    if it.info.kind != Kind::Mod {
        for m in d.members(it) {
            if m.info.kind == Kind::Impl {
                // Trait impls are listed under their type only.
                if m.info.parent.as_deref() != Some(&it.info.id) {
                    continue;
                }
                if filter(m) || d.members(m).into_iter().any(filter) {
                    shown.push(m);
                }
            } else if filter(m) {
                shown.push(m);
            }
        }
    }
    if !filter(it) && shown.is_empty() {
        return String::new();
    }
    let mut html = item_row(d, it, scope, !changed(it), false, with_module);
    for m in shown {
        html.push_str(&item_row(d, m, scope, !changed(m), true, with_module));
    }
    html
}

/// Overview: how does the crate's public surface change?
pub fn render_overview(app: &App) {
    let d = &app.d;
    let all: Vec<&Item> = d.items().filter(|it| it.info.kind != Kind::Mod).collect();
    let count = |st: Status| all.iter().filter(|it| it.status == st).count();
    let pub_changed = all
        .iter()
        .filter(|it| it.info.public && changed(it))
        .count();
    let cosmetic = all.iter().filter(|it| it.changes.cosmetic).count();
    let is_pub = |it: &Item| it.info.public && changed(it);
    // Seen from outside the crate.
    const OUTSIDE: &str = "<extern>";

    fn mod_block(d: &Data, m: &str, is_pub: &dyn Fn(&Item) -> bool) -> String {
        let Some(module) = d.item(m) else {
            return String::new();
        };
        let root = d.is_root(module);
        if !module.info.public && !root {
            return String::new();
        }
        let mut kids = d.mod_children_items(m);
        kids.sort_by(|a, b| kind_order(a, b));
        let entries: String = kids
            .iter()
            .filter(|c| c.info.kind != Kind::Mod && c.info.public)
            .map(|c| item_with_members(d, c, OUTSIDE, is_pub, false))
            .collect();
        let subs: String = kids
            .iter()
            .filter(|c| c.info.kind == Kind::Mod)
            .map(|c| mod_block(d, &c.info.id, is_pub))
            .collect();
        // Public items of private modules are reachable through `pub use`.
        let reexported: String = if root {
            let mut list: Vec<&Item> = d
                .items()
                .filter(|c| {
                    crate::data::within(d.mod_of(c), m)
                        && c.info.public
                        && c.info.parent.is_none()
                        && c.info.kind != Kind::Mod
                        && d.item(&c.info.module).is_some_and(|p| !p.info.public)
                })
                .collect();
            list.sort_by(|a, b| kind_order(a, b));
            list.iter()
                .map(|c| item_with_members(d, c, OUTSIDE, is_pub, true))
                .collect()
        } else {
            String::new()
        };
        if entries.is_empty() && subs.is_empty() && reexported.is_empty() {
            return String::new();
        }
        format!(
            r#"<div class="modbox"><div class="modbox-h"><span class="kind">{}</span>{} {}<span style="flex:1"></span><a data-mod="{}">module page ›</a></div>
            <div class="modbox-b">{entries}{}{subs}</div></div>"#,
            if root { "crate" } else { "mod" },
            d.title_html(module, false),
            d.status_html(module),
            esc(m),
            if reexported.is_empty() {
                String::new()
            } else {
                format!(
                    r#"<div class="section-label">re-exported from private modules</div>{reexported}"#
                )
            }
        )
    }

    let public_html: String = d.roots().iter().map(|r| mod_block(d, r, &is_pub)).collect();
    let mut internal: BTreeMap<String, Vec<&Item>> = BTreeMap::new();
    for it in all.iter().filter(|it| !it.info.public && changed(it)) {
        internal
            .entry(d.mod_of(it).to_string())
            .or_default()
            .push(it);
    }
    let internal_html: String = internal
        .iter()
        .map(|(m, list)| {
            format!(
                r#"<div class="irow"><div class="ihead"><a data-mod="{}" class="title-name"><span class="k-mod">{}</span></a>
                <span class="dim">{} private item{}: {}</span></div>
                <div class="iacts"><button data-graph="{}">Internal call graph ▸</button></div></div>"#,
                esc(m),
                esc(&d.mod_label(m)),
                list.len(),
                if list.len() > 1 { "s" } else { "" },
                list.iter().map(|it| esc(&d.display_name(it))).collect::<Vec<_>>().join(", "),
                esc(m)
            )
        })
        .collect();
    let meta = &d.out.meta;
    let html = format!(
        r#"<h1>{}</h1>
        <div class="sub">{} → {} {}</div>
        <div class="stats">
          <div class="stat"><b style="color:var(--added)">{}</b><span>items added</span></div>
          <div class="stat"><b style="color:var(--changed)">{}</b><span>items changed</span></div>
          <div class="stat"><b style="color:var(--removed)">{}</b><span>items removed</span></div>
          <div class="stat"><b>{pub_changed}</b><span>in the public API</span></div>
          <div class="stat"><b>{cosmetic}</b><span>formatting-only (hidden)</span></div>
        </div>
        <h2>Public API changes</h2>
        <p class="sub">Items reachable from outside the crate that were added, changed or removed, in rustdoc form. Types appear as context when only their members changed.</p>
        {}
        <h2>Internal changes</h2>
        <p class="sub">Private and crate-internal items are kept out of the overview. Open a module to see its private items and internal call graph.</p>
        {}"#,
        esc(&meta.title),
        esc(&meta.base),
        esc(&meta.head),
        meta.url
            .as_deref()
            .map(|u| format!(
                r#"· <a href="{}" target="_blank" rel="noopener">view on GitHub</a>"#,
                esc(u)
            ))
            .unwrap_or_default(),
        count(Status::Added),
        count(Status::Changed),
        count(Status::Removed),
        if public_html.is_empty() {
            r#"<div class="empty-state">This pull request does not change the public API.</div>"#
                .to_string()
        } else {
            public_html
        },
        if internal_html.is_empty() {
            r#"<div class="empty-state">No internal changes.</div>"#.to_string()
        } else {
            internal_html
        },
    );
    dom::set_html("view-overview", &html);
}

/// Module page: a module as a privacy boundary.
pub fn render_module_page(app: &App) {
    let d = &app.d;
    let m = app.s.module.as_str();
    let Some(module) = d.item(m) else { return };
    let show_unchanged = app.s.show_unchanged;
    let filter = |it: &Item| show_unchanged || changed(it);

    let section = |list: Vec<&Item>| -> String {
        let total = list.len();
        let mut shown: Vec<&Item> = list
            .into_iter()
            .filter(|it| show_unchanged || d.touched(it))
            .collect();
        shown.sort_by(|a, b| kind_order(a, b));
        let hidden = total - shown.len();
        let mut html: String = shown
            .iter()
            .map(|it| item_with_members(d, it, m, &filter, false))
            .collect();
        if hidden > 0 {
            html.push_str(&format!(
                r#"<div class="collapsed-note">{hidden} unchanged item{} not shown</div>"#,
                if hidden > 1 { "s" } else { "" }
            ));
        }
        html
    };

    let children = d.mod_children_items(m);
    let exposed: Vec<&Item> = children
        .iter()
        .copied()
        .filter(|c| c.info.kind != Kind::Mod && !d.private_to(c, m))
        .collect();
    let private: Vec<&Item> = children
        .iter()
        .copied()
        .filter(|c| c.info.kind != Kind::Mod && d.private_to(c, m))
        .collect();
    let mut subs: Vec<&Item> = children
        .iter()
        .copied()
        .filter(|c| c.info.kind == Kind::Mod)
        .collect();
    subs.sort_by(|a, b| kind_order(a, b));

    // A submodule as seen from here: only what it exposes to this module.
    fn sub_box(d: &Data, s: &Item, m: &str, section: &dyn Fn(Vec<&Item>) -> String) -> String {
        let kids = d.mod_children_items(&s.info.id);
        let visible: Vec<&Item> = kids
            .iter()
            .copied()
            .filter(|k| k.info.kind != Kind::Mod && d.visible_from(&k.info.id, m))
            .collect();
        let hidden_changed = d
            .changes_in(&s.info.id, true)
            .iter()
            .filter(|it| !d.visible_from(&it.info.id, m))
            .count();
        let nested: String = kids
            .iter()
            .filter(|k| k.info.kind == Kind::Mod && d.visible_from(&k.info.id, m))
            .map(|k| sub_box(d, k, m, section))
            .collect();
        let body = section(visible) + &nested;
        let note = if hidden_changed > 0 {
            format!(
                r#"<div class="hidden-note">{hidden_changed} changed item{} private to <b>{}</b> and not visible from here. <a data-mod="{}">Open the module</a> to see {}.</div>"#,
                if hidden_changed > 1 { "s are" } else { " is" },
                esc(&s.info.name),
                esc(&s.info.id),
                if hidden_changed > 1 { "them" } else { "it" }
            )
        } else {
            String::new()
        };
        let empty = if body.is_empty() && hidden_changed == 0 {
            r#"<div class="collapsed-note">Nothing visible from here changed.</div>"#
        } else {
            ""
        };
        format!(
            r#"<div class="modbox"><div class="modbox-h"><span class="kind">mod</span>{} {} {}<span style="flex:1"></span><a data-mod="{}">open ›</a></div>
            <div class="modbox-b">{body}{note}{empty}</div></div>"#,
            d.title_html(s, false),
            d.vis_html(s),
            d.status_html(s),
            esc(&s.info.id),
        )
    }

    let crumbs = d
        .mod_chain(m)
        .iter()
        .map(|c| {
            format!(
                r#"<a data-mod="{}">{}</a>"#,
                esc(c),
                esc(d.item(c).map_or("", |i| &i.info.name))
            )
        })
        .collect::<Vec<_>>()
        .join("::");
    let none = |what: &str| {
        format!(
            r#"<div class="empty-state">No {what} items{}.</div>"#,
            if show_unchanged { "" } else { " changed" }
        )
    };
    let exposed_html = section(exposed);
    let private_html = section(private);
    let subs_html = if subs.is_empty() {
        String::new()
    } else {
        format!(
            r#"<h2>Submodules</h2><p class="sub">Each submodule shows only what it makes visible to <code>{}</code>.</p>{}"#,
            esc(d.short_path(m)),
            subs.iter()
                .map(|s| sub_box(d, s, m, &section))
                .collect::<String>()
        )
    };
    let html = format!(
        r#"<div class="crumbs">{crumbs}</div>
        <h1><span class="kind">{}</span> {} {} {}</h1>
        <div class="sub">{} · <label><input type="checkbox" id="show-unchanged" {}> show unchanged items</label> ·
          <button id="mod-graph">Internal call graph on canvas ▸</button></div>
        {}{}
        <h2>Exposed by this module</h2><div>{}</div>
        <h2>Private to this module</h2><div class="private-zone">{}</div>
        {subs_html}"#,
        if d.is_root(module) { "crate" } else { "module" },
        d.title_html(module, false),
        d.vis_html(module),
        d.status_html(module),
        esc(&module.info.file),
        if show_unchanged { "checked" } else { "" },
        if module.info.docs.is_empty() {
            String::new()
        } else {
            format!(
                r#"<div class="docs">{}</div>"#,
                docs_html(d, &module.info.docs, &module.info.doc_refs)
            )
        },
        if module.changes.docs {
            format!(
                r#"<details class="docs-old"><summary>Module docs changed — show previous</summary><div class="docs">{}</div></details>"#,
                docs_html(
                    d,
                    module.old_docs.as_deref().unwrap_or(""),
                    &module.info.doc_refs
                )
            )
        } else {
            String::new()
        },
        if exposed_html.is_empty() {
            none("exposed")
        } else {
            exposed_html
        },
        if private_html.is_empty() {
            none("private")
        } else {
            private_html
        },
    );
    dom::set_html("view-module", &html);
}
