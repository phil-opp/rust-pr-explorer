//! The public API: which items are reachable, and by which paths.

use super::{Analyzer, ItemData, Ns, Res, syntax::is_extra_target};
use model::Kind;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// A `pub use`: (module, target, name it gets there).
type Reexport = (String, String, Option<String>);

impl Analyzer {
    /// The `pub use`s of all modules, including those a glob re-export
    /// forwards.
    pub(super) fn reexports(&self) -> Vec<Reexport> {
        let mut reexports: Vec<Reexport> = Vec::new();
        // `pub use m::*`: (module, m).
        let mut globs: Vec<(String, String)> = Vec::new();
        for (module, data) in &self.modules {
            for u in &data.uses {
                // `pub use path::Item;` and `pub use path::*;`
                if !u.public {
                    continue;
                }
                let ns = if u.alias.is_some() {
                    None
                } else {
                    Some(Ns::Type)
                };
                if let Some(Res { id: Some(id), .. }) = self.resolve(module, &u.path, None, ns) {
                    if let Some(alias) = &u.alias {
                        reexports.push((module.clone(), id, Some(alias.clone())));
                    } else if self.kinds.get(&id) == Some(&Kind::Mod) {
                        for child in self.children.get(&id).into_iter().flatten() {
                            if self.def_vis.get(child).is_some_and(|v| v.is_none()) {
                                reexports.push((module.clone(), child.clone(), None));
                            }
                        }
                        globs.push((module.clone(), id));
                    }
                }
            }
        }
        // A glob re-export also forwards the module's own `pub use`s (the
        // facade pattern), through chains of globs.
        for _ in 0..16 {
            let mut added = false;
            for (m, t) in &globs {
                let forwarded: Vec<Reexport> = reexports
                    .iter()
                    .filter(|(rm, _, _)| rm == t)
                    .map(|(_, target, name)| (m.clone(), target.clone(), name.clone()))
                    .collect();
                for r in forwarded {
                    if !reexports.contains(&r) {
                        reexports.push(r);
                        added = true;
                    }
                }
            }
            if !added {
                break;
            }
        }
        reexports
    }

    /// Marks the items reachable from outside their crate, and records
    /// the paths by which they can be named.
    pub(super) fn mark_public_api(
        &self,
        items: &mut BTreeMap<String, ItemData>,
        reexports: &[Reexport],
    ) {
        // Public reachability.
        let ids: Vec<String> = items.keys().cloned().collect();
        let mut memo: HashMap<String, bool> = HashMap::new();
        for id in &ids {
            let public = reachable(id, items, &mut memo);
            items.get_mut(id).unwrap().info.public = public;
        }
        // Re-exports from public modules make their targets public, and
        // with them everything public inside; repeat until nothing changes,
        // since a module can itself become public through a re-export.
        fn mark_public(id: &str, items: &mut BTreeMap<String, ItemData>) {
            let Some(item) = items.get_mut(id) else {
                return;
            };
            if item.info.public {
                return;
            }
            item.info.public = true;
            for child in item.info.children.clone() {
                if items
                    .get(&child)
                    .is_some_and(|c| c.info.vis_scope.is_none())
                {
                    mark_public(&child, items);
                }
            }
        }
        // Public paths: crate roots, then every `pub` item of a module
        // with a public path, and every `pub use` in such a module, until
        // nothing changes (a module can become public through a re-export).
        let mut paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let roots: Vec<String> = self
            .crate_info
            .keys()
            .filter(|k| !is_extra_target(k))
            .cloned()
            .collect();
        for root in &roots {
            paths.entry(root.clone()).or_default().insert(root.clone());
        }
        // Long paths only arise from re-export cycles (`pub use self::*`).
        const MAX_SEGMENTS: usize = 12;
        for _ in 0..64 {
            let mut new: Vec<(String, String)> = Vec::new();
            for (id, item) in items.iter() {
                let i = &item.info;
                // Impl blocks have no path of their own.
                if i.parent.is_some()
                    || i.id == i.module
                    || i.vis_scope.is_some()
                    || i.kind == Kind::Impl
                {
                    continue;
                }
                for p in paths.get(&i.module).into_iter().flatten() {
                    new.push((id.clone(), format!("{p}::{}", i.name)));
                }
            }
            for (module, target, name) in reexports {
                let Some(name) = name
                    .clone()
                    .or_else(|| items.get(target).map(|t| t.info.name.clone()))
                else {
                    continue;
                };
                for p in paths.get(module).into_iter().flatten() {
                    new.push((target.clone(), format!("{p}::{name}")));
                }
            }
            let mut changed = false;
            for (id, p) in new {
                if p.split("::").count() <= MAX_SEGMENTS && items.contains_key(&id) {
                    changed |= paths.entry(id).or_default().insert(p);
                }
            }
            if !changed {
                break;
            }
        }
        for (id, set) in paths {
            let mut list: Vec<String> = set.into_iter().collect();
            list.sort_by_key(|p| (p.split("::").count(), p.clone()));
            mark_public(&id, items);
            if let Some(item) = items.get_mut(&id) {
                item.info.public_paths = list;
            }
        }
    }
}

fn reachable(
    id: &str,
    items: &BTreeMap<String, ItemData>,
    memo: &mut HashMap<String, bool>,
) -> bool {
    if let Some(&r) = memo.get(id) {
        return r;
    }
    memo.insert(id.to_string(), false);
    let r = match items.get(id) {
        None => true,
        Some(item) => {
            item.info.vis_scope.is_none()
                && match &item.info.vis_parent {
                    None => true,
                    Some(parent) => reachable(parent, items, memo),
                }
        }
    };
    memo.insert(id.to_string(), r);
    r
}
