//! The embedded analysis plus derived indexes and privacy rules.

use crate::code::{Seg, esc, marked_html};
use model::{ApiLevel, External, Item, ItemInfo, Kind, Output, Status, UseCtx};
use std::collections::HashMap;

/// Body diffs with up to this many changed lines start open.
pub const SMALL_BODY: usize = 20;

pub struct Data {
    pub out: Output,
    /// Callers of each item: (caller id, guessed?).
    pub callers: HashMap<String, Vec<String>>,
    /// Users of each item: (user id, context).
    pub used_by: HashMap<String, Vec<(String, UseCtx)>>,
    /// Items listed directly in each module (no associated items).
    pub mod_children: HashMap<String, Vec<String>>,
    /// Whether an item or anything it contains changed.
    touched: HashMap<String, bool>,
    /// Items outside the analyzed crates, shown read-only, by path.
    ext: HashMap<String, Item>,
    /// Modules by each of their public paths.
    mod_by_path: HashMap<String, String>,
}

/// `m` is `s` or nested inside it.
pub fn within(m: &str, s: &str) -> bool {
    m == s || (m.len() > s.len() + 2 && m.starts_with(s) && m[s.len()..].starts_with("::"))
}

impl Data {
    pub fn new(out: Output) -> Data {
        let mut callers: HashMap<String, Vec<String>> = HashMap::new();
        let mut used_by: HashMap<String, Vec<(String, UseCtx)>> = HashMap::new();
        let mut mod_children: HashMap<String, Vec<String>> = HashMap::new();
        for item in out.items.values() {
            let i = &item.info;
            for c in &i.calls {
                callers.entry(c.to.clone()).or_default().push(i.id.clone());
            }
            for u in &i.uses {
                used_by
                    .entry(u.to.clone())
                    .or_default()
                    .push((i.id.clone(), u.ctx));
            }
            if i.id != i.module && i.parent.is_none() {
                mod_children
                    .entry(i.module.clone())
                    .or_default()
                    .push(i.id.clone());
            }
        }
        let ext = out
            .externals
            .iter()
            .map(|(path, e)| {
                let info = ItemInfo {
                    id: path.clone(),
                    kind: e.kind,
                    name: e.name.clone(),
                    module: e.module.clone(),
                    public: true,
                    sig: e.sig.clone(),
                    docs: e.docs.clone(),
                    ..Default::default()
                };
                let item = Item {
                    info,
                    ..Default::default()
                };
                (path.clone(), item)
            })
            .collect();
        let mut d = Data {
            out,
            callers,
            used_by,
            mod_children,
            touched: HashMap::new(),
            ext,
            mod_by_path: HashMap::new(),
        };
        for it in d.out.items.values() {
            if it.info.kind == Kind::Mod {
                for p in &it.info.public_paths {
                    d.mod_by_path.insert(p.clone(), it.info.id.clone());
                }
            }
        }
        let ids: Vec<String> = d.out.items.keys().cloned().collect();
        let mut memo = HashMap::new();
        for id in &ids {
            d.compute_touched(id, &mut memo);
        }
        d.touched = memo;
        d
    }

    fn compute_touched(&self, id: &str, memo: &mut HashMap<String, bool>) -> bool {
        if let Some(&t) = memo.get(id) {
            return t;
        }
        memo.insert(id.to_string(), false);
        let Some(it) = self.item(id) else {
            return false;
        };
        let mut r = it.status != Status::Unchanged;
        if it.info.kind == Kind::Mod {
            if !r {
                r = self
                    .children_of_mod(id)
                    .iter()
                    .any(|c| self.compute_touched(c, memo));
            }
        } else if !r {
            r = it
                .info
                .children
                .iter()
                .any(|c| c != id && self.compute_touched(c, memo));
        }
        memo.insert(id.to_string(), r);
        r
    }

    /// An item of the analyzed crates, or one outside them that the code
    /// names.
    pub fn item(&self, id: &str) -> Option<&Item> {
        self.out.items.get(id).or_else(|| self.ext.get(id))
    }

    /// Whether the item is part of the public API on either side.
    pub fn in_public_api(&self, it: &Item) -> bool {
        it.info.public || !it.changes.exports_removed.is_empty()
    }

