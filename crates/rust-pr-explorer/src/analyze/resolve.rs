//! Name resolution: paths, imports, the prelude and visibility.

use super::{
    Analyzer, Ns, Res,
    syntax::{path_segments, within},
};
use model::Kind;
use std::collections::BTreeSet;
use syn::Visibility;

/// Names of the standard prelude: name, path, name space, and the first
/// edition that has them.
const PRELUDE: &[(&str, &str, Ns, u16)] = &[
    ("Option", "core::option::Option", Ns::Type, 2015),
    ("Some", "core::option::Option::Some", Ns::Value, 2015),
    ("None", "core::option::Option::None", Ns::Value, 2015),
    ("Result", "core::result::Result", Ns::Type, 2015),
    ("Ok", "core::result::Result::Ok", Ns::Value, 2015),
    ("Err", "core::result::Result::Err", Ns::Value, 2015),
    ("String", "alloc::string::String", Ns::Type, 2015),
    ("ToString", "alloc::string::ToString", Ns::Type, 2015),
    ("Vec", "alloc::vec::Vec", Ns::Type, 2015),
    ("Box", "alloc::boxed::Box", Ns::Type, 2015),
    ("Clone", "core::clone::Clone", Ns::Type, 2015),
    ("Copy", "core::marker::Copy", Ns::Type, 2015),
    ("Send", "core::marker::Send", Ns::Type, 2015),
    ("Sync", "core::marker::Sync", Ns::Type, 2015),
    ("Sized", "core::marker::Sized", Ns::Type, 2015),
    ("Unpin", "core::marker::Unpin", Ns::Type, 2015),
    ("Default", "core::default::Default", Ns::Type, 2015),
    ("Drop", "core::ops::Drop", Ns::Type, 2015),
    ("Fn", "core::ops::Fn", Ns::Type, 2015),
    ("FnMut", "core::ops::FnMut", Ns::Type, 2015),
    ("FnOnce", "core::ops::FnOnce", Ns::Type, 2015),
    ("From", "core::convert::From", Ns::Type, 2015),
    ("Into", "core::convert::Into", Ns::Type, 2015),
    ("AsRef", "core::convert::AsRef", Ns::Type, 2015),
    ("AsMut", "core::convert::AsMut", Ns::Type, 2015),
    ("Iterator", "core::iter::Iterator", Ns::Type, 2015),
    ("IntoIterator", "core::iter::IntoIterator", Ns::Type, 2015),
    (
        "DoubleEndedIterator",
        "core::iter::DoubleEndedIterator",
        Ns::Type,
        2015,
    ),
    (
        "ExactSizeIterator",
        "core::iter::ExactSizeIterator",
        Ns::Type,
        2015,
    ),
    ("Extend", "core::iter::Extend", Ns::Type, 2015),
    ("PartialEq", "core::cmp::PartialEq", Ns::Type, 2015),
    ("Eq", "core::cmp::Eq", Ns::Type, 2015),
    ("PartialOrd", "core::cmp::PartialOrd", Ns::Type, 2015),
    ("Ord", "core::cmp::Ord", Ns::Type, 2015),
    ("ToOwned", "alloc::borrow::ToOwned", Ns::Type, 2015),
    ("TryFrom", "core::convert::TryFrom", Ns::Type, 2021),
    ("TryInto", "core::convert::TryInto", Ns::Type, 2021),
    ("FromIterator", "core::iter::FromIterator", Ns::Type, 2021),
];

/// Crates every crate can name without declaring them.
const BUILTIN_CRATES: &[&str] = &["std", "core", "alloc"];

/// Whether an item of `kind` (with `value_struct` for tuple and unit
/// structs) lives in name space `ns`. `None` accepts any.
fn in_ns(kind: Kind, value_struct: bool, ns: Option<Ns>) -> bool {
    match ns {
        None => true,
        Some(Ns::Type) => matches!(
            kind,
            Kind::Mod | Kind::Struct | Kind::Enum | Kind::Union | Kind::Trait | Kind::TypeAlias
        ),
        Some(Ns::Value) => {
            matches!(kind, Kind::Fn | Kind::Const | Kind::Static)
                || (kind == Kind::Struct && value_struct)
        }
    }
}

impl Analyzer {
    pub(super) fn crate_of(id: &str) -> &str {
        id.split("::").next().unwrap()
    }

    fn deps_of(&self, module: &str) -> Option<&BTreeSet<String>> {
        self.crate_info.get(Self::crate_of(module)).map(|(d, _)| d)
    }

    /// A crate name usable in paths of `module`'s crate other than the
    /// analyzed crates: a declared dependency or `std`/`core`/`alloc`.
    fn is_extern_crate(&self, module: &str, name: &str) -> bool {
        BUILTIN_CRATES.contains(&name) || self.deps_of(module).is_some_and(|d| d.contains(name))
    }

