//! Compares the base and head analyses item by item.

use crate::analyze::{Analysis, ItemData};
use crate::semver;
use model::{Changes, Item, Kind, Meta, Output, SigDiff, SourceLine, Status};
use similar::{ChangeTag, TextDiff};
use std::collections::{BTreeMap, HashMap, HashSet};

pub fn merge(base: Analysis, head: Analysis, meta: Meta) -> Output {
    let mut base_items = base.items;
    let mut head_items = head.items;
    let mut items = BTreeMap::new();

    // Items that only changed location count as moved, not removed + added.
    let moves = find_moves(&base_items, &head_items);
    let base_public: HashSet<String> = base_items
        .iter()
        .filter(|(_, b)| b.info.public)
        .map(|(id, _)| id.clone())
        .collect();
    let ctx = semver::Context {
        base_public: &base_public,
    };

    for (id, h) in std::mem::take(&mut head_items) {
        let b = match moves.get(&id) {
            Some(old) => base_items.remove(old),
            None => base_items.remove(&id),
        };
        let item = match b {
            Some(b) => compare(&ctx, b, h, moves.get(&id).cloned()),
            None => Item {
                api: semver::classify(&ctx, None, Some(&h), &[], &[], None),
                unsafe_code: crate::unsafety::compare(&[], &h.unsafe_sites),
                source: lines(&h.source, '+'),
                info: h.info,
                status: Status::Added,
                ..Default::default()
            },
        };
        items.insert(id, item);
    }
    for (id, b) in base_items {
        items.insert(
            id,
            Item {
                api: semver::classify(&ctx, Some(&b), None, &[], &[], None),
                unsafe_code: crate::unsafety::compare(&b.unsafe_sites, &[]),
                source: lines(&b.source, '-'),
                old_line: Some(b.info.line),
                info: b.info,
                status: Status::Removed,
                ..Default::default()
            },
        );
    }

    crate::moved::mark(&mut items);

    let mut crates = head.crates;
    for c in base.crates {
        if !crates.contains(&c) {
            crates.push(c);
        }
    }
    let mut warnings: Vec<String> = base
        .warnings
        .into_iter()
        .map(|w| format!("base: {w}"))
        .collect();
    warnings.extend(head.warnings.into_iter().map(|w| format!("head: {w}")));
    // Externals are not part of the review; the head's view wins.
    let mut externals = base.externals;
    externals.extend(head.externals);
    Output {
        meta,
        crates,
        items,
        externals,
        files: Vec::new(),
        warnings,
        semver_checks: None,
    }
}