    /// The path the item is listed under in the public API: where rustdoc
    /// documents it, the first public path (or, if it lost all, the first
    /// one it had).
    pub fn public_path<'a>(&self, it: &'a Item) -> Option<&'a str> {
        it.info
            .public_paths
            .first()
            .or(it.changes.exports_removed.first())
            .map(String::as_str)
    }

    /// The module whose public path contains the item's public path, i.e.
    /// where rustdoc lists it.
    pub fn public_module(&self, it: &Item) -> Option<&str> {
        let (parent, _) = self.public_path(it)?.rsplit_once("::")?;
        self.mod_by_path.get(parent).map(String::as_str)
    }

    /// Whether the item's own definition path is not how the public API
    /// names it (it is re-exported, perhaps renamed, or another crate's).
    pub fn reexported(&self, it: &Item) -> bool {
        if it.info.reexport_of.is_some() {
            return true;
        }
        let def = format!("{}::{}", it.info.module, it.info.name);
        it.info.parent.is_none()
            && it.info.kind != Kind::Mod
            && self.public_path(it).is_some_and(|p| p != def)
    }

    /// The item's SemVer label: breaking, non-breaking or "check by hand",
    /// with the reasons on hover.
    pub fn api_badge(&self, it: &Item) -> String {
        let Some(api) = &it.api else {
            return String::new();
        };
        let icon = match api.level {
            ApiLevel::Breaking => "⚠ ",
            ApiLevel::Unknown => "? ",
            ApiLevel::Compatible => "",
        };
        format!(
            r#"<span class="api api-{}" title="{}">{icon}{}</span>"#,
            api.level.css(),
            esc(&api.reasons.join("\n").replace('`', "")),
            api.level.label()
        )
    }

    /// Why the item's API change has its label, one line per reason.
    pub fn api_reasons_html(&self, it: &Item) -> String {
        let Some(api) = &it.api else {
            return String::new();
        };
        // Reasons the signature diff's notes already give stay out.
        let noted: Vec<&str> = [&it.sig_diff, &it.sig_diff_public]
            .into_iter()
            .flatten()
            .flat_map(|d| d.notes.iter().map(|n| n.text.as_str()))
            .collect();
        let lines: String = api
            .reasons
            .iter()
            .filter(|r| !noted.contains(&r.as_str()))
            .map(|r| format!("<li>{}</li>", crate::code::note_html(r)))
            .collect();
        if lines.is_empty() {
            return String::new();
        }
        format!(
            r#"<ul class="api-why api-{}">{lines}</ul>"#,
            api.level.css()
        )
    }

    /// Public paths gained and lost, marked like other changes.
    pub fn exports_segs(&self, it: &Item) -> Vec<Seg> {
        let c = &it.changes;
        if it.status != Status::Changed {
            return Vec::new();
        }
        let added = c.exports_added.iter().map(|p| {
            Seg::new(
                "m-add",
                format!(
                    r#"<div class="export"><span class="mk">+</span> now public as <code>{}</code></div>"#,
                    esc(p)
                ),
            )
        });
        let removed = c.exports_removed.iter().map(|p| {
            Seg::new(
                "m-del",
                format!(
                    r#"<div class="export"><span class="mk">−</span> no longer public as <code><s>{}</s></code></div>"#,
                    esc(p)
                ),
            )
        });
        added.chain(removed).collect()
    }

    /// Everything shown about an item's definition, in order, with
    /// neighboring parts that changed the same way under one marker.
    pub fn item_body_html(
        &self,
        it: &Item,
        scope: &str,
        full_docs: bool,
        src: Option<Seg>,
    ) -> String {
        let mut segs = Vec::new();
        if it.info.kind != Kind::Mod {
            segs.extend(self.sig_segs(it, scope));
        }
        segs.extend(self.exports_segs(it));
        segs.extend(self.docs_segs(it, full_docs));
        segs.extend(src);
        marked_html(segs)
    }

    /// What is known about an item outside the analyzed crates.
    pub fn external(&self, id: &str) -> Option<&External> {
        if self.out.items.contains_key(id) {
            return None;
        }
        self.out.externals.get(id)
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.out.items.values()
    }

    pub fn roots(&self) -> &[String] {
        &self.out.crates
    }

    /// The first crate, or `""` when the PR has no Rust crate.
    pub fn main_root(&self) -> &str {
        self.out.crates.first().map_or("", String::as_str)
    }

    pub fn children_of_mod(&self, m: &str) -> &[String] {
        self.mod_children.get(m).map_or(&[], Vec::as_slice)
    }

    pub fn mod_children_items(&self, m: &str) -> Vec<&Item> {
        self.children_of_mod(m)
            .iter()
            .filter_map(|c| self.item(c))
            .collect()
    }

    pub fn members(&self, it: &Item) -> Vec<&Item> {
        it.info
            .children
            .iter()
            .filter_map(|c| self.item(c))
            .collect()
    }

    /// Whether the PR adds, changes or removes any Rust item (modules
    /// themselves aside: a module only counts through what is in it).
    pub fn rust_changed(&self) -> bool {
        self.items()
            .any(|it| it.info.kind != Kind::Mod && changed(it))
    }

    pub fn touched(&self, it: &Item) -> bool {
        self.touched.get(&it.info.id).copied().unwrap_or(false)
    }

    pub fn is_root(&self, it: &Item) -> bool {
        it.info.kind == Kind::Mod && it.info.module == it.info.id
    }

    /// The module an item lives in (a module's own id for modules).
    /// The type an impl or associated item belongs to, if that type is in
    /// the same module; otherwise the item itself.
    pub fn type_anchor<'a>(&'a self, it: &'a Item) -> &'a Item {
        it.info
            .self_ty
            .as_deref()
            .filter(|t| *t != it.info.id)
            .and_then(|t| self.item(t))
            .filter(|t| t.info.module == it.info.module)
            .unwrap_or(it)
    }

    /// Reading order within a module: by position in the source, except
    /// that a type's impls and associated items come right after the type,
    /// wherever they are.
    pub fn reading_order(&self, a: &Item, b: &Item) -> std::cmp::Ordering {
        let key = |it: &Item| {
            let t = self.type_anchor(it);
            (
                t.info.file.clone(),
                t.info.line,
                t.info.id.clone(),
                t.info.id != it.info.id,
                it.info.file.clone(),
                it.info.line,
                it.info.id.clone(),
            )
        };
        key(a).cmp(&key(b))
    }

    pub fn mod_of<'a>(&self, it: &'a Item) -> &'a str {
        if it.info.kind == Kind::Mod {
            &it.info.id
        } else {
            &it.info.module
        }
    }

    /// Enclosing modules of `m`, outermost first, including `m`.
    pub fn mod_chain(&self, m: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = self.item(m);
        while let Some(it) = cur {
            out.insert(0, it.info.id.clone());
            if self.is_root(it) {
                break;
            }
            cur = self.item(&it.info.module);
        }
        out
    }

    pub fn mod_depth(&self, m: &str) -> usize {
        self.mod_chain(m).len().saturating_sub(1)
    }

    /// Changed items (not modules) in `m`, optionally including submodules.
    pub fn changes_in(&self, m: &str, deep: bool) -> Vec<&Item> {
        self.items()
            .filter(|it| {
                it.info.kind != Kind::Mod
                    && it.status != Status::Unchanged
                    && if deep {
                        within(self.mod_of(it), m)
                    } else {
                        self.mod_of(it) == m
                    }
            })
            .collect()
    }

    /// Whether item `id` can be named from code in module `scope`: the item
    /// and every enclosing module up to a common ancestor must be visible.
    pub fn visible_from(&self, id: &str, scope: &str) -> bool {
        let mut cur = self.item(id);
        for _ in 0..64 {
            let Some(x) = cur else { return true };
            if x.info.kind == Kind::Mod && within(scope, &x.info.id) {
                return true;
            }
            if let Some(vs) = &x.info.vis_scope
                && !within(scope, vs)
            {
                return false;
            }
            match &x.info.vis_parent {
                None => return true,
                Some(p) => cur = self.item(p),
            }
        }
        true
    }

    /// The module whose privacy hides `id` from `scope`.
    pub fn hiding_module(&self, id: &str, scope: &str) -> Option<String> {
        let mut cur = self.item(id);
        let mut hide = None;
        for _ in 0..64 {
            let Some(x) = cur else { break };
            if x.info.kind == Kind::Mod && within(scope, &x.info.id) {
                break;
            }
            if let Some(vs) = &x.info.vis_scope
                && !within(scope, vs)
            {
                hide = Some(vs.clone());
            }
            match &x.info.vis_parent {
                None => break,
                Some(p) => cur = self.item(p),
            }
        }
        hide
    }

    /// Not visible outside module `m`.
    pub fn private_to(&self, it: &Item, m: &str) -> bool {
        it.info.vis_scope.as_deref().is_some_and(|s| within(s, m))
    }

    pub fn short_path<'a>(&self, id: &'a str) -> &'a str {
        // Binaries, tests, … don't count: they are not libraries a path
        // could be confused with.
        let libraries = self.out.crates.iter().filter(|c| !c.contains(':')).count();
        if libraries != 1 || self.main_root().contains(':') {
            return id;
        }
        let root = self.main_root();
        match id.strip_prefix(root).and_then(|r| r.strip_prefix("::")) {
            Some(rest) => rest,
            None => id,
        }
    }

    pub fn display_name(&self, it: &Item) -> String {
        let i = &it.info;
        if i.kind == Kind::Mod && self.is_root(it) {
            return root_name(&i.id);
        }
        if matches!(i.kind, Kind::Mod | Kind::Impl) {
            return i.name.clone();
        }
        if let Some(parent) = i.parent.as_deref().and_then(|p| self.item(p)) {
            if parent.info.kind == Kind::Impl {
                // The self type as written in the impl header, so that
                // `impl<T> Log for Arc<T>` gives `<Arc<T> as Log>::log`.
                let (trait_text, self_text) = impl_parts(&parent.info.sig);
                let self_name = self_text.unwrap_or_else(|| {
                    parent
                        .info
                        .self_ty
                        .as_deref()
                        .and_then(|t| self.item(t))
                        .map_or("Self".to_string(), |t| t.info.name.clone())
                });
                return match trait_text {
                    Some(tr) => format!("<{self_name} as {tr}>::{}", i.name),
                    None => format!("{self_name}::{}", i.name),
                };
            }
            return format!("{}::{}", parent.info.name, i.name);
        }
        i.name.clone()
    }

    pub fn display_name_of(&self, id: &str) -> String {
        self.item(id)
            .map_or_else(|| id.to_string(), |it| self.display_name(it))
    }

    pub fn mod_label(&self, m: &str) -> String {
        match self.item(m) {
            Some(it) if self.is_root(it) => target_label(&it.info.id),
            Some(_) => format!("mod {}", self.short_path(m)),
            None => m.to_string(),
        }
    }

    pub fn title_html(&self, it: &Item, with_module: bool) -> String {
        if let Some(e) = self.external(&it.info.id) {
            return format!(
                r#"<span class="title-name"><span class="dim">{}::</span><span class="k-{}">{}</span></span>"#,
                esc(&e.module),
                it.info.kind.css(),
                esc(&e.name)
            );
        }
        let prefix = if with_module
            && it.info.kind != Kind::Mod
            && !self.item(&it.info.module).is_some_and(|m| self.is_root(m))
        {
            format!(
                r#"<span class="dim">{}::</span>"#,
                esc(self.short_path(&it.info.module))
            )
        } else {
            String::new()
        };
        format!(
            r#"<span class="title-name">{prefix}<span class="k-{}">{}</span></span>"#,
            it.info.kind.css(),
            esc(&self.display_name(it))
        )
    }

    pub fn status_html(&self, it: &Item) -> String {
        if it.info.kind == Kind::Mod {
            return match it.status {
                Status::Added => r#"<span class="st st-added badge">New module</span>"#.into(),
                Status::Removed => r#"<span class="st st-removed badge">Removed</span>"#.into(),
                _ => {
                    // Changed items in the module and its submodules; say
                    // when they are in submodules, so that a module with
                    // nothing of its own doesn't look like it has work left.
                    let n = self.changes_in(&it.info.id, true).len();
                    let own = self.changes_in(&it.info.id, false).len();
                    match (n, own) {
                        (0, _) => String::new(),
                        (n, 0) => {
                            format!(r#"<span class="st st-inner">{n} changed in submodules</span>"#)
                        }
                        (n, own) if own == n => {
                            format!(r#"<span class="st st-inner">{n} changed</span>"#)
                        }
                        (n, own) => format!(
                            r#"<span class="st st-inner">{n} changed, {} in submodules</span>"#,
                            n - own
                        ),
                    }
                }
            };
        }
        let c = &it.changes;
        match it.status {
            Status::Added => r#"<span class="st st-added badge">New</span>"#.into(),
            Status::Removed => r#"<span class="st st-removed badge">Removed</span>"#.into(),
            // What changed is marked where it changed (signature, docs,
            // body), not summarized here.
            Status::Changed => {
                let mut s = String::new();
                if let Some(from) = &c.moved_from {
                    let from_mod = from.rsplit_once("::").map_or(from.as_str(), |(m, _)| m);
                    s.push_str(&format!(
                        r#"<span class="st st-changed" title="{}">moved from {}</span>"#,
                        esc(from),
                        esc(self.short_path(from_mod))
                    ));
                }
                s
            }
            Status::Unchanged if self.external(&it.info.id).is_some() => {
                r#"<span class="st st-unchanged">outside the analyzed crates</span>"#.into()
            }
            Status::Unchanged => {
                if c.cosmetic {
                    r#"<span class="st st-cosmetic" title="Only formatting, comments or import style differ">formatting only</span>"#.into()
                } else if self.members(it).iter().any(|m| self.touched(m)) {
                    r#"<span class="st st-inner">members changed</span>"#.into()
                } else {
                    r#"<span class="st st-unchanged">unchanged</span>"#.into()
                }
            }
        }
    }

    /// The visibility of a module. Other items show it in their signature.
    pub fn vis_html(&self, it: &Item) -> String {
        let i = &it.info;
        let test = if i.test_only {
            r#"<span class="st st-test" title="Compiled only for tests: marked #[test] or #[cfg(test)], or inside test-only code">test-only</span>"#
        } else {
            ""
        };
        if i.kind != Kind::Mod {
            return test.to_string();
        }
        let class = if i.vis_label == "pub" {
            "vis pub"
        } else {
            "vis"
        };
        let title = match &i.vis_scope {
            Some(s) => format!("visible in {s}"),
            None => "public".into(),
        };
        format!(
            r#"{test}<span class="{class}" title="{}">{}</span>"#,
            esc(&title),
            esc(&i.vis_label)
        )
    }

    /// The signature shown when looking from `scope`: private fields are
    /// hidden outside the defining module.
    pub fn sig_for<'a>(&self, it: &'a Item, scope: &str) -> &'a str {
        match &it.info.sig_public {
            Some(public) if !within(scope, self.mod_of(it)) => public,
            _ => &it.info.sig,
        }
    }
}