    /// Resolves a path as written in `module`. `ns` is the name space of the
    /// last segment, if the context tells (a type or an expression).
    /// Returns `None` whenever the target is not certain.
    pub(super) fn resolve(
        &self,
        module: &str,
        segs: &[String],
        self_ty: Option<&str>,
        ns: Option<Ns>,
    ) -> Option<Res> {
        self.resolve_depth(module, segs, self_ty, ns, 0)
    }

    fn resolve_depth(
        &self,
        module: &str,
        segs: &[String],
        self_ty: Option<&str>,
        ns: Option<Ns>,
        depth: usize,
    ) -> Option<Res> {
        if depth > 12 || segs.is_empty() {
            return None;
        }
        let first = segs[0].as_str();
        let first_ns = if segs.len() == 1 { ns } else { Some(Ns::Type) };
        let mut idx = 1;
        let mut cur: Res = match first {
            "crate" => Res {
                id: Some(Self::crate_of(module).to_string()),
                path: Self::crate_of(module).to_string(),
                display_start: 1,
            },
            "self" => Res {
                id: Some(module.to_string()),
                path: module.to_string(),
                display_start: 1,
            },
            "super" => {
                let mut m = module.to_string();
                let mut i = 0;
                while segs.get(i).map(String::as_str) == Some("super") {
                    m = self.modules.get(&m)?.parent.clone()?;
                    i += 1;
                }
                idx = i;
                Res {
                    id: Some(m.clone()),
                    path: m,
                    display_start: i,
                }
            }
            "Self" => {
                let id = self_ty?.to_string();
                Res {
                    id: Some(id.clone()),
                    path: id,
                    display_start: 0,
                }
            }
            _ => match self.lookup_name(module, first, first_ns, depth) {
                Some(r) => r,
                None if self.has_opaque_glob(module, depth + 1) => {
                    // A glob import we can't see into might bring this name.
                    return None;
                }
                None if first_ns != Some(Ns::Value) && self.crate_roots.contains(first) => Res {
                    id: Some(first.to_string()),
                    path: first.to_string(),
                    display_start: 1,
                },
                None if segs.len() > 1 && self.is_extern_crate(module, first) => Res {
                    id: None,
                    path: first.to_string(),
                    display_start: 0,
                },
                None => {
                    let edition = self
                        .crate_info
                        .get(Self::crate_of(module))
                        .and_then(|(_, e)| *e);
                    let (_, path, _, _) = PRELUDE.iter().find(|(n, _, pns, since)| {
                        *n == first
                            && first_ns.is_none_or(|ns| ns == *pns)
                            // Unknown edition: only names in every prelude.
                            && edition.unwrap_or(2015) >= *since
                    })?;
                    Res {
                        id: None,
                        path: path.to_string(),
                        display_start: 0,
                    }
                }
            },
        };
        let mut reached_type = None;
        while idx < segs.len() {
            let seg = &segs[idx];
            let last = idx == segs.len() - 1;
            match cur.id.as_deref() {
                Some(id) if self.kinds.get(id) == Some(&Kind::Mod) => {
                    let seg_ns = if last { ns } else { Some(Ns::Type) };
                    let r = self.lookup_name(id, seg, seg_ns, depth + 1)?;
                    cur = Res {
                        display_start: idx,
                        ..r
                    };
                }
                Some(id) if self.kinds.get(id).is_some_and(|k| k.is_type_like()) => {
                    reached_type.get_or_insert(idx - 1);
                    let candidates: Vec<&String> = self
                        .assoc
                        .get(id)
                        .into_iter()
                        .flatten()
                        .filter(|(n, _)| n == seg)
                        .map(|(_, m)| m)
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    if candidates.len() > 1 {
                        // Several impls provide the name (`From<u8>` and
                        // `From<String>`): which one needs types.
                        return None;
                    }
                    if let Some(member) = candidates.first().copied() {
                        cur = Res {
                            id: Some(member.clone()),
                            path: member.clone(),
                            display_start: 0,
                        };
                    } else if self
                        .enum_variants
                        .get(id)
                        .is_some_and(|v| v.iter().any(|v| v == seg))
                    {
                        // Variants refer to their enum.
                        cur.path = format!("{id}::{seg}");
                    } else {
                        // An associated item we don't see (from a derive or a
                        // trait impl elsewhere): it is still the type's.
                        cur.path = format!("{id}::{seg}");
                    }
                }
                Some(_) => return None,
                None => {
                    cur.path = format!("{}::{seg}", cur.path);
                }
            }
            idx += 1;
        }
        cur.display_start = match (&cur.id, reached_type) {
            (_, Some(t)) => t,
            (Some(_), None) => segs.len() - 1,
            (None, None) => {
                // External path: keep from the first capitalized segment.
                segs.iter()
                    .position(|s| s.starts_with(|c: char| c.is_uppercase()))
                    .unwrap_or(segs.len() - 1)
            }
        };
        if segs[0] == "Self" {
            cur.display_start = 0;
        }
        Some(cur)
    }

