//! The embedded analysis plus derived indexes and privacy rules.

use crate::code::esc;
use model::{Item, Kind, Output, Status, UseCtx};
use std::collections::HashMap;

pub struct Data {
    pub out: Output,
    /// Callers of each item: (caller id, guessed?).
    pub callers: HashMap<String, Vec<(String, bool)>>,
    /// Users of each item: (user id, context).
    pub used_by: HashMap<String, Vec<(String, UseCtx)>>,
    /// Items listed directly in each module (no associated items).
    pub mod_children: HashMap<String, Vec<String>>,
    /// Whether an item or anything it contains changed.
    touched: HashMap<String, bool>,
}

/// `m` is `s` or nested inside it.
pub fn within(m: &str, s: &str) -> bool {
    m == s || (m.len() > s.len() + 2 && m.starts_with(s) && m[s.len()..].starts_with("::"))
}

impl Data {
    pub fn new(out: Output) -> Data {
        let mut callers: HashMap<String, Vec<(String, bool)>> = HashMap::new();
        let mut used_by: HashMap<String, Vec<(String, UseCtx)>> = HashMap::new();
        let mut mod_children: HashMap<String, Vec<String>> = HashMap::new();
        for item in out.items.values() {
            let i = &item.info;
            for c in &i.calls {
                callers
                    .entry(c.to.clone())
                    .or_default()
                    .push((i.id.clone(), c.inferred));
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
        let mut d = Data {
            out,
            callers,
            used_by,
            mod_children,
            touched: HashMap::new(),
        };
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

    pub fn item(&self, id: &str) -> Option<&Item> {
        self.out.items.get(id)
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.out.items.values()
    }

    pub fn roots(&self) -> &[String] {
        &self.out.crates
    }

    pub fn main_root(&self) -> &str {
        &self.out.crates[0]
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

    pub fn touched(&self, it: &Item) -> bool {
        self.touched.get(&it.info.id).copied().unwrap_or(false)
    }

    pub fn is_root(&self, it: &Item) -> bool {
        it.info.kind == Kind::Mod && it.info.module == it.info.id
    }

    /// The module an item lives in (a module's own id for modules).
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
        if self.out.crates.len() != 1 {
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
        if matches!(i.kind, Kind::Mod | Kind::Impl) {
            return i.name.clone();
        }
        if let Some(parent) = i.parent.as_deref().and_then(|p| self.item(p)) {
            if parent.info.kind == Kind::Impl {
                let self_name = parent
                    .info
                    .self_ty
                    .as_deref()
                    .and_then(|t| self.item(t))
                    .map_or("Self".to_string(), |t| t.info.name.clone());
                let tr = parent
                    .info
                    .trait_ref
                    .as_ref()
                    .map(|t| t.path.rsplit("::").next().unwrap_or("").to_string())
                    .unwrap_or_default();
                return format!("<{self_name} as {tr}>::{}", i.name);
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
            Some(it) if self.is_root(it) => format!("crate {}", it.info.name),
            Some(_) => format!("mod {}", self.short_path(m)),
            None => m.to_string(),
        }
    }

    pub fn title_html(&self, it: &Item, with_module: bool) -> String {
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
                Status::Added => r#"<span class="st st-added">+ new module</span>"#.into(),
                Status::Removed => r#"<span class="st st-removed">− removed</span>"#.into(),
                _ => {
                    let n = self.changes_in(&it.info.id, true).len();
                    if n > 0 {
                        format!(r#"<span class="st st-inner">{n} changed</span>"#)
                    } else {
                        String::new()
                    }
                }
            };
        }
        let c = &it.changes;
        match it.status {
            Status::Added => r#"<span class="st st-added">+ added</span>"#.into(),
            Status::Removed => r#"<span class="st st-removed">− removed</span>"#.into(),
            Status::Changed => {
                let what: Vec<&str> = [
                    (c.signature, "signature"),
                    (c.body, "body"),
                    (c.docs, "docs"),
                ]
                .iter()
                .filter(|(on, _)| *on)
                .map(|(_, w)| *w)
                .collect();
                let mut s = format!(
                    r#"<span class="st st-changed">~ {}</span>"#,
                    if what.is_empty() {
                        "changed".to_string()
                    } else {
                        what.join(", ")
                    }
                );
                if let Some(from) = &c.moved_from {
                    let from_mod = from.rsplit_once("::").map_or(from.as_str(), |(m, _)| m);
                    s.push_str(&format!(
                        r#" <span class="st st-changed" title="{}">moved from {}</span>"#,
                        esc(from),
                        esc(self.short_path(from_mod))
                    ));
                }
                s
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

    pub fn vis_html(&self, it: &Item) -> String {
        let i = &it.info;
        let in_trait_impl = i.kind.is_member()
            && i.parent
                .as_deref()
                .and_then(|p| self.item(p))
                .is_some_and(|p| p.info.kind == Kind::Impl);
        if in_trait_impl || i.kind == Kind::TraitMethod {
            return String::new();
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
            r#"<span class="{class}" title="{}">{}</span>"#,
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
