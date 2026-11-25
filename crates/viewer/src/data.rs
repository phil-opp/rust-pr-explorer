//! The embedded analysis plus derived indexes and privacy rules.

use crate::code::{Seg, esc, marked_html};
use model::{ApiLevel, External, Item, ItemInfo, Kind, Output, Status, UseCtx};
use std::collections::HashMap;
use std::ops::Range;

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
    /// Where the PR's review threads are shown.
    pub placement: crate::threads::Placement,
    /// For removed items: the head line of the item that followed them in
    /// the base version (in the same file and module), if one is still
    /// there, to sort them where they were.
    removed_at: HashMap<String, Option<usize>>,
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
            placement: Default::default(),
            removed_at: HashMap::new(),
        };
        // Kept items by file and module: (base line, head line), sorted.
        let mut kept: HashMap<(&str, &str), Vec<(usize, usize)>> = HashMap::new();
        for it in d.out.items.values() {
            if !matches!(it.status, Status::Added | Status::Removed) {
                let file = it.old_file.as_deref().unwrap_or(&it.info.file);
                let old = it.old_line.unwrap_or(it.info.line);
                kept.entry((file, &it.info.module))
                    .or_default()
                    .push((old, it.info.line));
            }
        }
        for list in kept.values_mut() {
            list.sort();
        }
        d.removed_at = d
            .out
            .items
            .values()
            .filter(|it| it.status == Status::Removed)
            .map(|it| {
                let next = kept
                    .get(&(it.info.file.as_str(), it.info.module.as_str()))
                    .and_then(|list| {
                        let i = list.partition_point(|&(old, _)| old <= it.info.line);
                        list.get(i).map(|&(_, new)| new)
                    });
                (it.info.id.clone(), next)
            })
            .collect();
        for it in d.out.items.values() {
            if it.info.kind == Kind::Mod {
                for p in &it.info.public_paths {
                    d.mod_by_path.insert(p.clone(), it.info.id.clone());
                }
            }
        }
        d.touched = d.compute_all_touched();
        d.placement = crate::threads::place(&d);
        d
    }

    /// Whether each item or anything it contains changed (see
    /// [`changed`]).
    pub fn compute_all_touched(&self) -> HashMap<String, bool> {
        let mut memo = HashMap::new();
        for id in self.out.items.keys() {
            self.compute_touched(id, &mut memo);
        }
        memo
    }

    fn compute_touched(&self, id: &str, memo: &mut HashMap<String, bool>) -> bool {
        if let Some(&t) = memo.get(id) {
            return t;
        }
        memo.insert(id.to_string(), false);
        let Some(it) = self.item(id) else {
            return false;
        };
        let mut r = changed(it);
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

    /// Whether the item's crate allows breaking changes (see
    /// `CrateVersion::allow_breaking`).
    pub fn breaking_allowed(&self, it: &Item) -> bool {
        let krate = it.info.id.split("::").next().unwrap_or(&it.info.id);
        self.out
            .meta
            .versions
            .get(krate)
            .is_some_and(|v| v.allow_breaking)
    }

    /// A breaking change in a crate that doesn't allow them.
    pub fn breaks(&self, it: &Item) -> bool {
        it.api
            .as_ref()
            .is_some_and(|a| a.level == ApiLevel::Breaking)
            && !self.breaking_allowed(it)
    }

    /// The item's SemVer label: breaking, non-breaking or "check by hand",
    /// with the reasons on hover. In a crate that allows breaking changes,
    /// "breaking" is muted and says so.
    pub fn api_badge(&self, it: &Item) -> String {
        let Some(api) = &it.api else {
            return String::new();
        };
        if api.level == ApiLevel::Breaking && self.breaking_allowed(it) {
            return format!(
                r#"<span class="api api-allowed" title="{}&#10;&#10;Breaking changes are allowed in this crate (allow-breaking-changes in its Cargo.toml).">breaking (allowed)</span>"#,
                esc(&api.reasons.join("\n").replace('`', ""))
            );
        }
        let icon = match api.level {
            ApiLevel::Breaking => "⚠ ",
            ApiLevel::Unknown => "? ",
            ApiLevel::Compatible => "",
        };
        let mut title = api.reasons.join("\n").replace('`', "");
        if !api.confirmed_by.is_empty() {
            title.push_str(&format!(
                "\nConfirmed by cargo-semver-checks: {}",
                api.confirmed_by.join(", ")
            ));
        }
        format!(
            r#"<span class="api api-{}" title="{}">{icon}{}</span>"#,
            api.level.css(),
            esc(title.trim_start()),
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
            .chain((!api.confirmed_by.is_empty()).then(|| {
                format!(
                    r#"<li class="api-confirmed">✓ cargo-semver-checks agrees: {}</li>"#,
                    api.confirmed_by
                        .iter()
                        .map(|l| format!("<code>{}</code>", esc(l)))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }))
            .collect();
        if lines.is_empty() {
            return String::new();
        }
        let css = if api.level == ApiLevel::Breaking && self.breaking_allowed(it) {
            "allowed"
        } else {
            api.level.css()
        };
        format!(r#"<ul class="api-why api-{css}">{lines}</ul>"#)
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
        if let Some(t) = crate::commit_filter::touched(&it.info.id) {
            return t;
        }
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
    /// Removed items sort where they were in the base version: right
    /// before the item that followed them there, or at the end.
    pub fn reading_order(&self, a: &Item, b: &Item) -> std::cmp::Ordering {
        let pos = |it: &Item| match self.removed_at.get(&it.info.id) {
            Some(next) => (next.unwrap_or(usize::MAX), false, it.info.line),
            None => (it.info.line, true, 0),
        };
        let key = |it: &Item| {
            let t = self.type_anchor(it);
            (
                t.info.file.clone(),
                pos(t),
                t.info.id.clone(),
                t.info.id != it.info.id,
                it.info.file.clone(),
                pos(it),
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
                    && changed(it)
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

    /// A name's target by its path, linked when it is an item of the page.
    pub fn ref_link(&self, r: &model::Ref) -> String {
        let path = esc(self.short_path(&r.path));
        match r.id.as_deref().and_then(|id| self.item(id)) {
            Some(o) => format!(
                r#"<a class="ref" data-id="{}" data-kind="{}" data-path="{}"{}>{path}</a>"#,
                esc(&o.info.id),
                o.info.kind.css(),
                esc(&o.info.id),
                crate::nav::item_href(&o.info.id),
            ),
            None => format!("<code>{path}</code>"),
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

    /// [`Self::mod_label`] with the path as code: "mod <code>parser::lexer</code>".
    pub fn mod_label_html(&self, m: &str) -> String {
        let label = self.mod_label(m);
        match label.split_once(' ') {
            Some((kind, path)) => format!("{} <code>{}</code>", esc(kind), esc(path)),
            None => format!("<code>{}</code>", esc(&label)),
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
    /// Test code gets a tag in front: "test" for a test function,
    /// "test-only" for other code that is compiled only for tests.
    pub fn vis_html(&self, it: &Item) -> String {
        let i = &it.info;
        let test = if i.test {
            test_tag()
        } else if i.test_only {
            r#"<span class="st st-test" title="Compiled only for tests: marked #[cfg(test)], or inside test-only code">test-only</span>"#
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
            r#"{test} <span class="{class}" title="{}">{}</span>"#,
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

/// What a page of the changes since an earlier commit compares.
const SINCE_TIP: &str = "Only what the PR changed since this earlier commit. \
What its base branch changed in between, through a merge or a rebase, is left out.";

/// The tag of a test function, for item rows, cards and compact lists.
pub fn test_tag() -> &'static str {
    r#"<span class="st st-test st-testfn" title="A test function: marked #[test], or with a test attribute such as #[tokio::test]">test</span>"#
}

/// [`test_tag`] if `it` is a test function, for compact lists of items.
pub fn test_tag_of(it: &Item) -> &'static str {
    if it.info.test { test_tag() } else { "" }
}

impl Data {
    /// What is compared, under the overview's title (which is the PR's
    /// title, so it isn't repeated here): for a pull request a line with
    /// the repository and PR number, then one row each for the base and
    /// the PR's branch, with the compared commits linked on GitHub. Empty
    /// when there is nothing to add to the title.
    pub fn compared_html(&self) -> String {
        let meta = &self.out.meta;
        let Some(pr) = &meta.pr else {
            if meta.title.contains(&meta.base) {
                return String::new();
            }
            return format!(
                r#"<div class="pm-revs"><span class="pm-k">base</span><code class="pm-ref">{}</code><span class="pm-k">head</span><code class="pm-ref">{}</code></div>"#,
                esc(&meta.base),
                esc(&meta.head)
            );
        };
        let repo_url = format!("https://github.com/{}", pr.repo);
        let pr_url = format!("{repo_url}/pull/{}", pr.number);
        let commit = |rev: &str, title: &str| {
            let short = rev.get(..10).unwrap_or(rev);
            format!(
                r#"<a href="{repo_url}/commit/{}" target="_blank" rel="noopener" title="{}"><code>{}</code></a>"#,
                esc(rev),
                esc(title),
                esc(short)
            )
        };
        let head_ref = match (&pr.head_ref, &pr.head_owner) {
            (Some(r), Some(o)) => format!("<code class=\"pm-ref\">{}:{}</code> ", esc(o), esc(r)),
            (Some(r), None) => format!("<code class=\"pm-ref\">{}</code> ", esc(r)),
            _ => String::new(),
        };
        let head_rev = if pr.head_commit.is_empty() {
            &pr.head_rev
        } else {
            &pr.head_commit
        };
        let head = format!(
            r#"<span class="pm-k">head</span><span class="pm-v">{head_ref}<span class="pm-at">{}{}</span></span>"#,
            if head_ref.is_empty() { "" } else { "@ " },
            commit(head_rev, "The PR's commit shown, on GitHub")
        );
        let base = if pr.since.is_some() {
            format!(
                r#"<span class="pm-k" title="{SINCE_TIP}">since</span><span class="pm-v" title="{SINCE_TIP}">{}</span>"#,
                commit(&pr.base_rev, "The earlier commit, on GitHub")
            )
        } else {
            let what = if pr.custom_base {
                "The base given with --base, on GitHub"
            } else {
                "The merge base, on GitHub"
            };
            format!(
                r#"<span class="pm-k">base</span><span class="pm-v"><code class="pm-ref">{}</code> <span class="pm-at">@ {}</span></span>"#,
                esc(&pr.base_ref),
                commit(&pr.base_rev, what)
            )
        };
        format!(
            r#"<div class="pm-pr"><a href="{repo_url}" target="_blank" rel="noopener">github.com/{}</a> · <a href="{pr_url}" target="_blank" rel="noopener">PR #{}</a></div><div class="pm-revs">{base}{head}</div>"#,
            esc(&pr.repo),
            pr.number,
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
        if pr.since.is_some() {
            let tip = format!(
                "{} #{}: {title}\nchanges since {} ← {head} @ {}\n{SINCE_TIP}",
                pr.repo, pr.number, pr.base_rev, pr.head_rev
            );
            return format!(
                r#"<div class="hd-main" title="{tip}"><b>{}</b> <a href="{pr_url}" target="_blank" rel="noopener">#{}</a></div><div class="hd-sub" title="{tip}">{} · since <code>{}</code> ← <code>{}</code></div>"#,
                esc(title),
                pr.number,
                esc(&pr.repo),
                esc(&pr.base_rev),
                esc(&head),
                tip = esc(&tip),
            );
        }
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
        // A line whose names now refer to other items (see
        // `Item::retargeted`) differs from its old version, with the same
        // text.
        let retargeted_at = |old_side: bool, i: usize| {
            it.retargeted
                .iter()
                .any(|r| (if old_side { r.old_line } else { r.line }) as usize == i)
        };
        let keys = |lines: &[&str], old_side: bool| -> Vec<String> {
            lines
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let k = key(l);
                    if retargeted_at(old_side, i) {
                        format!("{k}\u{0}{}", if old_side { '-' } else { '+' })
                    } else {
                        k
                    }
                })
                .collect()
        };
        let (ok, nk) = (keys(&old, true), keys(&new, false));
        // Rows: (old line index, new line index); one of them for added
        // or removed lines.
        let mut rows: Vec<(Option<usize>, Option<usize>)> = Vec::new();
        let split = opts.split && it.status == Status::Changed;
        // Changed lines that pair up (the first old one with the first new
        // one, and so on), to have their changed words marked.
        let mut pairs: Vec<(usize, usize)> = Vec::new();
        // Lines that were reformatted (see `Item::reformatted`), with their
        // group: groups are shown in their new form, as unchanged lines
        // with any added or removed tokens marked, and the lines between
        // them are diffed.
        let mut rf_old: HashMap<usize, &model::Reformatted> = HashMap::new();
        let mut rf_new: HashMap<usize, &model::Reformatted> = HashMap::new();
        let mut segments: Vec<(Range<usize>, Range<usize>, bool)> = Vec::new();
        let (mut po, mut pn) = (0, 0);
        if opts.reformat && matches!(it.status, Status::Changed | Status::Unchanged) {
            for g in &it.reformatted {
                let o = g.old as usize..(g.old + g.old_len) as usize;
                let n = g.new as usize..(g.new + g.new_len) as usize;
                if o.start < po || n.start < pn || o.end > old.len() || n.end > new.len() {
                    continue;
                }
                segments.push((po..o.start, pn..n.start, false));
                (po, pn) = (o.end, n.end);
                rf_old.extend(o.clone().map(|a| (a, g)));
                rf_new.extend(n.clone().map(|b| (b, g)));
                segments.push((o, n, true));
            }
        }
        segments.push((po..old.len(), pn..new.len(), false));
        for (so, sn, reformatted) in segments {
            if reformatted {
                // Inline, only the new version; side by side, both.
                let len = if split {
                    so.len().max(sn.len())
                } else {
                    sn.len()
                };
                rows.extend((0..len).map(|k| {
                    (
                        (split && k < so.len()).then(|| so.start + k),
                        (k < sn.len()).then(|| sn.start + k),
                    )
                }));
                continue;
            }
            for op in capture_diff_slices(Algorithm::Myers, &ok[so.clone()], &nk[sn.clone()]) {
                let o = op.old_range().start + so.start..op.old_range().end + so.start;
                let n = op.new_range().start + sn.start..op.new_range().end + sn.start;
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
        }
        // A row of reformatted lines.
        let rf_row = |&(o, n): &(Option<usize>, Option<usize>)| {
            (o.is_some() || n.is_some())
                && o.is_none_or(|a| rf_old.contains_key(&a))
                && n.is_none_or(|b| rf_new.contains_key(&b))
        };
        // Reformatted lines with tokens added (on the new side) or removed
        // (on the old side; inline, they are shown on the new side).
        let rf_added = |b: usize| rf_new[&b].added.iter().any(|t| t.line as usize == b);
        let rf_removed = |a: usize| rf_old[&a].removed.iter().any(|t| t.old.line as usize == a);
        let changed_row = |r: &(Option<usize>, Option<usize>)| match *r {
            (o, n) if rf_row(r) => {
                n.is_some_and(|b| {
                    if split {
                        rf_added(b)
                    } else {
                        rf_new[&b].changes_new_line(b as u32)
                    }
                }) || o.is_some_and(rf_removed)
            }
            (Some(a), Some(b)) => ok[a] != nk[b],
            _ => true,
        };
        // Reformatted lines count as added or removed where they have
        // added or removed tokens, inline and side by side alike.
        let added = rows
            .iter()
            .filter(|r| r.1.is_some() && (r.0.is_none() || split) && changed_row(r) && !rf_row(r))
            .count()
            + rf_new.keys().filter(|&&b| rf_added(b)).count();
        let removed = rows
            .iter()
            .filter(|r| r.0.is_some() && (r.1.is_none() || split) && changed_row(r) && !rf_row(r))
            .count()
            + rf_old.keys().filter(|&&a| rf_removed(a)).count();
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
                    r#"<a class="ref" data-id="{}" data-kind="{}" data-path="{}"{}>{}</a>, line {}"#,
                    esc(&o.info.id),
                    o.info.kind.css(),
                    esc(&o.info.id),
                    crate::nav::item_href(&o.info.id),
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
        // Whole lines of reformatted groups that were added or removed.
        let rf_adds = |b: usize| {
            rf_new
                .get(&b)
                .is_some_and(|g| g.adds_line(b as u32, new[b]))
        };
        let rf_removes = |a: usize| {
            rf_old
                .get(&a)
                .is_some_and(|g| g.removes_line(a as u32, old[a]))
        };
        let new_code = |n: usize| {
            if let Some(html) = marked_new.get(&n) {
                return html.clone();
            }
            let html = code_html(
                self,
                new[n],
                Links::Source {
                    line: new_start + n,
                    refs: &it.info.source_refs,
                    of: &it.info.id,
                },
            );
            // The added tokens of a reformatted line, and inline, the
            // removed ones where they were; the same for the segments of
            // paths that name the same item, marked as no change.
            let Some(g) = rf_new.get(&n) else {
                return html;
            };
            let on_line = |spans: &[model::TokenSpan]| -> Vec<(usize, usize)> {
                spans
                    .iter()
                    .filter(|t| t.line as usize == n)
                    .map(|t| (t.start as usize, t.end as usize))
                    .collect()
            };
            /// Inline, the removed tokens shown on line `n`.
            fn at_line(
                removed: &[model::RemovedTokens],
                n: usize,
                split: bool,
            ) -> Vec<(usize, &str)> {
                if split {
                    return Vec::new();
                }
                removed
                    .iter()
                    .filter(|t| t.at.line as usize == n)
                    .map(|t| (t.at.start as usize, t.text.as_str()))
                    .collect()
            }
            let changed = !g.only_reformatted();
            let added = if changed && !rf_adds(n) {
                on_line(&g.added)
            } else {
                Vec::new()
            };
            let removed = if changed {
                at_line(&g.removed, n, split)
            } else {
                Vec::new()
            };
            let re_added = on_line(&g.requalified_added);
            let re_removed = at_line(&g.requalified_removed, n, split);
            if added.is_empty()
                && removed.is_empty()
                && re_added.is_empty()
                && re_removed.is_empty()
            {
                return html;
            }
            crate::code::mark_spans(&html, &added, &removed, &re_added, &re_removed)
        };
        let old_code = |o: usize| {
            if let Some(html) = marked_old.get(&o) {
                return html.clone();
            }
            let html = code_html(self, old[o], old_links(o));
            let Some(g) = rf_old.get(&o) else {
                return html;
            };
            let on_line = |removed: &[model::RemovedTokens]| -> Vec<(usize, usize)> {
                removed
                    .iter()
                    .filter(|t| t.old.line as usize == o)
                    .map(|t| (t.old.start as usize, t.old.end as usize))
                    .collect()
            };
            let removed = if !g.only_reformatted() && !rf_removes(o) {
                on_line(&g.removed)
            } else {
                Vec::new()
            };
            let unqualified = on_line(&g.requalified_removed);
            if removed.is_empty() && unqualified.is_empty() {
                return html;
            }
            crate::code::mark_removed_spans(&html, &removed, &unqualified)
        };
        let nonempty = |s: String| if s.is_empty() { " ".to_string() } else { s };
        // Lines inside `unsafe` code that changed get a stripe, and the
        // first one says what it is.
        let uns = |old: bool, no: usize| -> (&'static str, String) {
            if !crate::unsafety::marks_line(&it.unsafe_code, old, no) {
                return ("", String::new());
            }
            (" uns", crate::unsafety::note_at(&it.unsafe_code, old, no))
        };
        // The PR's review threads under the rows of their lines.
        let threads = self.item_threads(&it.info.id);
        let threads_at = |r: usize| -> Vec<&model::ReviewThread> {
            let (o, n) = rows[r];
            threads
                .iter()
                .copied()
                .filter(|t| {
                    let at = if t.left {
                        o.map(|a| old_start + a)
                    } else {
                        n.map(|b| new_start + b)
                    };
                    at.is_some() && t.line == at
                })
                .collect()
        };
        let row_line_html = |r: usize| -> String {
            let (o, n) = rows[r];
            if split {
                let same = !changed_row(&(o, n));
                let rf = rf_row(&(o, n));
                let cell = |idx: Option<usize>,
                            start: usize,
                            class: &str,
                            side: &str,
                            code: &dyn Fn(usize) -> String| match idx {
                    Some(i) => {
                        let tag = if class == "del" { '-' } else { '+' };
                        let mv = !same && moved(tag, i).is_some();
                        let (un, un_note) = uns(side == "o", start + i);
                        format!(
                            r#"<span class="no {side}">{}</span><span class="ln {}{}{un}" data-t="{}">{un_note}{}{}</span>"#,
                            start + i,
                            if rf && class == "add" && rf_adds(i) {
                                "add rfa"
                            } else if rf && class == "del" && rf_removes(i) {
                                "del rfa"
                            } else if rf {
                                "rf"
                            } else if same {
                                ""
                            } else {
                                class
                            },
                            if mv { " mv" } else { "" },
                            if same || rf && !changed_row(&(o, n)) {
                                String::new()
                            } else {
                                tag.to_string()
                            },
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
                    // A reformatted line: wholly added, with some tokens
                    // added or removed (`~`), or only reformatted.
                    (None, Some(b)) if rf_adds(b) => {
                        ("add rfa", '+', b, new_start + b, new_code(b))
                    }
                    (None, Some(b))
                        if rf_new.get(&b).is_some_and(|g| g.changes_new_line(b as u32)) =>
                    {
                        ("rf", '~', b, new_start + b, new_code(b))
                    }
                    (None, Some(b)) if rf_new.contains_key(&b) => {
                        ("rf", ' ', b, new_start + b, new_code(b))
                    }
                    (None, Some(b)) => ("add", '+', b, new_start + b, new_code(b)),
                    (Some(a), _) => ("del", '-', a, old_start + a, old_code(a)),
                    (None, None) => unreachable!(),
                };
                let mv =
                    matches!(tag, '+' | '-') && class != "add rfa" && moved(tag, idx).is_some();
                let (un, un_note) = uns(tag == '-', no);
                format!(
                    r#"<span class="ln {class}{}{un}" data-t="{}" data-no="{no}">{}{un_note}{}{}</span>"#,
                    if mv { " mv" } else { "" },
                    if tag == ' ' {
                        String::new()
                    } else {
                        tag.to_string()
                    },
                    crate::code::gutter(o.map(|a| old_start + a), n.map(|b| new_start + b), tag),
                    if mv {
                        moved_note(tag, idx)
                    } else {
                        String::new()
                    },
                    nonempty(code)
                )
            }
        };
        // One row of the diff, with the threads on its line.
        let row_html = |r: usize| -> String {
            let list = threads_at(r);
            row_line_html(r) + &crate::threads::row_html(self, &list, split)
        };
        // Unchanged lines more than `CONTEXT` rows away from a change are
        // folded: a button in their place shows them. Runs of fewer than
        // `MIN_FOLD` lines are shown, since the button takes a line too.
        const CONTEXT: usize = 3;
        const MIN_FOLD: usize = 2;
        let mut gaps: Vec<std::ops::Range<usize>> = Vec::new();
        if rows.iter().any(changed_row) {
            // Lines with a review thread stay shown, like changed ones, and
            // so do all reformatted lines of a group with changed tokens:
            // the rest of the call or literal that changed.
            let in_changed_group = |(o, n): (Option<usize>, Option<usize>)| {
                o.and_then(|a| rf_old.get(&a))
                    .or_else(|| n.and_then(|b| rf_new.get(&b)))
                    .is_some_and(|g| !g.only_reformatted())
            };
            let near = |i: usize| {
                in_changed_group(rows[i])
                    || (i.saturating_sub(CONTEXT)..(i + CONTEXT + 1).min(rows.len()))
                        .any(|r| changed_row(&rows[r]) || !threads_at(r).is_empty())
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
        // controls of the hunks that start in it (see `review::hunks`), and
        // a part with controls is boxed together with them (`.hunk`), so
        // that the controls clearly belong to the lines below them.
        let part_gh = |part: std::ops::Range<usize>| -> (Vec<usize>, Vec<usize>, String) {
            let (mut olds, mut news) = (Vec::new(), Vec::new());
            for &(o, n) in rows[part.clone()].iter().filter(|r| changed_row(r)) {
                olds.extend(o.map(|a| old_start + a));
                news.extend(n.map(|b| new_start + b));
            }
            // A link to the part's first changed line on GitHub.
            let first_added = rows[part.clone()]
                .iter()
                .find(|r| changed_row(r))
                .is_some_and(|r| r.0.is_none());
            let hunk = crate::review::Hunk {
                id: String::new(),
                new: news.clone(),
                old: olds.clone(),
                starts_added: first_added,
            };
            let gh = crate::post::github_link(
                self,
                crate::post::hunk_loc(&self.out.files, it, &hunk),
                "gh-link",
                "GitHub ↗",
                "this change",
            );
            (olds, news, gh)
        };
        let part_html = |part: std::ops::Range<usize>| -> String {
            let (olds, news, gh) = part_gh(part.clone());
            let lines: String = part.map(&row_html).collect();
            match crate::review::hunk_controls(&it.info.id, &olds, &news) {
                c if c.is_empty() => lines,
                // In review mode the link goes into the hunk's bar.
                c => format!(
                    r#"<span class="hunk"><span class="hunk-rv">{c}{gh}</span>{lines}</span>"#
                ),
            }
        };
        // Outside review mode, a part's link is in the row of the folded
        // lines above it, not over its first line, where it would hide the
        // end of that line. The first part has none unless lines above it
        // are folded: the item's own link (in a card's header or an item's
        // row) leads to its first change.
        let mut parts = Vec::new();
        let mut r = 0;
        for g in &gaps {
            parts.push(r..g.start);
            r = g.end;
        }
        parts.push(r..rows.len());
        let mut folded = 0;
        let mut body = String::new();
        for (i, g) in gaps.iter().enumerate() {
            body += &part_html(parts[i].clone());
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
                let gh = match part_gh(parts[i + 1].clone()).2 {
                    gh if gh.is_empty() => gh,
                    gh => format!(r#"<span class="hunk-gh">{gh}</span>"#),
                };
                body += &format!(
                    r#"<span class="{}unfold" data-unfold="{}" title="Show these lines">{arrow} {} unchanged lines{gh}</span><span class="fold" hidden>{inner}</span>"#,
                    if split { "skip " } else { "ln skip " },
                    g.start,
                    g.len()
                );
            }
        }
        body += &part_html(parts[gaps.len()].clone());
        let code = format!(
            r#"<div class="src-code" data-item="{}">{}{}</div>"#,
            esc(&it.info.id),
            if split {
                format!(r#"<div class="sbs">{body}</div>"#)
            } else {
                format!(
                    "<pre {}>{body}</pre>",
                    crate::code::gutter_style((old_start + old.len()).max(new_start + new.len()))
                )
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
        // whose members follow it with their own source, and one with an
        // open review thread.
        let small = match it.status {
            Status::Changed => it.changes.body && (1..=SMALL_BODY).contains(&(added + removed)),
            Status::Added => it.info.kind != Kind::Impl && it.source.len() <= SMALL_BODY,
            _ => false,
        };
        let open = open.unwrap_or(small || threads.iter().any(|t| !t.resolved));
        // Changed lines that were moved, on either side.
        let moved_count = rows
            .iter()
            .filter(|&&(o, n)| changed_row(&(o, n)))
            .map(|&(o, n)| {
                usize::from(o.is_some_and(|a| moved('-', a).is_some()))
                    + usize::from(n.is_some_and(|b| moved('+', b).is_some()))
            })
            .sum::<usize>();
        // Names that refer to other items with the same text.
        let retargeted = it.retargeted.len();
        let stats = format!(
            r#"<span class="d-add">+{added}</span> <span class="d-del">−{removed}</span>{}{}{}"#,
            if moved_count > 0 {
                format!(r#" <span class="d-mv">({moved_count} moved)</span>"#)
            } else {
                String::new()
            },
            match retargeted {
                0 => String::new(),
                1 => r#" <span class="d-mv">(a name refers to another item)</span>"#.to_string(),
                n => format!(r#" <span class="d-mv">({n} names refer to other items)</span>"#),
            },
            // A formatting-only item says so below.
            match (opts.ignore_ws, !rf_new.is_empty() && !it.changes.cosmetic) {
                (true, true) => r#" <span class="dim">(whitespace and reformatting hidden)</span>"#,
                (true, false) => r#" <span class="dim">(whitespace hidden)</span>"#,
                (false, true) => r#" <span class="dim">(reformatting hidden)</span>"#,
                (false, false) => "",
            }
        );
        let where_ = format!(
            "{}:{}{}",
            esc(&it.info.file),
            it.info.line,
            crate::threads::summary_note(&threads)
        );
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
        // What each retargeted name referred to and does now, above the
        // code: the lines alone look unchanged.
        let retarget_notes: String = it
            .retargeted
            .iter()
            .map(|r| {
                let target = |t: &Option<model::Ref>| match t {
                    Some(t) => self.ref_link(t),
                    None => "nothing that was resolved".to_string(),
                };
                format!(
                    r#"<div class="rt-note">Line {}: <code>{}</code> now refers to {}, before to {}.</div>"#,
                    new_start + r.line as usize,
                    esc(&r.name),
                    target(&r.new),
                    target(&r.old)
                )
            })
            .collect();
        let html = format!(
            r#"<details class="src {}" {} {}><summary>{summary}</summary>{retarget_notes}{code}</details>"#,
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

/// Whether the item changed, and, while one commit is selected, whether
/// that commit changed it.
pub fn changed(it: &Item) -> bool {
    it.status != Status::Unchanged && crate::commit_filter::item_in(&it.info.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::PrMeta;

    fn data(pr: PrMeta) -> Data {
        Data::new(Output {
            meta: model::Meta {
                pr: Some(pr),
                ..Default::default()
            },
            crates: Vec::new(),
            items: Default::default(),
            files: Vec::new(),
            externals: Default::default(),
            warnings: Vec::new(),
            semver_checks: None,
        })
    }

    #[test]
    fn compared_links_the_commits_and_leaves_out_the_title() {
        let html = data(PrMeta {
            repo: "o/r".to_string(),
            number: 7,
            title: Some("Fix the parser".to_string()),
            base_ref: "main".to_string(),
            base_rev: "6a1039ba07".to_string(),
            head_ref: Some("fix-parser".to_string()),
            head_rev: "2704086310".to_string(),
            head_commit: "2704086310abcdef2704086310abcdef27040863".to_string(),
            ..Default::default()
        })
        .compared_html();
        assert!(!html.contains("Fix the parser"), "{html}");
        assert!(
            html.contains(r#"href="https://github.com/o/r/commit/6a1039ba07""#),
            "{html}"
        );
        // The head links the full commit, but shows it abbreviated.
        assert!(
            html.contains(
                r#"href="https://github.com/o/r/commit/2704086310abcdef2704086310abcdef27040863""#
            ),
            "{html}"
        );
        assert!(html.contains("<code>2704086310</code>"), "{html}");
        assert!(
            html.contains(r#"<code class="pm-ref">fix-parser</code>"#),
            "{html}"
        );
    }

    #[test]
    fn removed_items_sort_where_they_were() {
        let item = |id: &str, file: &str, line: usize, old_line: Option<usize>, status| Item {
            info: ItemInfo {
                id: id.to_string(),
                kind: Kind::Fn,
                module: "c".to_string(),
                file: file.to_string(),
                line,
                ..Default::default()
            },
            old_line,
            status,
            ..Default::default()
        };
        // Base: parse (1), validate (6), report (11). Head: read (1),
        // trim (6), parse (11), report (16). A line diff pairs the old
        // `parse` with `read` and `trim`, and would put `validate` before
        // the new `parse`; it was between `parse` and `report`.
        let lib = "src/lib.rs";
        let items = [
            item("c::read", lib, 1, None, Status::Added),
            item("c::trim", lib, 6, None, Status::Added),
            item("c::parse", lib, 11, Some(1), Status::Changed),
            item("c::validate", lib, 6, None, Status::Removed),
            item("c::report", lib, 16, Some(11), Status::Changed),
            // In a deleted file: by their old lines.
            item("c::z", "src/old.rs", 1, None, Status::Removed),
            item("c::y", "src/old.rs", 5, None, Status::Removed),
        ];
        let d = Data::new(Output {
            items: items
                .iter()
                .map(|it| (it.info.id.clone(), it.clone()))
                .collect(),
            ..data(PrMeta::default()).out
        });
        let mut list: Vec<&Item> = items.iter().collect();
        list.sort_by(|a, b| d.reading_order(a, b));
        let ids: Vec<&str> = list.iter().map(|it| it.info.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "c::read",
                "c::trim",
                "c::parse",
                "c::validate",
                "c::report",
                "c::z",
                "c::y"
            ]
        );
    }
}