    /// Looks a name up in a module: its definitions, then its imports, then
    /// its glob imports (only what they make visible).
    /// Looks a name up in a module, memoized: glob imports that import
    /// each other (`use super::*` in every child of a `pub use child::*`
    /// root) would otherwise make this exponential. A lookup already in
    /// progress further up (a cycle) finds nothing on this path.
    fn lookup_name(&self, module: &str, name: &str, ns: Option<Ns>, depth: usize) -> Option<Res> {
        let key = (module.to_string(), name.to_string(), ns);
        if let Some(r) = self.lookup_memo.borrow().get(&key) {
            return r.clone();
        }
        if depth > 12 || !self.lookup_active.borrow_mut().insert(key.clone()) {
            self.lookup_cut.set(true);
            return None;
        }
        let outer_cut = self.lookup_cut.replace(false);
        let r = self.lookup_name_uncached(module, name, ns, depth);
        let cut = self.lookup_cut.get();
        self.lookup_cut.set(outer_cut || cut);
        self.lookup_active.borrow_mut().remove(&key);
        // A "not found" reached through a cut cycle may hold only for this
        // path; anything found holds everywhere.
        if r.is_some() || !cut {
            self.lookup_memo.borrow_mut().insert(key, r.clone());
        }
        r
    }

    fn lookup_name_uncached(
        &self,
        module: &str,
        name: &str,
        ns: Option<Ns>,
        depth: usize,
    ) -> Option<Res> {
        let m = self.modules.get(module)?;
        if let Some(ids) = m.defs.get(name)
            && let Some(id) = ids.iter().find(|id| {
                self.kinds
                    .get(*id)
                    .is_some_and(|k| in_ns(*k, self.value_structs.contains(*id), ns))
            })
        {
            return Some(Res {
                path: id.clone(),
                id: Some(id.clone()),
                display_start: 0,
            });
        }
        for u in &m.uses {
            if u.alias.as_deref() == Some(name) {
                if u.path.len() == 1 && u.path[0] == name {
                    // `use foo;` / `extern crate foo;`
                    if self.crate_roots.contains(name) {
                        return Some(Res {
                            id: Some(name.to_string()),
                            path: name.to_string(),
                            display_start: 0,
                        });
                    }
                    if self.is_extern_crate(module, name) {
                        return Some(Res {
                            id: None,
                            path: name.to_string(),
                            display_start: 0,
                        });
                    }
                    return None;
                }
                if let Some(r) = self.resolve_depth(module, &u.path, None, ns, depth + 1) {
                    return Some(r);
                }
            }
        }
        for u in &m.uses {
            if u.alias.is_some() {
                continue;
            }
            let Some(Res {
                id: Some(target), ..
            }) = self.resolve_depth(module, &u.path, None, Some(Ns::Type), depth + 1)
            else {
                continue;
            };
            if target == module {
                continue;
            }
            if self.kinds.get(&target) == Some(&Kind::Enum) {
                if ns != Some(Ns::Type)
                    && self
                        .enum_variants
                        .get(&target)
                        .is_some_and(|v| v.iter().any(|v| v == name))
                {
                    return Some(Res {
                        id: Some(target.clone()),
                        path: format!("{target}::{name}"),
                        display_start: 0,
                    });
                }
                continue;
            }
            if let Some(r) = self.lookup_name(&target, name, ns, depth + 1) {
                // A glob only imports what is visible to the importing module.
                let visible = r.id.as_ref().is_none_or(|id| {
                    self.def_vis
                        .get(id)
                        .is_none_or(|scope| scope.as_ref().is_none_or(|s| within(module, s)))
                });
                if visible {
                    return Some(r);
                }
            }
        }
        None
    }

    /// Whether a module has glob imports of something whose contents we
    /// can't see (another crate, or a path we can't resolve).
    fn has_opaque_glob(&self, module: &str, depth: usize) -> bool {
        if depth > 8 {
            return true;
        }
        let Some(m) = self.modules.get(module) else {
            return false;
        };
        m.uses.iter().filter(|u| u.alias.is_none()).any(|u| {
            match self.resolve_depth(module, &u.path, None, Some(Ns::Type), depth + 1) {
                Some(Res {
                    id: Some(target), ..
                }) => {
                    target != module
                        && self.kinds.get(&target) == Some(&Kind::Mod)
                        && self.has_opaque_glob(&target, depth + 1)
                }
                _ => true,
            }
        })
    }

    pub(super) fn vis_scope(&self, module: &str, vis: &Visibility) -> (Option<String>, String) {
        match vis {
            Visibility::Public(_) => (None, "pub".into()),
            Visibility::Inherited => (Some(module.to_string()), "private".into()),
            Visibility::Restricted(r) => {
                let segs = path_segments(&r.path);
                let label = format!(
                    "pub({}{})",
                    if r.in_token.is_some() { "in " } else { "" },
                    segs.join("::")
                );
                let scope = match self.resolve(module, &segs, None, Some(Ns::Type)) {
                    Some(Res { id: Some(id), .. }) => id,
                    _ => Self::crate_of(module).to_string(),
                };
                (Some(scope), label)
            }
        }
    }
}