fn compare(ctx: &semver::Context, b: ItemData, h: ItemData, moved_from: Option<String>) -> Item {
    // Names of items that moved along point into the new module: compare
    // with the old module's paths rewritten, as `find_moves` does.
    let (om, nm) = (&b.info.module, &h.info.module);
    let same = |old: &str, new: &str| {
        old == new || (moved_from.is_some() && rewrite_module(old, om, nm) == new)
    };
    let signature = !same(&b.sig_norm, &h.sig_norm);
    let body = !same(&b.body_norm, &h.body_norm);
    let docs = b.info.docs != h.info.docs;
    // Public paths gained or lost, e.g. by adding or removing a `pub use`.
    let exports_added: Vec<String> = h
        .info
        .public_paths
        .iter()
        .filter(|p| !b.info.public_paths.contains(p))
        .cloned()
        .collect();
    let exports_removed: Vec<String> = b
        .info
        .public_paths
        .iter()
        .filter(|p| !h.info.public_paths.contains(p))
        .cloned()
        .collect();
    let exports = !exports_added.is_empty() || !exports_removed.is_empty();
    let structural = signature || body || docs || exports;
    let cosmetic = !structural && b.source != h.source && h.info.kind != Kind::Mod;
    let status = if structural || moved_from.is_some() {
        Status::Changed
    } else {
        Status::Unchanged
    };
    let source = if b.source == h.source {
        lines(&h.source, ' ')
    } else {
        let diff = TextDiff::from_lines(&b.source, &h.source);
        diff.iter_all_changes()
            .map(|c| {
                let tag = match c.tag() {
                    ChangeTag::Equal => ' ',
                    ChangeTag::Insert => '+',
                    ChangeTag::Delete => '-',
                };
                SourceLine(tag, c.value().trim_end_matches('\n').to_string())
            })
            .collect()
    };
    // Structured diffs of the full and the public (private fields hidden)
    // signatures.
    // Computed whenever the signature's meaning changed, which includes
    // names that now refer to other items with the same text.
    let refs = (&b.info.sig_refs, &h.info.sig_refs);
    let sig_diff = signature
        .then(|| crate::sigdiff::diff(h.info.kind, &b.info.sig, &h.info.sig, refs))
        .flatten();
    let has_public = b.info.sig_public.is_some() || h.info.sig_public.is_some();
    let sig_diff_public = (signature && has_public)
        .then(|| {
            let old = b.info.sig_public.as_deref().unwrap_or(&b.info.sig);
            let new = h.info.sig_public.as_deref().unwrap_or(&h.info.sig);
            crate::sigdiff::diff(h.info.kind, old, new, refs)
        })
        .flatten();
    let (sig_diff, sig_diff_public) = {
        let docs = (member_docs(&b.source), member_docs(&h.source));
        let fill = |d: Option<SigDiff>| {
            d.map(|mut d| {
                for n in &mut d.notes {
                    n.docs = n
                        .member
                        .as_ref()
                        .and_then(|m| docs.1.get(m).or_else(|| docs.0.get(m)))
                        .cloned();
                }
                d
            })
        };
        (fill(sig_diff), fill(sig_diff_public))
    };
    let api = semver::classify(
        ctx,
        Some(&b),
        Some(&h),
        &exports_added,
        &exports_removed,
        sig_diff.as_ref(),
    );
    Item {
        api,
        unsafe_code: crate::unsafety::compare(&b.unsafe_sites, &h.unsafe_sites),
        sig_diff,
        sig_diff_public,
        old_sig_public: (b.info.sig != h.info.sig)
            .then(|| b.info.sig_public.clone())
            .flatten(),
        old_sig: (b.info.sig != h.info.sig).then(|| b.info.sig.clone()),
        old_docs: docs.then(|| b.info.docs.clone()),
        old_line: Some(b.info.line),
        old_file: (b.info.file != h.info.file).then(|| b.info.file.clone()),
        old_source_refs: if b.source != h.source {
            b.info.source_refs
        } else {
            BTreeMap::new()
        },
        reformatted: if b.source != h.source {
            let lines = |s: &str| s.lines().map(str::to_string).collect::<Vec<_>>();
            let (old, new) = (lines(&b.source), lines(&h.source));
            let old: Vec<&str> = old.iter().map(String::as_str).collect();
            let new: Vec<&str> = new.iter().map(String::as_str).collect();
            crate::reformat::find(&old, &new)
        } else {
            Vec::new()
        },
        source,
        moved_lines: Vec::new(),
        info: h.info,
        status,
        changes: Changes {
            signature,
            body,
            docs,
            cosmetic,
            moved_from,
            exports_added,
            exports_removed,
        },
    }
}

fn lines(text: &str, tag: char) -> Vec<SourceLine> {
    if text.is_empty() {
        return Vec::new();
    }
    text.lines()
        .map(|l| SourceLine(tag, l.to_string()))
        .collect()
}