impl Data {
    /// What is compared: for a pull request the repository, PR number and
    /// title, and the base and PR branches with the compared commits.
    pub fn compared_html(&self) -> String {
        let meta = &self.out.meta;
        let Some(pr) = &meta.pr else {
            if meta.title.contains(&meta.base) {
                return format!("<b>{}</b>", esc(&meta.title));
            }
            return format!(
                "<b>{}</b> <span class=\"dim\">{} → {}</span>",
                esc(&meta.title),
                esc(&meta.base),
                esc(&meta.head)
            );
        };
        let repo_url = format!("https://github.com/{}", pr.repo);
        let pr_url = format!("{repo_url}/pull/{}", pr.number);
        let title = pr
            .title
            .as_deref()
            .map(|t| format!(" <b>{}</b>", esc(t)))
            .unwrap_or_default();
        let head = match (&pr.head_ref, &pr.head_owner) {
            (Some(r), Some(o)) => format!("<code>{}:{}</code>", esc(o), esc(r)),
            (Some(r), None) => format!("<code>{}</code>", esc(r)),
            _ => "PR head".to_string(),
        };
        format!(
            r#"<a href="{repo_url}" target="_blank" rel="noopener">github.com/{}</a> · <a href="{pr_url}" target="_blank" rel="noopener">PR #{}</a>{title} <span class="dim">· base <code>{}</code> <span title="merge base">@ {}</span> ← {head} @ {}</span>"#,
            esc(&pr.repo),
            pr.number,
            esc(&pr.base_ref),
            esc(&pr.base_rev),
            esc(&pr.head_rev),
        )
    }

