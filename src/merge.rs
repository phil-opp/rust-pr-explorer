//! Compares the base and head analyses item by item.

use crate::analyze::{Analysis, ItemData, Kind};
use serde::Serialize;
use similar::{ChangeTag, TextDiff};
use std::collections::{BTreeMap, HashMap};

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Added,
    Removed,
    Changed,
    Unchanged,
}

#[derive(Serialize, Default, Debug)]
pub struct Changes {
    pub signature: bool,
    pub body: bool,
    pub docs: bool,
    /// Only formatting or comments differ.
    pub cosmetic: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
}

#[derive(Serialize)]
pub struct OutItem {
    #[serde(flatten)]
    pub data: ItemData,
    pub status: Status,
    pub changes: Changes,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_sig: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_docs: Option<String>,
    /// Source lines as `[tag, text]`, tag being ` `, `+` or `-`.
    pub source: Vec<(char, String)>,
}

#[derive(Serialize)]
pub struct Meta {
    pub title: String,
    pub base: String,
    pub head: String,
    pub url: Option<String>,
}

#[derive(Serialize)]
pub struct Output {
    pub meta: Meta,
    pub crates: Vec<String>,
    pub items: BTreeMap<String, OutItem>,
    pub warnings: Vec<String>,
}

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
            None => OutItem {
                source: lines(&h.source, '+'),
                data: h,
                status: Status::Added,
                changes: Changes::default(),
                old_sig: None,
                old_docs: None,
            },
        };
        items.insert(id, item);
    }
    for (id, b) in base_items {
        items.insert(
            id,
            OutItem {
                source: lines(&b.source, '-'),
                data: b,
                status: Status::Removed,
                changes: Changes::default(),
                old_sig: None,
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

fn compare(b: ItemData, h: ItemData, moved_from: Option<String>) -> OutItem {
    let signature = b.sig_norm != h.sig_norm;
    let body = b.body_norm != h.body_norm;
    let docs = b.docs != h.docs;
    let structural = signature || body || docs;
    let cosmetic = !structural && b.source != h.source && h.kind != Kind::Mod;
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
                (tag, c.value().trim_end_matches('\n').to_string())
            })
            .collect()
    };
    OutItem {
        old_sig: (b.sig != h.sig).then(|| b.sig.clone()),
        old_docs: docs.then(|| b.docs.clone()),
        source,
        data: h,
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

fn lines(text: &str, tag: char) -> Vec<(char, String)> {
    if text.is_empty() {
        return Vec::new();
    }
    text.lines().map(|l| (tag, l.to_string())).collect()
}

/// Maps head ids to base ids of items that were moved to another module.
fn find_moves(
    base: &BTreeMap<String, ItemData>,
    head: &BTreeMap<String, ItemData>,
) -> HashMap<String, String> {
    fn key(item: &ItemData) -> (Kind, String, String) {
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
        if !head.contains_key(id) && item.kind != Kind::Mod {
            removed.entry(key(item)).or_default().push(id);
        }
    }
    let mut added: HashMap<_, Vec<&String>> = HashMap::new();
    for (id, item) in head {
        if !base.contains_key(id) && item.kind != Kind::Mod {
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