/// Maps head ids to base ids of items that were moved to another module.
fn find_moves(
    base: &BTreeMap<String, ItemData>,
    head: &BTreeMap<String, ItemData>,
) -> HashMap<String, String> {
    fn key(item: &ItemData) -> (Kind, String, String) {
        let item = &item.info;
        let parent = item
            .parent
            .as_deref()
            .and_then(|p| p.rsplit("::").next())
            .unwrap_or("")
            .to_string();
        (item.kind, item.name.clone(), parent)
    }
    let mut removed: HashMap<_, Vec<&String>> = HashMap::new();
    for (id, item) in base {
        if !head.contains_key(id) && item.info.kind != Kind::Mod {
            removed.entry(key(item)).or_default().push(id);
        }
    }
    let mut added: HashMap<_, Vec<&String>> = HashMap::new();
    for (id, item) in head {
        if !base.contains_key(id) && item.info.kind != Kind::Mod {
            added.entry(key(item)).or_default().push(id);
        }
    }
    // The code compared: the body for items that have one (a function
    // whose visibility changed while moving still counts as the same), the
    // whole definition otherwise.
    fn code(item: &ItemData) -> &str {
        let has_body = item.info.kind.is_callable()
            || matches!(
                item.info.kind,
                Kind::Const | Kind::Static | Kind::AssocConst | Kind::Macro
            );
        if has_body {
            &item.body_norm
        } else {
            &item.sig_norm
        }
    }
    let mut moves = HashMap::new();
    for (k, new_ids) in added {
        if let [new_id] = new_ids.as_slice()
            && let Some([old_id]) = removed.get(&k).map(Vec::as_slice)
        {
            let (old, new) = (&base[*old_id], &head[*new_id]);
            // Names of items that moved along (siblings, `Self`) point into
            // the new module: compare with the old module's paths rewritten.
            let old_code = rewrite_module(code(old), &old.info.module, &new.info.module);
            if old_code == code(new) || similar_code(&old_code, code(new)) {
                moves.insert((*new_id).clone(), (*old_id).clone());
            }
        }
    }
    moves
}

/// Whether two normalized forms are close enough for an item that is the
/// only candidate on both sides to count as moved and edited, rather than
/// removed and added.
///
/// The forms are compared token by token. At least half of the tokens must
/// be the same, and at least [`MIN_SHARED_TOKENS`] of them, so that small
/// unrelated items with the same name (`fn f() -> u8 { 1 }` and
/// `fn f() -> u8 { 2 }`) are not paired.
fn similar_code(old: &str, new: &str) -> bool {
    let old: Vec<&str> = old.split_whitespace().collect();
    let new: Vec<&str> = new.split_whitespace().collect();
    let diff = TextDiff::configure()
        .timeout(std::time::Duration::from_millis(100))
        .diff_slices(&old, &new);
    let shared: usize = diff
        .ops()
        .iter()
        .filter(|op| op.tag() == similar::DiffTag::Equal)
        .map(|op| op.old_range().len())
        .sum();
    shared >= MIN_SHARED_TOKENS && 2 * shared >= old.len().max(new.len())
}

/// See [`similar_code`].
const MIN_SHARED_TOKENS: usize = 10;

/// `norm` (a normalized form, with full paths in quotes) with paths into
/// module `old` pointing into module `new` instead.
fn rewrite_module(norm: &str, old: &str, new: &str) -> String {
    if old == new {
        return norm.to_string();
    }
    norm.replace(&format!("\"{old}::"), &format!("\"{new}::"))
}

/// The doc comments of a struct's or union's fields and of an enum's
/// variants, by name (tuple fields by index), and of the variants' fields
/// as `Variant.field`. Only non-empty ones.
fn member_docs(source: &str) -> HashMap<String, String> {
    use crate::analyze::extract_docs;
    let mut out = HashMap::new();
    let fields = |fields: &syn::Fields, out: &mut HashMap<String, String>| {
        for (i, f) in fields.iter().enumerate() {
            let name = f.ident.as_ref().map_or(i.to_string(), |n| n.to_string());
            out.insert(name, extract_docs(&f.attrs));
        }
    };
    match syn::parse_str::<syn::Item>(source.trim()) {
        Ok(syn::Item::Struct(s)) => fields(&s.fields, &mut out),
        Ok(syn::Item::Union(u)) => fields(&syn::Fields::Named(u.fields), &mut out),
        Ok(syn::Item::Enum(e)) => {
            for v in &e.variants {
                let mut of_variant = HashMap::new();
                fields(&v.fields, &mut of_variant);
                for (field, docs) in of_variant {
                    out.insert(format!("{}.{field}", v.ident), docs);
                }
                out.insert(v.ident.to_string(), extract_docs(&v.attrs));
            }
        }
        _ => {}
    }
    out.retain(|_, d| !d.is_empty());
    out
}