    /// The PR in the header: its title and number, then a small line with
    /// the repository and the compared branches. The full details are in
    /// the tooltip, and on the overview.
    pub fn header_html(&self) -> String {
        let meta = &self.out.meta;
        let Some(pr) = &meta.pr else {
            let sub = if meta.title.contains(&meta.base) {
                String::new()
            } else {
                format!("{} → {}", meta.base, meta.head)
            };
            return format!(
                r#"<div class="hd-main" title="{}"><b>{}</b></div><div class="hd-sub">{}</div>"#,
                esc(format!("{}\n{sub}", meta.title).trim_end()),
                esc(&meta.title),
                esc(&sub)
            );
        };
        let pr_url = format!("https://github.com/{}/pull/{}", pr.repo, pr.number);
        let title = pr.title.as_deref().unwrap_or("");
        let head = match (&pr.head_ref, &pr.head_owner) {
            (Some(r), Some(o)) => format!("{o}:{r}"),
            (Some(r), None) => r.clone(),
            _ => "PR head".to_string(),
        };
        let tip = format!(
            "{} #{}: {title}\nbase {} @ {} (merge base) ← {head} @ {}",
            pr.repo, pr.number, pr.base_ref, pr.base_rev, pr.head_rev
        );
        format!(
            r#"<div class="hd-main" title="{tip}"><b>{}</b> <a href="{pr_url}" target="_blank" rel="noopener">#{}</a></div><div class="hd-sub" title="{tip}">{} · <code>{}</code> ← <code>{}</code></div>"#,
            esc(title),
            pr.number,
            esc(&pr.repo),
            esc(&pr.base_ref),
            esc(&head),
            tip = esc(&tip),
        )
    }

