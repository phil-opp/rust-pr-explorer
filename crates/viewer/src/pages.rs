//! The sidebar, the overview and the module pages.

use crate::App;
use crate::code::esc;
use crate::data::{Data, changed, kind_order};
use crate::dom;
use model::{ChangedFile, Item, Kind, Status};

/// Whether the sidebar lists the modules. Without any Rust change, a tree of
/// unchanged modules only gets in the way, so it shows only on the module
/// pages, where it is the way to browse the modules.
fn modules_shown(app: &App) -> bool {
    app.d.rust_changed() || (app.s.view == crate::View::Module && !app.d.roots().is_empty())
}

/// A small "?" that explains a section on hover, so the explanation isn't
/// read again on every page.
pub fn help_html(text: &str) -> String {
    format!(
        r#"<span class="help" title="{0}" aria-label="{0}">?</span>"#,
        esc(text)
    )
}

/// Whether the sidebar shows: it lists the changed modules and files.
pub fn sidebar_shown(app: &App) -> bool {
    modules_shown(app) || !app.d.out.files.is_empty()
}

/// Every changed file, with its status and line counts. A Rust file whose
/// changes are all inside items opens its module's page; any other file
/// opens its entry in the overview.
fn files_html(d: &Data) -> String {
    let mut files: Vec<&ChangedFile> = d
        .out
        .files
        .iter()
        .filter(|f| crate::commit_filter::file_in(&f.path))
        .collect();
    if files.is_empty() {
        return String::new();
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let (added, removed) = files
        .iter()
        .fold((0, 0), |(a, r), f| (a + f.added, r + f.removed));
    let rows: String = files
        .iter()
        .map(|f| {
            let module = (f.analyzed && f.outside_items == 0)
                .then(|| {
                    d.items()
                        .find(|it| it.info.kind == Kind::Mod && it.info.file == f.path)
                })
                .flatten();
            let target = match module {
                Some(m) => format!(
                    r#"data-mod="{}"{}"#,
                    esc(&m.info.id),
                    crate::nav::module_href(&m.info.id)
                ),
                None => format!(
                    r#"data-goto-file="{}"{}"#,
                    esc(&f.path),
                    crate::nav::file_href(&f.path)
                ),
            };
            let (dir, name) = match f.path.rsplit_once('/') {
                Some((dir, name)) => (dir.to_string(), name),
                None => (String::new(), f.path.as_str()),
            };
            let counts = if f.lang == "binary" {
                r#"<span class="dim">bin</span>"#.to_string()
            } else {
                format!(
                    r#"<span class="d-add">+{}</span> <span class="d-del">−{}</span>"#,
                    f.added, f.removed
                )
            };
            format!(
                r#"<li><a class="tree-row file-row {}" {target} title="{}{}"><span class="dot {}"></span><span class="fn">{}<span class="fdir">{}</span></span><span class="cnt">{counts}</span></a></li>"#,
                if f.generated.is_some() { "gen" } else { "" },
                esc(&f.path),
                f.generated
                    .as_deref()
                    .map(|g| format!(" ({})", esc(g)))
                    .unwrap_or_default(),
                f.status.css(),
                esc(name),
                esc(&dir),
            )
        })
        .collect();
    format!(
        r#"<div class="side-h">Files <span class="side-h-n">{} · <span class="d-add">+{added}</span> <span class="d-del">−{removed}</span></span></div><ul class="tree files">{rows}</ul>"#,
        files.len()
    )
}

fn warnings_html(d: &Data) -> String {
    d.out
        .warnings
        .iter()
        .map(|w| format!(r#"<div class="side-warn">⚠ {}</div>"#, esc(w)))
        .collect()
}

pub fn render_sidebar(app: &App) {
    let d = &app.d;
    let shown = sidebar_shown(app);
    if let Some(el) = dom::by_id("sidebar") {
        dom::toggle_class(&el, "hidden", !shown);
    }
    if !shown {
        return;
    }
    crate::outline::apply_closed(app);
    if crate::outline::shown(app) {
        dom::set_html(
            "sidebar",
            &format!("{}{}", crate::outline::html(app), warnings_html(d)),
        );
        return;
    }
    let files = files_html(d);
    if !modules_shown(app) {
        dom::set_html("sidebar", &format!("{files}{}", warnings_html(d)));
        return;
    }
    fn submodules<'a>(d: &'a Data, m: &str) -> Vec<&'a Item> {
        let mut subs: Vec<&Item> = d
            .mod_children_items(m)
            .into_iter()
            .filter(|c| c.info.kind == Kind::Mod)
            .collect();
        subs.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        subs
    }
    fn count(d: &Data, m: &str) -> usize {
        1 + submodules(d, m)
            .iter()
            .map(|s| count(d, &s.info.id))
            .sum::<usize>()
    }
    // Unchanged modules are left out unless asked for, except the open
    // module page's module and its parents.
    fn tree(app: &App, m: &str, hidden: &mut usize) -> String {
        let d = &app.d;
        let Some(it) = d.item(m) else {
            return String::new();
        };
        let open = app.s.view == crate::View::Module
            && (app.s.module == m || app.s.module.starts_with(&format!("{m}::")));
        if !app.s.side_all && !d.touched(it) && !open {
            *hidden += count(d, m);
            return String::new();
        }
        let subs = submodules(d, m);
        let changes = d.changes_in(m, false);
        let n = changes.len();
        // In review mode: how many still need a review. A module without
        // changes of its own gets the ✓ once its submodules are reviewed.
        let open_in =
            |list: &[&Item]| crate::review::open_count(list.iter().map(|c| c.info.id.as_str()));
        let count = if !crate::review::is_on() {
            if n == 0 { String::new() } else { n.to_string() }
        } else if n == 0 {
            let deep = d.changes_in(m, true);
            if !deep.is_empty() && open_in(&deep) == 0 {
                r#"<span class="rv-all" title="Everything in its submodules is reviewed">✓</span>"#
                    .to_string()
            } else {
                String::new()
            }
        } else {
            match open_in(&changes) {
                0 => r#"<span class="rv-all" title="All reviewed">✓</span>"#.to_string(),
                left => format!(r#"<span title="{left} of {n} not reviewed yet">{left}</span>"#),
            }
        };
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
            r#"<li><a class="tree-row {} {}" data-mod="{}"{} title="{} {}"><span class="dot {dot}"></span><span class="nm">{}</span>{}<span class="cnt">{}</span></a>{}</li>"#,
            if private { "priv" } else { "" },
            if sel { "sel" } else { "" },
            esc(m),
            crate::nav::module_href(m),
            esc(&it.info.vis_label),
            esc(m),
            esc(&if d.is_root(it) {
                crate::data::root_name(&it.info.id)
            } else {
                it.info.name.clone()
            }),
            if private {
                r#"<span class="vis">priv</span>"#
            } else {
                ""
            },
            count,
            if subs.is_empty() {
                String::new()
            } else {
                format!(
                    r#"<ul class="tree">{}</ul>"#,
                    subs.iter()
                        .map(|s| tree(app, &s.info.id, hidden))
                        .collect::<String>()
                )
            }
        )
    }
    let mut hidden = 0;
    let roots: String = d
        .roots()
        .iter()
        .map(|r| tree(app, r, &mut hidden))
        .collect();
    let warnings = warnings_html(d);
    let note = if d.rust_changed() {
        ""
    } else {
        r#"<div class="side-note">No module changed in this PR.</div>"#
    };
    // Offered only when it would show more.
    let toggle = if hidden > 0 || app.s.side_all {
        format!(
            r#"<label class="side-opt"><input type="checkbox" id="side-all" {}> show unchanged modules{}</label>"#,
            if app.s.side_all { "checked" } else { "" },
            if hidden > 0 {
                format!(" ({hidden})")
            } else {
                String::new()
            }
        )
    } else {
        String::new()
    };
    dom::set_html(
        "sidebar",
        &format!(
            r#"<div class="side-h">Modules</div>{note}<ul class="tree">{roots}</ul>{toggle}{files}
            <div class="side-legend">
              <div class="legend-dots"><span><span class="dot added"></span> added</span><span><span class="dot changed"></span> changed</span><span><span class="dot removed"></span> removed</span></div>
              <div>No symbol: unchanged.</div>
              <div class="legend-bars"><span class="lb m-add">added</span><span class="lb m-chg">changed</span><span class="lb m-del">removed</span></div>
              <div>{}</div>
              <div>Calls resolved with: {}</div>
            </div>{warnings}"#,
            if crate::review::is_on() {
                "Numbers: changed items defined directly in the module that are not reviewed yet; ✓ when all are, and for a module without changes of its own, when everything in its submodules is."
            } else {
                "Numbers: changed items defined directly in the module."
            },
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
    // Whether this row says "new"/"removed"; not when a group around it
    // already does.
    badge: bool,
) -> String {
    let title = d.title_html(it, false);
    // What changed in the body, expandable to the diff; the whole source
    // of new and removed items, and of items with review threads.
    let src = if it.changes.body
        || it.changes.cosmetic
        || matches!(it.status, Status::Added | Status::Removed)
        || !d.item_threads(&it.info.id).is_empty()
    {
        d.source_seg(it, None, false, &Default::default())
    } else {
        None
    };
    let body = format!(
        "{}{}{}{}",
        crate::unsafety::lines_html(&it.unsafe_code),
        d.api_reasons_html(it),
        crate::impact::line_html(d, it),
        d.item_body_html(it, scope, false, src)
    );
    format!(
        r#"<div class="irow {} {} {} {}" data-id="{}">
        <div class="ihead{}"><span class="kind">{}</span>{} {} {} {}{}</div>
        <div class="iacts">{}{}<a class="btn" data-open="{}"{} title="Open on the canvas">Open ▸</a></div>
        {body}</div>"#,
        it.status.css(),
        if context { "context" } else { "" },
        if member { "member" } else { "" },
        if badge { "" } else { "grouped" },
        esc(&it.info.id),
        crate::data::faded_title(it),
        it.info.kind.label(),
        title,
        d.vis_html(it),
        if badge || !matches!(it.status, Status::Added | Status::Removed) {
            d.status_html(it)
        } else {
            String::new()
        },
        crate::unsafety::tag(it),
        d.api_badge(it),
        crate::review::control_html(&it.info.id),
        crate::post::item_links(d, it),
        esc(&it.info.id),
        crate::nav::item_href(&it.info.id),
    )
}

/// Rows for an item plus its members that pass `filter` (e.g. a type with
/// changed methods). Empty if neither the item nor a member passes.
fn item_with_members(
    d: &Data,
    it: &Item,
    scope: &str,
    filter: &dyn Fn(&Item) -> bool,
    // The status of the group the item is listed in, if that group (a new
    // or removed module) is already marked as a whole.
    group: Option<Status>,
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
    let marked_above = group == Some(it.status);
    let html = item_row(d, it, scope, !changed(it), false, !marked_above);
    // An added or removed item, with its members, as one marked group:
    // the item says "new" once, its members don't repeat it.
    let whole = matches!(it.status, Status::Added | Status::Removed);
    let (mut inner, mut rest) = (String::new(), String::new());
    for m in shown {
        let same = whole && m.status == it.status;
        let badge = !same && group != Some(m.status);
        let row = if m.info.kind == Kind::Impl {
            // A trait impl is folded into its type: one line, then its
            // members that changed.
            let new_impl = matches!(m.status, Status::Added | Status::Removed);
            let methods: String = d
                .members(m)
                .into_iter()
                .filter(|x| filter(x))
                .map(|x| {
                    let said = (new_impl || same) && x.status == m.status;
                    let badge = !said && group != Some(x.status);
                    item_row(d, x, scope, !changed(x), true, badge)
                })
                .collect();
            impl_line(d, m, badge) + &methods
        } else {
            item_row(d, m, scope, !changed(m), true, badge)
        };
        if same {
            inner.push_str(&row);
        } else {
            rest.push_str(&row);
        }
    }
    // One card per item, its members included; a new or removed item's
    // card is marked as a whole.
    let class = if whole && !marked_above {
        format!("whole whole-{}", it.status.css())
    } else {
        String::new()
    };
    format!(r#"<div class="ientry {class}">{html}{inner}{rest}</div>"#)
}

/// A trait impl of a listed type, as one line.
fn impl_line(d: &Data, m: &Item, badge: bool) -> String {
    use crate::code::{Links, code_html};
    let header = code_html(
        d,
        &m.info.sig,
        Links::Sig {
            refs: &m.info.sig_refs,
            at: &m.info.sig_links,
            base: 0,
        },
    );
    format!(
        r#"<div class="impl-line {}" data-id="{}"><code>{header}</code> {}{}<span class="grow"></span>{}<a class="btn" data-open="{}"{} title="Open on the canvas">Open ▸</a></div>"#,
        m.status.css(),
        esc(&m.info.id),
        if badge || !matches!(m.status, Status::Added | Status::Removed) {
            d.status_html(m)
        } else {
            String::new()
        },
        crate::unsafety::tag(m),
        crate::review::control_html(&m.info.id),
        esc(&m.info.id),
        crate::nav::item_href(&m.info.id),
    )
}

/// The status a module marks as a whole (new or removed), if any.
fn group_status(module: &Item) -> Option<Status> {
    matches!(module.status, Status::Added | Status::Removed).then_some(module.status)
}

/// Overview: how does the crate's public surface change?
pub fn render_overview(app: &App) {
    let d = &app.d;
    // Sections without changes are left out. Without any Rust change, only
    // the analysis warnings remain, and only if no sidebar shows them.
    let rust_html = if !d.rust_changed() {
        if sidebar_shown(app) {
            String::new()
        } else {
            warnings_html(d)
        }
    } else {
        let unsafe_html = crate::unsafety::overview_html(d);
        let semver = semver_summary(d);
        let public = if semver.is_empty() {
            String::new()
        } else {
            format!(
                r#"<h2>Public API changes {}</h2>{semver}"#,
                help_html(
                    "Changes to what other crates can use, by their public path, with their SemVer label and its first reason. A click shows the item on its module page, with its signature, docs and all reasons. Changes to a public function's body or docs only are no API change: they are in the call graphs above."
                )
            )
        };
        unsafe_html + &crate::graphs::overview_html(d) + &public
    };
    let meta = &d.out.meta;
    let html = format!(
        r#"<h1>{}</h1>
        {}
        {}
        {}
        {}
        {}
        {}
        {}"#,
        esc(meta
            .pr
            .as_ref()
            .and_then(|p| p.title.as_deref())
            .unwrap_or(&meta.title)),
        match d.compared_html() {
            c if c.is_empty() => c,
            c => format!(r#"<div class="sub pr-meta">{c}</div>"#),
        },
        crate::commit_filter::banner_html(d),
        description_html(d),
        crate::threads::overview_html(d),
        crate::files::manifests_html(d),
        rust_html,
        crate::files::other_files_html(d),
    );
    dom::set_html("view-overview", &html);
    sync_header_title();
}

/// The overview's heading is the PR's title, so while it is in view the
/// header hides its own copy of the title (and the details under it,
/// which the overview shows in full). It comes back once the heading
/// scrolls away, and in the other views.
pub fn sync_header_title() {
    let (Some(title), Some(view)) = (dom::by_id("pr-title"), dom::by_id("view-overview")) else {
        return;
    };
    let shown_below = view.class_list().contains("active")
        && view.query_selector("h1").ok().flatten().is_some_and(|h| {
            h.get_bounding_client_rect().bottom() > view.get_bounding_client_rect().top()
        });
    dom::toggle_class(&title, "on-page", shown_below);
}

/// How many public API changes are breaking, and the version bump that
/// follows from that: for each crate whose version is known, the exact
/// version, and whether the PR's own version is enough.
fn semver_summary(d: &Data) -> String {
    use crate::bump::Level;
    use model::ApiLevel;
    use std::collections::BTreeMap;
    // Breaking, allowed breaking, to check by hand, non-breaking.
    let mut counts = [0usize; 4];
    // The most severe level per crate.
    let mut crates: BTreeMap<&str, Level> = BTreeMap::new();
    // Crates whose allowed breaking changes are left out of the version
    // they need.
    let mut allowed: Vec<&str> = Vec::new();
    for it in d.items().filter(|it| d.in_public_api(it) && changed(it)) {
        if let Some(api) = &it.api {
            let krate = it.info.id.split("::").next().unwrap_or(&it.info.id);
            let (i, level) = match api.level {
                ApiLevel::Breaking if d.breaking_allowed(it) => {
                    if !allowed.contains(&krate) {
                        allowed.push(krate);
                    }
                    counts[1] += 1;
                    continue;
                }
                ApiLevel::Breaking => (0, Level::Breaking),
                ApiLevel::Unknown => (2, Level::Unknown),
                ApiLevel::Compatible => (3, Level::Compatible),
            };
            counts[i] += 1;
            // A crate that allows breaking changes doesn't follow SemVer:
            // the version it needs isn't for this page to say.
            if d.breaking_allowed(it) {
                continue;
            }
            let worst = crates.entry(krate).or_insert(level);
            *worst = match (*worst, level) {
                (Level::Breaking, _) | (_, Level::Breaking) => Level::Breaking,
                (Level::Unknown, _) | (_, Level::Unknown) => Level::Unknown,
                _ => Level::Compatible,
            };
        }
    }
    let [breaking, allowed_n, unknown, compatible] = counts;
    if breaking + allowed_n + unknown + compatible == 0 {
        // A failed run of cargo-semver-checks still says why.
        return semver_checks_html(d);
    }
    let versions: Vec<String> = crates
        .iter()
        .filter_map(|(name, level)| {
            crate::bump::sentence(name, d.out.meta.versions.get(*name)?, *level)
        })
        .collect();
    // The levels in crates that follow SemVer.
    let level_in_rules = |l: Level| crates.values().any(|v| *v == l);
    let rules = if !versions.is_empty() {
        format!(
            r#"<span title="Compared with the version on the base branch. If that one isn't released yet, the release may need less.">By Cargo's SemVer rules, {}</span>"#,
            versions.join(" ")
        )
    } else if level_in_rules(Level::Breaking) {
        "By Cargo's SemVer rules, this needs a major version bump (minor before 1.0).".to_string()
    } else if level_in_rules(Level::Unknown) {
        "By Cargo's SemVer rules, this needs at least a minor version bump, depending on the changes to check by hand.".to_string()
    } else if level_in_rules(Level::Compatible) {
        "By Cargo's SemVer rules, this needs a minor version bump (patch before 1.0).".to_string()
    } else {
        String::new()
    };
    let code_list = |names: &[&str]| {
        names
            .iter()
            .map(|n| format!("<code>{}</code>", esc(n)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut notes = Vec::new();
    if !allowed.is_empty() {
        notes.push(format!(
            "Breaking changes are allowed in {} (<code>allow-breaking-changes</code> in its <code>Cargo.toml</code>).",
            code_list(&allowed)
        ));
    }
    // The setting counts from the base: one the PR adds applies to later
    // PRs only.
    let mut added: Vec<&str> = d
        .out
        .meta
        .versions
        .iter()
        .filter(|(_, v)| v.allow_breaking_head && !v.allow_breaking)
        .map(|(n, _)| n.as_str())
        .collect();
    added.sort();
    if !added.is_empty() {
        notes.push(format!(
            "This PR allows breaking changes in {}; that applies once it is merged, not to its own changes.",
            code_list(&added)
        ));
    }
    let part = |n: usize, what: &str, css: &str| {
        (n > 0).then(|| format!(r#"<span class="api api-{css}">{n} {what}</span>"#))
    };
    let parts: Vec<String> = [
        part(breaking, "breaking", "breaking"),
        part(allowed_n, "breaking (allowed)", "allowed"),
        part(unknown, "to check by hand", "unknown"),
        part(compatible, "non-breaking", "compatible"),
    ]
    .into_iter()
    .flatten()
    .collect();
    format!(
        r#"<div class="semver-sum">{} <span class="dim">{rules} {} Hover a label for why.</span></div>{}{}"#,
        parts.join(" "),
        notes.join(" "),
        changes_list(d),
        semver_checks_html(d),
    )
}

/// Public API changes shown before the rest of their group is folded.
const SHOWN_CHANGES: usize = 10;

/// Every public API change by its public path, with its label and first
/// reason, each a link to the item on its module page: breaking ones
/// first, then those to check by hand, the non-breaking ones and the
/// allowed breaking ones. Also unchanged items that cargo-semver-checks
/// labeled, e.g. a type that is no longer `Send` because of a change
/// elsewhere.
fn changes_list(d: &Data) -> String {
    use model::ApiLevel;
    let mut items: Vec<&Item> = d
        .items()
        .filter(|it| it.api.is_some() && d.in_public_api(it))
        .collect();
    items.sort_by(|a, b| d.reading_order(a, b));
    let group = |it: &Item| match it.api.as_ref().map(|a| a.level) {
        Some(ApiLevel::Breaking) if d.breaking_allowed(it) => 3,
        Some(ApiLevel::Breaking) => 0,
        Some(ApiLevel::Unknown) => 1,
        _ => 2,
    };
    items.sort_by_key(|it| group(it));
    let line = |it: &&Item| {
        let path = d
            .public_path(it)
            .map(str::to_string)
            .unwrap_or_else(|| d.display_name(it));
        let why = it
            .api
            .as_ref()
            .and_then(|a| a.reasons.first())
            .map(|r| format!(r#" <span class="dim">{}</span>"#, crate::code::note_html(r)))
            .unwrap_or_default();
        // An impl's name already says "impl".
        let kind = if it.info.kind == Kind::Impl {
            String::new()
        } else {
            format!(r#"<span class="kind">{}</span>"#, it.info.kind.label())
        };
        format!(
            r#"<li>{} {kind}<a data-goto-item="{}"{}><code>{}</code></a>{why}</li>"#,
            d.api_badge(it),
            esc(&it.info.id),
            crate::nav::row_href(&it.info.id),
            crate::review::item_tag(&it.info.id, &path),
        )
    };
    let mut html = String::new();
    for g in 0..4 {
        let of: Vec<&&Item> = items.iter().filter(|it| group(it) == g).collect();
        if of.is_empty() {
            continue;
        }
        // Breaking changes and those to check by hand are all shown.
        let shown = if g < 2 { of.len() } else { SHOWN_CHANGES };
        let head: String = of.iter().take(shown).copied().map(line).collect();
        html.push_str(&format!(r#"<ul class="api-list">{head}</ul>"#));
        if of.len() > shown {
            let rest: String = of.iter().skip(shown).copied().map(line).collect();
            html.push_str(&format!(
                r#"<details class="api-fold"><summary>{} more</summary><ul class="api-list">{rest}</ul></details>"#,
                of.len() - shown
            ));
        }
    }
    html
}

/// How cargo-semver-checks was run, and what it found that no item shows.
fn semver_checks_html(d: &Data) -> String {
    let Some(r) = &d.out.semver_checks else {
        return String::new();
    };
    if let Some(e) = &r.error {
        return format!(
            r#"<div class="semver-tool warn">cargo-semver-checks {} could not check this PR, so the labels come from the definitions alone: <pre>{}</pre></div>"#,
            esc(&r.version),
            esc(e)
        );
    }
    let unmatched: String = r
        .findings
        .iter()
        .filter(|f| f.item.is_none())
        .map(|f| {
            format!(
                r#"<li><span class="api api-{}">{}</span> {}: <code>{}</code>{} <span class="dim">({})</span></li>"#,
                f.level.css(),
                f.level.label(),
                esc(&f.title),
                esc(&f.message),
                f.location
                    .as_deref()
                    .map(|l| format!(r#" <span class="dim">at {}</span>"#, esc(l)))
                    .unwrap_or_default(),
                esc(&f.lint)
            )
        })
        .collect();
    let matched = r.findings.iter().filter(|f| f.item.is_some()).count();
    format!(
        r#"<div class="semver-tool">Also checked with cargo-semver-checks {} ({} lints, {}, {:.0} s): {} finding{} on the items below{}.{}</div>"#,
        esc(&r.version),
        r.checks,
        esc(&r.features),
        r.seconds,
        matched,
        if matched == 1 { "" } else { "s" },
        if unmatched.is_empty() {
            ""
        } else {
            ", and these, which match no item"
        },
        if unmatched.is_empty() {
            String::new()
        } else {
            format!(r#"<ul class="semver-unmatched">{unmatched}</ul>"#)
        }
    )
}

/// Descriptions up to this many lines are shown open.
const SHORT_DESCRIPTION: usize = 20;

/// The PR's description, rendered as Markdown like a doc comment: raw HTML
/// is shown as text, except HTML comments, which GitHub doesn't show
/// either (PR templates use them for instructions). Images are left out,
/// so the page never loads anything from elsewhere.
fn description_html(d: &Data) -> String {
    let Some(body) = d.out.meta.pr.as_ref().and_then(|p| p.body.as_deref()) else {
        return String::new();
    };
    let text = strip_html_comments(&body.replace("\r\n", "\n"));
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let lines = text.lines().count();
    format!(
        r#"<details class="pr-desc"{}><summary>Description <span class="dim">{lines} line{}</span></summary><div class="docs">{}</div></details>"#,
        if lines <= SHORT_DESCRIPTION {
            " open"
        } else {
            ""
        },
        if lines == 1 { "" } else { "s" },
        crate::code::markdown_file_html(d, text)
    )
}

/// Removes `<!-- … -->` comments outside fenced code blocks; an unclosed
/// one runs to the end, as in HTML. Lines that held only a comment are
/// dropped.
fn strip_html_comments(s: &str) -> String {
    let mut out = String::new();
    let mut fence: Option<&str> = None;
    let mut in_comment = false;
    for line in s.lines() {
        let t = line.trim_start();
        if !in_comment {
            if let Some(f) = fence {
                if t.starts_with(f) {
                    fence = None;
                }
                out.push_str(line);
                out.push('\n');
                continue;
            }
            if let Some(f) = ["```", "~~~"].into_iter().find(|f| t.starts_with(f)) {
                fence = Some(f);
                out.push_str(line);
                out.push('\n');
                continue;
            }
        }
        let mut kept = String::new();
        let mut rest = line;
        loop {
            if in_comment {
                match rest.find("-->") {
                    Some(j) => {
                        rest = &rest[j + 3..];
                        in_comment = false;
                    }
                    None => break,
                }
            } else {
                match rest.find("<!--") {
                    Some(i) => {
                        kept.push_str(&rest[..i]);
                        rest = &rest[i + 4..];
                        in_comment = true;
                    }
                    None => {
                        kept.push_str(rest);
                        break;
                    }
                }
            }
        }
        if kept.trim().is_empty() && kept.len() != line.len() {
            continue;
        }
        out.push_str(&kept);
        out.push('\n');
    }
    out
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
            .map(|it| {
                // Items of a new module: the module says "new" once.
                let group = d.item(&it.info.module).and_then(group_status);
                item_with_members(d, it, m, &filter, group)
            })
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
                r#"<div class="hidden-note">{hidden_changed} changed item{} private to <b>{}</b> and not visible from here. <a data-mod="{}"{}>Open the module</a> to see {}.</div>"#,
                if hidden_changed > 1 { "s are" } else { " is" },
                esc(&s.info.name),
                esc(&s.info.id),
                crate::nav::module_href(&s.info.id),
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
            r#"<div class="modbox {}"><div class="modbox-h"><span class="kind">mod</span>{} {} {}<span style="flex:1"></span><a data-mod="{}"{}>open ›</a></div>
            <div class="modbox-b">{body}{note}{empty}</div></div>"#,
            group_status(s).map_or(String::new(), |st| format!("whole-{}", st.css())),
            d.title_html(s, false),
            d.vis_html(s),
            d.status_html(s),
            esc(&s.info.id),
            crate::nav::module_href(&s.info.id),
        )
    }

    let crumbs = d
        .mod_chain(m)
        .iter()
        .map(|c| {
            format!(
                r#"<a data-mod="{}"{}>{}</a>"#,
                esc(c),
                crate::nav::module_href(c),
                esc(&d.item(c).map_or(String::new(), |i| d.display_name(i)))
            )
        })
        .collect::<Vec<_>>()
        .join("::");
    // Empty sections are left out; a page with none says so in one line.
    let exposed_html = section(exposed);
    let private_html = section(private);
    let mut sections = String::new();
    if !exposed_html.is_empty() {
        sections += &format!(r#"<h2>Exposed by this module</h2><div>{exposed_html}</div>"#);
    }
    if !private_html.is_empty() {
        sections += &format!(
            r#"<h2>Private to this module</h2><div class="private-zone">{private_html}</div>"#
        );
    }
    if !subs.is_empty() {
        sections += &format!(
            r#"<h2>Submodules {}</h2>{}"#,
            help_html("Only what is visible from here."),
            subs.iter()
                .map(|s| sub_box(d, s, m, &section))
                .collect::<String>()
        );
    }
    if sections.is_empty() {
        sections = format!(
            r#"<div class="empty-state">No {}items.</div>"#,
            if show_unchanged { "" } else { "changed " }
        );
    }
    let html = format!(
        r#"{}<div class="crumbs">{crumbs}</div>
        <h1><span class="kind">{}</span> {} {} {}</h1>
        <div class="sub">{} · <label><input type="checkbox" id="show-unchanged" {}> show unchanged items</label> ·
          <a class="btn" id="mod-graph"{}>Internal call graph{} ▸</a></div>
        {}
        {sections}"#,
        crate::commit_filter::banner_html(d),
        match (d.is_root(module), module.info.id.contains(':')) {
            // `binary repl`, `test calc`: the title says what it is.
            (true, true) => "",
            (true, false) => "crate",
            _ => "module",
        },
        d.title_html(module, false),
        if d.is_root(module) {
            String::new()
        } else {
            d.vis_html(module)
        },
        d.status_html(module),
        esc(&module.info.file),
        if show_unchanged { "checked" } else { "" },
        crate::nav::href(&crate::nav::Route::Canvas(Some(
            crate::nav::OnCanvas::ModuleGraph(m.to_string())
        ))),
        if crate::narrow() { "" } else { " on canvas" },
        if module.info.docs.is_empty() && !module.changes.docs {
            String::new()
        } else {
            d.docs_block_html(module, true)
        },
    );
    dom::set_html("view-module", &html);
}

#[cfg(test)]
mod tests {
    use super::strip_html_comments;

    #[test]
    fn html_comments_are_removed_outside_code() {
        let md = "<!-- Describe the change -->\n## What\nFix <!-- inline --> it.\n<!--\nmulti\nline -->\n```html\n<!-- kept -->\n```\nend";
        assert_eq!(
            strip_html_comments(md),
            "## What\nFix  it.\n```html\n<!-- kept -->\n```\nend\n"
        );
        // Unclosed: runs to the end.
        assert_eq!(strip_html_comments("a\n<!-- b\nc"), "a\n");
    }
}
