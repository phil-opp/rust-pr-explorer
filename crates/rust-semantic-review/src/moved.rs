//! Code moved between items, such as the lines of an extracted helper, like
//! `git diff --color-moved`.
//!
//! Removed lines of one item that reappear among the added lines of another
//! (or elsewhere in the same item), the same apart from whitespace, are
//! marked as moved instead of as removed and added.

use model::{Item, Kind, MovedLines, Status};
use similar::{Algorithm, DiffOp, capture_diff_slices};
use std::collections::{BTreeMap, HashMap};

/// A block of moved lines needs at least this many alphanumeric characters,
/// as in git, so that lines such as `}` or `Ok(())` that happen to be
/// removed in one place and added in another don't count as moved.
const MIN_ALNUM: usize = 20;

/// Lines with more matches than this on the other side are too common to
/// start a block, e.g. a line repeated in many removed tests.
const MAX_MATCHES: usize = 200;

/// One side (removed or added lines) of one item.
struct Side {
    id: String,
    /// The side's version, line by line: its normalized text if the line
    /// is removed (or added), `None` if it is unchanged.
    lines: Vec<Option<String>>,
    /// The first line's number in its file.
    first_line: usize,
}

/// Sets `moved_lines` of every item whose removed or added lines were moved.
pub fn mark(items: &mut BTreeMap<String, Item>) {
    let mut removed: Vec<Side> = Vec::new();
    let mut added: Vec<Side> = Vec::new();
    for it in items.values() {
        // An impl's or trait's source includes its members, which are items
        // of their own; modules have no source.
        if matches!(it.info.kind, Kind::Impl | Kind::Trait | Kind::Mod) {
            continue;
        }
        let (old, new) = changed_lines(it);
        let side = |lines: Vec<Option<String>>, first_line: usize| Side {
            id: it.info.id.clone(),
            lines,
            first_line,
        };
        if old.iter().any(Option::is_some) {
            removed.push(side(old, it.old_line.unwrap_or(it.info.line)));
        }
        if new.iter().any(Option::is_some) {
            added.push(side(new, it.info.line));
        }
    }
    if removed.is_empty() || added.is_empty() {
        return;
    }

    // Where each removed line is, by its text.
    let mut index: HashMap<&str, Vec<(usize, usize)>> = HashMap::new();
    for (k, s) in removed.iter().enumerate() {
        for (j, l) in s.lines.iter().enumerate() {
            if let Some(l) = l
                .as_deref()
                .filter(|l| l.chars().any(char::is_alphanumeric))
            {
                index.entry(l).or_default().push((k, j));
            }
        }
    }
    index.retain(|_, v| v.len() <= MAX_MATCHES);
    // Lines that are already part of a block.
    let mut rem_used: Vec<Vec<bool>> = removed.iter().map(|s| vec![false; s.lines.len()]).collect();

    // Found blocks: (added side, start, removed side, start, length).
    let mut blocks: Vec<(usize, usize, usize, usize, usize)> = Vec::new();
    for (a, add) in added.iter().enumerate() {
        let mut add_used = vec![false; add.lines.len()];
        let mut i = 0;
        while i < add.lines.len() {
            let Some(cands) = add.lines[i].as_deref().and_then(|l| index.get(l)) else {
                i += 1;
                continue;
            };
            // The longest run of equal lines starting here.
            let mut best: Option<(usize, usize, usize)> = None;
            for &(k, j) in cands {
                let rem = &removed[k];
                let mut len = 0;
                while i + len < add.lines.len()
                    && j + len < rem.lines.len()
                    && !add_used[i + len]
                    && !rem_used[k][j + len]
                    && add.lines[i + len].is_some()
                    && add.lines[i + len] == rem.lines[j + len]
                {
                    len += 1;
                }
                // Blank lines may be inside a block, not at its end.
                while len > 0 && add.lines[i + len - 1].as_deref() == Some("") {
                    len -= 1;
                }
                if best.is_none_or(|(_, _, l)| len > l) {
                    best = Some((k, j, len));
                }
            }
            let Some((k, j, len)) = best.filter(|b| b.2 > 0) else {
                i += 1;
                continue;
            };
            let alnum: usize = add.lines[i..i + len]
                .iter()
                .flatten()
                .map(|l| l.chars().filter(|c| c.is_alphanumeric()).count())
                .sum();
            if alnum < MIN_ALNUM {
                i += 1;
                continue;
            }
            add_used[i..i + len].fill(true);
            rem_used[k][j..j + len].fill(true);
            blocks.push((a, i, k, j, len));
            i += len;
        }
    }

    for (a, i, k, j, len) in blocks {
        let (add, rem) = (&added[a], &removed[k]);
        let block = |side, start, other: &Side, other_start| MovedLines {
            side,
            start: start as u32,
            len: len as u32,
            other: other.id.clone(),
            other_line: (other.first_line + other_start) as u32,
        };
        if let Some(it) = items.get_mut(&add.id) {
            it.moved_lines.push(block('+', i, rem, j));
        }
        if let Some(it) = items.get_mut(&rem.id) {
            it.moved_lines.push(block('-', j, add, i));
        }
    }
    for it in items.values_mut() {
        it.moved_lines.sort_by_key(|m| (m.side, m.start));
    }
}

/// The item's base and head version, line by line, with the normalized
/// text of the lines that were removed and added.
///
/// Lines are compared without whitespace, so that a line that was only
/// indented differently is not taken for moved code.
fn changed_lines(it: &Item) -> (Vec<Option<String>>, Vec<Option<String>>) {
    let norm = |l: &str| l.split_whitespace().collect::<Vec<_>>().join(" ");
    let side = |skip: char| -> Vec<String> {
        it.source
            .iter()
            .filter(|l| l.0 != skip)
            .map(|l| norm(&l.1))
            .collect()
    };
    match it.status {
        Status::Added => (Vec::new(), side('-').into_iter().map(Some).collect()),
        Status::Removed => (side('+').into_iter().map(Some).collect(), Vec::new()),
        Status::Changed if it.source.iter().any(|l| l.0 != ' ') => {
            let (old, new) = (side('+'), side('-'));
            let mut old_out = vec![None; old.len()];
            let mut new_out = vec![None; new.len()];
            for op in capture_diff_slices(Algorithm::Myers, &old, &new) {
                if !matches!(op, DiffOp::Equal { .. }) {
                    for o in op.old_range() {
                        old_out[o] = Some(old[o].clone());
                    }
                    for n in op.new_range() {
                        new_out[n] = Some(new[n].clone());
                    }
                }
            }
            (old_out, new_out)
        }
        _ => (Vec::new(), Vec::new()),
    }
}