    /// The signature block of an item seen from `scope`, with a bar on the
    /// left that tells at a glance whether it changed: a changed signature
    /// is shown as a diff against the base version, an unchanged one is
    /// shown muted, for reference.
    #[cfg(test)]
    pub fn sig_html(&self, it: &Item, scope: &str) -> String {
        marked_html(self.sig_segs(it, scope))
    }

    pub fn sig_segs(&self, it: &Item, scope: &str) -> Vec<Seg> {
        let (bar, class, title) = match it.status {
            // Added and removed items are marked as a whole.
            Status::Added => ("", "", "added"),
            Status::Removed => ("", "", "removed"),
            _ if it.changes.signature => ("m-chg", "", "signature changed"),
            _ => ("", "m-ref", "signature unchanged"),
        };
        let shown = format!(
            r#"<div class="sigbox {class}" title="{title}">{}</div>"#,
            self.sig_inner(it, scope)
        );
        vec![
            Seg::new(bar, shown),
            Seg::new("", self.private_def_html(it, scope)),
        ]
    }

    /// Seen from outside its module, a type's private fields are hidden.
    /// This offers the full definition, as seen from inside, as an
    /// expandable section; with a diff if it changed.
    fn private_def_html(&self, it: &Item, scope: &str) -> String {
        let has_private = it.info.sig_public.is_some() || it.old_sig_public.is_some();
        if !has_private || within(scope, self.mod_of(it)) {
            return String::new();
        }
        // The full text changed (not only where names point).
        let changed = it.status == Status::Changed && it.changes.signature && it.old_sig.is_some();
        let (bar, class) = if changed {
            ("m-chg", "")
        } else {
            ("", "m-ref")
        };
        // As seen from the defining module: every field.
        let full = format!(
            r#"<div class="sigbox {class}">{}</div>"#,
            self.sig_inner(it, self.mod_of(it))
        );
        format!(
            r#"<details class="private-def"><summary>{}</summary>{}</details>"#,
            if changed {
                "<b>Full definition changed</b>, including private fields"
            } else {
                "Full definition, including private fields"
            },
            marked_html([Seg::new(bar, full)])
        )
    }

    /// The item's docs, with changes marked. Listings show only the first
    /// paragraph of unchanged docs (`full: false`).
    pub fn docs_segs(&self, it: &Item, full: bool) -> Vec<Seg> {
        use crate::code::{docs_diff_segs, docs_html, first_paragraph};
        let docs = &it.info.docs;
        if it.status == Status::Changed && it.changes.docs {
            let old = it.old_docs.as_deref().unwrap_or("");
            let mut segs = docs_diff_segs(self, old, docs, &it.info.doc_refs);
            segs.push(Seg::new(
                "",
                format!(
                    r#"<div class="docs-acts"><button class="mini" data-compare-docs="{}">Compare docs side by side</button></div>"#,
                    esc(&it.info.id)
                ),
            ));
            return segs;
        }
        // Added and removed items are marked as a whole, not per part.
        let bar = "";
        let md = if full {
            docs.as_str()
        } else {
            first_paragraph(docs)
        };
        let html = if !md.trim().is_empty() {
            format!(
                r#"<div class="docs"><div class="dblk">{}</div></div>"#,
                docs_html(self, md, &it.info.doc_refs)
            )
        } else if full {
            r#"<div class="docs empty">No documentation.</div>"#.to_string()
        } else {
            String::new()
        };
        vec![Seg::new(bar, html)]
    }

    pub fn docs_block_html(&self, it: &Item, full: bool) -> String {
        marked_html(self.docs_segs(it, full))
    }

    /// The item's source as an expandable section, summarized by what
    /// changed in it. `open`: whether the reader opened or closed it,
    /// if they did. `card`: inside a canvas card, which remembers that.
    #[cfg(test)]
    pub fn source_html(&self, it: &Item, open: Option<bool>, card: bool) -> String {
        marked_html(self.source_seg(it, open, card, &Default::default()))
    }

