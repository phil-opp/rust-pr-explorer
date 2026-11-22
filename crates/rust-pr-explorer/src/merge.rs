//! Compares the base and head analyses item by item.

use crate::analyze::{Analysis, ItemData};
use model::{Changes, Item, Kind, Meta, Output, SourceLine, Status};
use similar::{ChangeTag, TextDiff};
use std::collections::{BTreeMap, HashMap};

pub fn merge(base: Analysis, head: Analysis, meta: Meta) -> Output {
    let mut base_items = base.items;
    let mut head_items = head.items;
    let mut items = BTreeMap::new();

    // Items that only changed location count as moved, not removed + added.
    let moves = find_moves(&base_items, &head_items);

    for (id, h) in std::mem::take(&mut head_items) {
        let b = match moves.get(&id) {
            Some(old) => base_items.remove(old),
            None => base_items.remove(&id),
        };
        let item = match b {
            Some(b) => compare(b, h, moves.get(&id).cloned()),
            None => Item {
                source: lines(&h.source, '+'),
                info: h.info,
                status: Status::Added,
                changes: Changes::default(),
                old_sig: None,
                old_sig_public: None,
                old_docs: None,
            },
        };
        items.insert(id, item);
    }
    for (id, b) in base_items {
        items.insert(
            id,
            Item {
                source: lines(&b.source, '-'),
                info: b.info,
                status: Status::Removed,
                changes: Changes::default(),
                old_sig: None,
                old_sig_public: None,
                old_docs: None,
            },
        );
    }

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
    Output {
        meta,
        crates,
        items,
        warnings,
    }
}

fn compare(b: ItemData, h: ItemData, moved_from: Option<String>) -> Item {
    let signature = b.sig_norm != h.sig_norm;
    let body = b.body_norm != h.body_norm;
    let docs = b.info.docs != h.info.docs;
    let structural = signature || body || docs;
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
    Item {
        old_sig_public: (b.info.sig != h.info.sig)
            .then(|| b.info.sig_public.clone())
            .flatten(),
        old_sig: (b.info.sig != h.info.sig).then(|| b.info.sig.clone()),
        old_docs: docs.then(|| b.info.docs.clone()),
        source,
        info: h.info,
        status,
        changes: Changes {
            signature,
            body,
            docs,
            cosmetic,
            moved_from,
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
    let mut moves = HashMap::new();
    for (k, new_ids) in added {
        if let [new_id] = new_ids.as_slice()
            && let Some([old_id]) = removed.get(&k).map(Vec::as_slice)
        {
            moves.insert((*new_id).clone(), (*old_id).clone());
        }
    }
    moves
}