    /// `unfolded`: the folds of unchanged lines that were expanded, by
    /// their first row.
    pub fn source_seg(
        &self,
        it: &Item,
        open: Option<bool>,
        card: bool,
        unfolded: &std::collections::BTreeSet<usize>,
    ) -> Option<Seg> {
        use crate::code::{Links, code_html};
        use similar::{Algorithm, DiffOp, capture_diff_slices};
        if it.source.is_empty() {
            return None;
        }
        let opts = crate::settings::get();
        // Both versions, from the stored diff, compared again with the
        // viewer's options.
        let old: Vec<&str> = it
            .source
            .iter()
            .filter(|l| l.0 != '+')
            .map(|l| l.1.as_str())
            .collect();
        let new: Vec<&str> = it
            .source
            .iter()
            .filter(|l| l.0 != '-')
            .map(|l| l.1.as_str())
            .collect();
        let key = |l: &&str| -> String {
            if opts.ignore_ws {
                l.split_whitespace().collect()
            } else {
                l.to_string()
            }
        };
        let (ok, nk): (Vec<String>, Vec<String>) =
            (old.iter().map(key).collect(), new.iter().map(key).collect());
        // Rows: (old line index, new line index); one of them for added
        // or removed lines.
        let mut rows: Vec<(Option<usize>, Option<usize>)> = Vec::new();
        let split = opts.split && it.status == Status::Changed;
        // Changed lines that pair up (the first old one with the first new
        // one, and so on), to have their changed words marked.
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        for op in capture_diff_slices(Algorithm::Myers, &ok, &nk) {
            let (o, n) = (op.old_range(), op.new_range());
            if let DiffOp::Replace { .. } = op {
                pairs.extend(o.clone().zip(n.clone()));
            }
            match op {
                DiffOp::Equal { .. } => rows.extend(o.zip(n).map(|(a, b)| (Some(a), Some(b)))),
                DiffOp::Replace { .. } if split => {
                    let len = o.len().max(n.len());
                    rows.extend((0..len).map(|k| {
                        (
                            (k < o.len()).then(|| o.start + k),
                            (k < n.len()).then(|| n.start + k),
                        )
                    }));
                }
                _ => {
                    rows.extend(o.map(|a| (Some(a), None)));
                    rows.extend(n.map(|b| (None, Some(b))));
                }
            }
        }
        let added = rows
            .iter()
            .filter(|r| {
                r.1.is_some() && (r.0.is_none() || split && ok[r.0.unwrap()] != nk[r.1.unwrap()])
            })
            .count();
        let removed = rows
            .iter()
            .filter(|r| {
                r.0.is_some() && (r.1.is_none() || split && ok[r.0.unwrap()] != nk[r.1.unwrap()])
            })
            .count();
        // Line numbers in the files the two versions come from.
        let (old_start, new_start) = match it.status {
            Status::Removed => (it.info.line, it.info.line),
            _ => (it.old_line.unwrap_or(it.info.line), it.info.line),
        };
        // What names in the base version's lines refer to.
        let old_refs = match it.status {
            Status::Removed => &it.info.source_refs,
            _ => &it.old_source_refs,
        };
        let old_links = |o: usize| Links::Source {
            line: old_start + o,
            refs: old_refs,
            of: &it.info.id,
        };
        // The block of moved lines (see `moved_lines`) that a removed (`-`)
        // or added (`+`) line is in.
        let moved = |side: char, i: usize| {
            it.moved_lines
                .iter()
                .find(|m| m.side == side && (m.start..m.start + m.len).contains(&(i as u32)))
        };
        // What the first line of a moved block says: where it went or came
        // from.
        let moved_note = |side: char, i: usize| -> String {
            let Some(m) = moved(side, i).filter(|m| m.start as usize == i) else {
                return String::new();
            };
            let dir = if side == '-' {
                "moved to"
            } else {
                "moved from"
            };
            let target = match self.item(&m.other) {
                Some(o) if m.other != it.info.id => format!(
                    r#"<a class="ref" data-id="{}" data-kind="{}" data-path="{}">{}</a>, line {}"#,
                    esc(&o.info.id),
                    o.info.kind.css(),
                    esc(&o.info.id),
                    esc(&self.display_name(o)),
                    m.other_line
                ),
                _ => format!("line {}", m.other_line),
            };
            format!(r#"<span class="mv-note">{dir} {target}</span>"#)
        };
        // Changed lines with their changed words marked. Moved lines are
        // shown whole.
        let mut marked_old: HashMap<usize, String> = HashMap::new();
        let mut marked_new: HashMap<usize, String> = HashMap::new();
        if it.status == Status::Changed {
            for &(o, n) in &pairs {
                if moved('-', o).is_some() || moved('+', n).is_some() {
                    continue;
                }
                let old_html = code_html(self, old[o], old_links(o));
                let new_html = code_html(
                    self,
                    new[n],
                    Links::Source {
                        line: new_start + n,
                        refs: &it.info.source_refs,
                        of: &it.info.id,
                    },
                );
                if let Some((a, b)) = crate::code::mark_changed_tokens(&old_html, &new_html) {
                    marked_old.insert(o, a);
                    marked_new.insert(n, b);
                }
            }
        }
        let new_code = |n: usize| {
            if let Some(html) = marked_new.get(&n) {
                return html.clone();
            }
            code_html(
                self,
                new[n],
                Links::Source {
                    line: new_start + n,
                    refs: &it.info.source_refs,
                    of: &it.info.id,
                },
            )
        };
        let old_code = |o: usize| {
            marked_old
                .get(&o)
                .cloned()
                .unwrap_or_else(|| code_html(self, old[o], old_links(o)))
        };
        let nonempty = |s: String| if s.is_empty() { " ".to_string() } else { s };
        let changed_row = |&(o, n): &(Option<usize>, Option<usize>)| match (o, n) {
            (Some(a), Some(b)) => ok[a] != nk[b],
            _ => true,
        };
        // One row of the diff.
        let row_html = |r: usize| -> String {
            let (o, n) = rows[r];
            if split {
                let same = matches!((o, n), (Some(a), Some(b)) if ok[a] == nk[b]);
                let cell = |idx: Option<usize>,
                            start: usize,
                            class: &str,
                            side: &str,
                            code: &dyn Fn(usize) -> String| match idx {
                    Some(i) => {
                        let tag = if class == "del" { '-' } else { '+' };
                        let mv = !same && moved(tag, i).is_some();
                        format!(
                            r#"<span class="no {side}">{}</span><span class="ln {}{}" data-t="{}">{}{}</span>"#,
                            start + i,
                            if same { "" } else { class },
                            if mv { " mv" } else { "" },
                            if same { String::new() } else { tag.to_string() },
                            if mv {
                                moved_note(tag, i)
                            } else {
                                String::new()
                            },
                            nonempty(code(i))
                        )
                    }
                    None => {
                        format!(r#"<span class="no {side}"></span><span class="ln gap"> </span>"#)
                    }
                };
                cell(o, old_start, "del", "o", &old_code)
                    + &cell(n, new_start, "add", "n", &new_code)
            } else {
                let (class, tag, idx, no, code) = match (o, n) {
                    (_, Some(b)) if o.is_some() => ("", ' ', b, new_start + b, new_code(b)),
                    (None, Some(b)) => ("add", '+', b, new_start + b, new_code(b)),
                    (Some(a), _) => ("del", '-', a, old_start + a, old_code(a)),
                    (None, None) => unreachable!(),
                };
                let mv = tag != ' ' && moved(tag, idx).is_some();
                format!(
                    r#"<span class="ln {class}{}" data-t="{}" data-no="{no}">{}{}</span>"#,
                    if mv { " mv" } else { "" },
                    if tag == ' ' {
                        String::new()
                    } else {
                        tag.to_string()
                    },
                    if mv {
                        moved_note(tag, idx)
                    } else {
                        String::new()
                    },
                    nonempty(code)
                )
            }
        };
        // Unchanged lines more than `CONTEXT` rows away from a change are
        // folded: a button in their place shows them. Runs of fewer than
        // `MIN_FOLD` lines are shown, since the button takes a line too.
        const CONTEXT: usize = 3;
        const MIN_FOLD: usize = 2;
        let mut gaps: Vec<std::ops::Range<usize>> = Vec::new();
        if rows.iter().any(changed_row) {
            let near = |i: usize| {
                rows[i.saturating_sub(CONTEXT)..(i + CONTEXT + 1).min(rows.len())]
                    .iter()
                    .any(changed_row)
            };
            let mut i = 0;
            while i < rows.len() {
                let start = i;
                while i < rows.len() && !near(i) {
                    i += 1;
                }
                if i - start >= MIN_FOLD {
                    gaps.push(start..i);
                }
                i += 1;
            }
        }
        // In review mode, each part between folds starts with the review
        // controls of the hunks that start in it (see `review::hunks`).
        let hunk_head = |part: std::ops::Range<usize>| -> String {
            let (mut olds, mut news) = (Vec::new(), Vec::new());
            for &(o, n) in rows[part].iter().filter(|r| changed_row(r)) {
                olds.extend(o.map(|a| old_start + a));
                news.extend(n.map(|b| new_start + b));
            }
            match crate::review::hunk_controls(&it.info.id, &olds, &news) {
                c if c.is_empty() => c,
                c => format!(r#"<span class="hunk-rv">{c}</span>"#),
            }
        };
        let mut folded = 0;
        let mut body = String::new();
        let mut r = 0;
        for g in &gaps {
            body += &hunk_head(r..g.start);
            body.extend((r..g.start).map(&row_html));
            let inner: String = g.clone().map(&row_html).collect();
            if unfolded.contains(&g.start) {
                body += &inner;
            } else {
                folded += 1;
                let arrow = if g.start == 0 {
                    "↑"
                } else if g.end == rows.len() {
                    "↓"
                } else {
                    "↕"
                };
                body += &format!(
                    r#"<span class="{}unfold" data-unfold="{}" title="Show these lines">{arrow} {} unchanged lines</span><span class="fold" hidden>{inner}</span>"#,
                    if split { "skip " } else { "ln skip " },
                    g.start,
                    g.len()
                );
            }
            r = g.end;
        }
        body += &hunk_head(r..rows.len());
        body.extend((r..rows.len()).map(&row_html));
        let code = format!(
            r#"<div class="src-code" data-item="{}">{}{}</div>"#,
            esc(&it.info.id),
            if split {
                format!(r#"<div class="sbs">{body}</div>"#)
            } else {
                format!("<pre>{body}</pre>")
            },
            if folded > 1 {
                format!(
                    r#"<button class="mini unfold-all" data-unfold-all>Show all {} lines</button>"#,
                    rows.len()
                )
            } else {
                String::new()
            }
        );
        // A small change to the body or definition starts open, the rest
        // behind the summary. So does a small new item, except an impl,
        // whose members follow it with their own source.
        let small = match it.status {
            Status::Changed => it.changes.body && (1..=SMALL_BODY).contains(&(added + removed)),
            Status::Added => it.info.kind != Kind::Impl && it.source.len() <= SMALL_BODY,
            _ => false,
        };
        let open = open.unwrap_or(small);
        // Changed lines that were moved, on either side.
        let moved_count = rows
            .iter()
            .filter(|&&(o, n)| changed_row(&(o, n)))
            .map(|&(o, n)| {
                usize::from(o.is_some_and(|a| moved('-', a).is_some()))
                    + usize::from(n.is_some_and(|b| moved('+', b).is_some()))
            })
            .sum::<usize>();
        let stats = format!(
            r#"<span class="d-add">+{added}</span> <span class="d-del">−{removed}</span>{}{}"#,
            if moved_count > 0 {
                format!(r#" <span class="d-mv">({moved_count} moved)</span>"#)
            } else {
                String::new()
            },
            if opts.ignore_ws {
                r#" <span class="dim">(whitespace hidden)</span>"#
            } else {
                ""
            }
        );
        let where_ = format!("{}:{}", esc(&it.info.file), it.info.line);
        let (class, summary) = match it.status {
            Status::Changed if it.changes.body => {
                let what = if it.info.kind.is_callable() {
                    "Body changed"
                } else {
                    "Definition changed"
                };
                ("m-chg", format!("<b>{what}</b> {stats} · {where_}"))
            }
            Status::Added | Status::Removed => {
                let moved = if moved_count > 0 {
                    format!(r#", <span class="d-mv">{moved_count} moved</span>"#)
                } else {
                    String::new()
                };
                let what = if it.status == Status::Added {
                    "Source"
                } else {
                    "Removed source"
                };
                let n = it.source.len();
                ("", format!("{what} · {where_} · {n} lines{moved}"))
            }
            _ if it.changes.cosmetic => (
                "",
                format!("Source {stats} (formatting, comments or imports only) · {where_}"),
            ),
            _ if added + removed > 0 => ("", format!("Source {stats} · {where_}")),
            _ => ("", format!("Source · {where_} · {} lines", it.source.len())),
        };
        let html = format!(
            r#"<details class="src {}" {} {}><summary>{summary}</summary>{code}</details>"#,
            // The marker is drawn around it; this only colors the summary.
            class.replace("m-", "src-"),
            if card {
                format!(r#"data-src="{}""#, esc(&it.info.id))
            } else {
                String::new()
            },
            if open { "open" } else { "" },
        );
        Some(Seg::new(class, html))
    }

    fn sig_inner(&self, it: &Item, scope: &str) -> String {
        use crate::code::{Links, code_html, sig_diff_html};
        let new = self.sig_for(it, scope);
        let refs = &it.info.sig_refs;
        // Where the shown signature links names.
        let at: &[u32] = if it.info.sig_public.as_deref() == Some(new) {
            &it.info.sig_public_links
        } else {
            &it.info.sig_links
        };
        let plain = |note: &str| {
            let note = if note.is_empty() {
                String::new()
            } else {
                format!(r#"<div class="sig-note">{note}</div>"#)
            };
            format!(
                r#"<pre class="sig">{}</pre>{note}"#,
                code_html(self, new, Links::Sig { refs, at, base: 0 })
            )
        };
        if !it.changes.signature {
            return plain("");
        }
        let public_view = !within(scope, self.mod_of(it));
        let has_public = it.info.sig_public.is_some() || it.old_sig_public.is_some();
        let structured = if public_view && has_public {
            it.sig_diff_public.as_ref()
        } else {
            it.sig_diff.as_ref()
        };
        match structured {
            Some(diff)
                if diff
                    .spans
                    .iter()
                    .any(|s| !matches!(s, model::DiffSpan::Same { .. })) =>
            {
                crate::code::structured_diff_html(self, diff, refs, at)
            }
            Some(_) if public_view && has_public => plain(
                "Only private parts of the definition changed; expand the full definition below.",
            ),
            Some(_) => plain("The signature changed in a way the diff can't show."),
            // The analysis could not parse the signatures: token diff.
            None => match &it.old_sig {
                Some(old_full) => {
                    let old = if public_view {
                        it.old_sig_public.as_deref().unwrap_or(old_full)
                    } else {
                        old_full
                    };
                    if old == new {
                        plain(
                            "Only private parts of the definition changed; expand the full definition below.",
                        )
                    } else {
                        format!(
                            r#"<div class="sig-block"><pre class="sig sig-diff">{}</pre></div>"#,
                            sig_diff_html(self, old, new, refs, at)
                        )
                    }
                }
                None => {
                    plain("Same text, but names in the signature now refer to different items.")
                }
            },
        }
    }
}

/// The trait and self type of an impl, as written in its header
/// (`impl<T> Log for Arc<T> where …` → `Some("Log")`, `Some("Arc<T>")`).
pub fn impl_parts(header: &str) -> (Option<String>, Option<String>) {
    let line = header.lines().next().unwrap_or("");
    let line = line.split(" where ").next().unwrap_or(line).trim();
    let rest = line.strip_prefix("unsafe ").unwrap_or(line);
    let Some(mut rest) = rest.strip_prefix("impl") else {
        return (None, None);
    };
    // Skip the impl's own generic parameters.
    if rest.starts_with('<') {
        let mut depth = 0;
        for (i, c) in rest.char_indices() {
            match c {
                '<' => depth += 1,
                // `->` in `F: Fn() -> u8` is not a closing bracket.
                '>' if rest[..i].ends_with('-') => {}
                '>' => {
                    depth -= 1;
                    if depth == 0 {
                        rest = &rest[i + 1..];
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let rest = rest.trim();
    // ` for ` outside of angle brackets separates trait and self type.
    let mut depth = 0i32;
    let mut split = None;
    for (i, c) in rest.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' if rest[..i].ends_with('-') => {}
            '>' | ')' | ']' => depth -= 1,
            ' ' if depth == 0 && rest[i..].starts_with(" for ") => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let clean = |s: &str| {
        let s = s.trim().trim_start_matches('!');
        // Paths are shown by their last segment, like everywhere else.
        let base = s.split('<').next().unwrap_or(s);
        let short = base.rsplit("::").next().unwrap_or(base);
        format!("{short}{}", &s[base.len()..])
    };
    match split {
        Some(i) => (
            Some(clean(&rest[..i])),
            Some(rest[i + 5..].trim().to_string()),
        ),
        None if rest.is_empty() => (None, None),
        None => (None, Some(rest.to_string())),
    }
}

/// How a crate root is called: `crate tinycalc`, `binary tool`, `test it`,
/// `build script`, …
pub fn target_label(id: &str) -> String {
    match id.split_once(':') {
        None => format!("crate {id}"),
        Some(("build", _)) => "build script".to_string(),
        Some((kind, name)) => {
            let kind = match kind {
                "bin" => "binary",
                other => other,
            };
            format!("{kind} {name}")
        }
    }
}

/// The name a crate root is listed under (the label without "crate").
pub fn root_name(id: &str) -> String {
    let label = target_label(id);
    label
        .strip_prefix("crate ")
        .map_or(label.clone(), str::to_string)
}

/// Sort order of items in listings: by kind (as rustdoc does), then name.
pub fn kind_order(a: &Item, b: &Item) -> std::cmp::Ordering {
    let rank = |k: Kind| {
        [
            Kind::Mod,
            Kind::Macro,
            Kind::Struct,
            Kind::Enum,
            Kind::Union,
            Kind::Trait,
            Kind::TypeAlias,
            Kind::Fn,
            Kind::Const,
            Kind::Static,
            Kind::Impl,
        ]
        .iter()
        .position(|&x| x == k)
        .unwrap_or(99)
    };
    rank(a.info.kind)
        .cmp(&rank(b.info.kind))
        .then_with(|| a.info.name.cmp(&b.info.name))
}

pub fn changed(it: &Item) -> bool {
    it.status != Status::Unchanged
}
