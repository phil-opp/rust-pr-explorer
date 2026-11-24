//! Finds the lines of a source diff that were rewrapped, as rustfmt does
//! when it splits a call or a struct literal over several lines, so that
//! the viewer can show them as unchanged, or as a few changed tokens.
//!
//! Both versions are split into Rust tokens and the tokens are diffed, as
//! difftastic does. Lines are then grouped: a removed and an added line
//! that share an unchanged token belong together. A group none of whose
//! lines has an added or removed token was only reformatted. A group that
//! was rewrapped (its two versions have a different number of lines) and
//! keeps most of its tokens is kept too, with its added and removed tokens:
//! a call that got an argument and was then split by rustfmt reads better
//! as that argument than as one removed line and six added ones.

use model::{Reformatted, RemovedTokens, TokenSpan};
use similar::{Algorithm, DiffOp, capture_diff_slices, capture_diff_slices_deadline};
use std::ops::Range;
use std::time::{Duration, Instant};

/// Bodies with more tokens than this are left as line diffs.
const MAX_TOKENS: usize = 50_000;

/// The groups of lines of `old` and `new` (the two versions of an item's
/// source, line by line) that hold the same tokens apart from whitespace,
/// line breaks and trailing commas, or were rewrapped and had a few tokens
/// added or removed.
pub fn find(old: &[&str], new: &[&str]) -> Vec<Reformatted> {
    let (ot, nt) = (tokens(old), tokens(new));
    if ot.len() > MAX_TOKENS || nt.len() > MAX_TOKENS {
        return Vec::new();
    }
    let ok: Vec<&str> = ot.iter().map(|t| t.text.as_str()).collect();
    let nk: Vec<&str> = nt.iter().map(|t| t.text.as_str()).collect();
    let deadline = Instant::now() + Duration::from_millis(200);
    let ops = capture_diff_slices_deadline(Algorithm::Patience, &ok, &nk, Some(deadline));

    // Groups of lines joined by unchanged tokens.
    let mut groups: Vec<Group> = Vec::new();
    // Runs of added or removed tokens: old and new token ranges.
    let mut runs: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    for op in ops {
        if let DiffOp::Equal {
            old_index,
            new_index,
            len,
        } = op
        {
            for k in 0..len {
                let (o, n) = (&ot[old_index + k], &nt[new_index + k]);
                match groups.last_mut() {
                    Some(g) if o.first <= g.old_last || n.first <= g.new_last => {
                        g.old_last = g.old_last.max(o.last);
                        g.new_last = g.new_last.max(n.last);
                        g.same += o.text.chars().count();
                    }
                    _ => groups.push(Group {
                        old_first: o.first,
                        old_last: o.last,
                        new_first: n.first,
                        new_last: n.last,
                        same: o.text.chars().count(),
                    }),
                }
            }
        } else {
            runs.push((op.old_range(), op.new_range()));
        }
    }

    // The block of changed lines of the line diff that each line is in.
    let mut old_block = vec![None; old.len()];
    let mut new_block = vec![None; new.len()];
    for (k, op) in capture_diff_slices(Algorithm::Myers, old, new)
        .into_iter()
        .enumerate()
    {
        if !matches!(op, DiffOp::Equal { .. }) {
            old_block[op.old_range()].fill(Some(k));
            new_block[op.new_range()].fill(Some(k));
        }
    }

    let (old_text, new_text) = (Text::new(old), Text::new(new));
    let mut out = Vec::new();
    for g in groups {
        let (mut os, mut oe, mut ns, mut ne) = (g.old_first, g.old_last, g.new_first, g.new_last);
        let in_old = |t: &Token| os <= t.first && t.last <= oe;
        let in_new = |t: &Token| ns <= t.first && t.last <= ne;
        // The runs with tokens on the group's lines. A run that is only
        // partly on them makes the group a plain line diff.
        let mut mine = Vec::new();
        let mut clean = true;
        for (ro, rn) in &runs {
            let touches = ot[ro.clone()].iter().any(|t| t.last >= os && t.first <= oe)
                || nt[rn.clone()].iter().any(|t| t.last >= ns && t.first <= ne);
            if !touches {
                continue;
            }
            if !(ot[ro.clone()].iter().all(in_old) && nt[rn.clone()].iter().all(in_new)) {
                clean = false;
                break;
            }
            mine.push((ro.clone(), rn.clone()));
        }
        if !clean {
            continue;
        }
        let mut added = Vec::new();
        let mut removed = Vec::new();
        if !mine.is_empty() {
            // Lines that were edited, not rewrapped, are a line diff, and
            // so is a group that changed too much to be read as tokens.
            let changed: usize = mine
                .iter()
                .flat_map(|(ro, rn)| ot[ro.clone()].iter().chain(&nt[rn.clone()]))
                .filter(|t| t.text != ",")
                .map(|t| t.text.chars().count())
                .sum();
            if changed > g.same {
                continue;
            }
            for (ro, rn) in &mine {
                added.extend(spans(&new_text, &nt[rn.clone()]));
                // Where the removed tokens go in the new version: after the
                // token before them, else before the token after them.
                let at = if rn.start > 0 && in_new(&nt[rn.start - 1]) {
                    new_text.pos(nt[rn.start - 1].end)
                } else if let Some(next) = nt.get(rn.start).filter(|t| in_new(t)) {
                    new_text.pos(next.start)
                } else {
                    continue;
                };
                let at = TokenSpan {
                    line: at.0 as u32,
                    start: at.1 as u32,
                    end: at.1 as u32,
                };
                // What separated them from the token before them, if on
                // the same line.
                let before = match ro.start.checked_sub(1).map(|i| &ot[i]) {
                    Some(p) => old_text.between(p.end, ot[ro.start].start),
                    None => String::new(),
                };
                for (k, span) in spans(&old_text, &ot[ro.clone()]).into_iter().enumerate() {
                    let text = old_text.slice(span);
                    let lead = if k == 0 { before.clone() } else { " ".into() };
                    removed.push(RemovedTokens {
                        old: span,
                        at,
                        text: lead + &text,
                    });
                }
            }
            if added.is_empty() && removed.is_empty() {
                // Only commas changed.
                continue;
            }
        }
        // Lines that are the same at either end are plain unchanged lines.
        let new_touched = |l: usize| {
            added.iter().any(|t| t.line as usize == l)
                || removed.iter().any(|t| t.at.line as usize == l)
        };
        let old_touched = |l: usize| removed.iter().any(|t| t.old.line as usize == l);
        while os <= oe && ns <= ne && old[os] == new[ns] && !old_touched(os) && !new_touched(ns) {
            os += 1;
            ns += 1;
        }
        while os <= oe && ns <= ne && old[oe] == new[ne] && !old_touched(oe) && !new_touched(ne) {
            if oe == 0 || ne == 0 {
                break;
            }
            oe -= 1;
            ne -= 1;
        }
        if os > oe || ns > ne {
            continue;
        }
        // With changed tokens, the group must be lines that the line diff
        // replaces with others, and a different number of them: lines
        // edited in place get their changed words marked anyway, and a
        // statement that moved to another line can look like a rewrap
        // (its `let` kept, the rest removed) but is no such thing.
        if !(added.is_empty() && removed.is_empty())
            && (oe - os == ne - ns
                || old_block[os..=oe]
                    .iter()
                    .chain(&new_block[ns..=ne])
                    .any(|b| b.is_none() || *b != old_block[os]))
        {
            continue;
        }
        out.push(Reformatted {
            old: os as u32,
            old_len: (oe - os + 1) as u32,
            new: ns as u32,
            new_len: (ne - ns + 1) as u32,
            added,
            removed,
        });
    }
    out
}

/// Lines of both versions joined by unchanged tokens (inclusive), and the
/// length of those tokens.
struct Group {
    old_first: usize,
    old_last: usize,
    new_first: usize,
    new_last: usize,
    same: usize,
}

/// The spans that `toks` (a run of consecutive tokens) cover, one per
/// line, from the first token's start to the last one's end on that line.
/// Spans of only commas are left out: rustfmt adds and removes those, and
/// an added argument brings one along.
fn spans(text: &Text, toks: &[Token]) -> Vec<TokenSpan> {
    let mut out: Vec<TokenSpan> = Vec::new();
    for t in toks {
        let (mut line, mut col) = text.pos(t.start);
        let (end_line, end_col) = text.pos(t.end);
        while line <= end_line {
            let end = if line == end_line {
                end_col
            } else {
                text.line_len(line)
            };
            match out.last_mut() {
                Some(s) if s.line as usize == line => s.end = end as u32,
                _ => out.push(TokenSpan {
                    line: line as u32,
                    start: col as u32,
                    end: end as u32,
                }),
            }
            line += 1;
            col = 0;
        }
    }
    out.retain(|s| {
        text.slice(*s)
            .chars()
            .any(|c| !c.is_whitespace() && c != ',')
    });
    out
}

/// The lines of a version joined, with a way to turn character offsets
/// into lines and columns.
struct Text {
    chars: Vec<char>,
    /// The offset of each line's first character.
    starts: Vec<usize>,
}

impl Text {
    fn new(lines: &[&str]) -> Text {
        let chars: Vec<char> = lines.join("\n").chars().collect();
        let mut starts = vec![0];
        starts.extend(
            chars
                .iter()
                .enumerate()
                .filter(|(_, c)| **c == '\n')
                .map(|(i, _)| i + 1),
        );
        Text { chars, starts }
    }

    /// The line and column of offset `i`.
    fn pos(&self, i: usize) -> (usize, usize) {
        let line = self.starts.partition_point(|&s| s <= i) - 1;
        (line, i - self.starts[line])
    }

    fn line_len(&self, line: usize) -> usize {
        let end = self
            .starts
            .get(line + 1)
            .map_or(self.chars.len(), |s| s - 1);
        end - self.starts[line]
    }

    fn slice(&self, s: TokenSpan) -> String {
        let at = self.starts[s.line as usize];
        self.chars[at + s.start as usize..at + s.end as usize]
            .iter()
            .collect()
    }

    /// The text between two offsets, if it is on one line.
    fn between(&self, from: usize, to: usize) -> String {
        let s: String = self.chars[from..to].iter().collect();
        if s.contains('\n') { String::new() } else { s }
    }
}

/// A token, the lines it spans, and its offsets in the joined lines.
#[derive(Debug)]
struct Token {
    text: String,
    first: usize,
    last: usize,
    start: usize,
    end: usize,
}

/// The tokens of `lines`, without whitespace, and without trailing commas
/// (a `,` right before a closing bracket), which rustfmt adds and removes
/// when it rewraps.
fn tokens(lines: &[&str]) -> Vec<Token> {
    let text = lines.join("\n");
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let mut line = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            if c == '\n' {
                line += 1;
            }
            i += 1;
            continue;
        }
        let start = i;
        let next = chars.get(i + 1).copied();
        i = if c == '/' && next == Some('/') {
            chars[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |p| i + p)
        } else if c == '/' && next == Some('*') {
            block_comment_end(&chars, i)
        } else if let Some(end) = string_end(&chars, i) {
            end
        } else if c == '\'' {
            char_end(&chars, i)
        } else if c.is_alphanumeric() || c == '_' {
            i + chars[i..]
                .iter()
                .take_while(|c| c.is_alphanumeric() || **c == '_')
                .count()
        } else {
            i + 1
        };
        let s: String = chars[start..i].iter().collect();
        let lines_in = s.matches('\n').count();
        // Comments are compared word by word, so that rewrapping a comment
        // doesn't count as changing it.
        let s = if s.starts_with("//") || s.starts_with("/*") {
            s.split_whitespace().collect::<Vec<_>>().join(" ")
        } else {
            s
        };
        if matches!(s.as_str(), ")" | "]" | "}") && out.last().is_some_and(|t| t.text == ",") {
            out.pop();
        }
        out.push(Token {
            text: s,
            first: line,
            last: line + lines_in,
            start,
            end: i,
        });
        line += lines_in;
    }
    out
}

/// The end of a (nested) block comment starting at `i`.
fn block_comment_end(chars: &[char], mut i: usize) -> usize {
    let mut depth = 0;
    while i < chars.len() {
        match (chars[i], chars.get(i + 1)) {
            ('/', Some('*')) => {
                depth += 1;
                i += 2;
            }
            ('*', Some('/')) => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return i;
                }
            }
            _ => i += 1,
        }
    }
    chars.len()
}

/// The end of a string literal starting at `i` (`"…"`, `b"…"`, `c"…"`,
/// `r#"…"#`, `br"…"`, …), if one starts there.
fn string_end(chars: &[char], i: usize) -> Option<usize> {
    // Not inside an identifier such as `bar"` (which isn't valid anyway).
    if i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_') {
        return None;
    }
    let mut j = i;
    if matches!(chars.get(j), Some('b' | 'c')) {
        j += 1;
    }
    let raw = chars.get(j) == Some(&'r');
    if raw {
        j += 1;
    }
    let hashes = chars[j.min(chars.len())..]
        .iter()
        .take_while(|c| **c == '#')
        .count();
    if !raw && hashes > 0 {
        return None;
    }
    j += hashes;
    if chars.get(j) != Some(&'"') {
        return None;
    }
    j += 1;
    while j < chars.len() {
        match chars[j] {
            '\\' if !raw => j += 2,
            '"' if chars[j + 1..]
                .iter()
                .take(hashes)
                .filter(|c| **c == '#')
                .count()
                == hashes =>
            {
                return Some(j + 1 + hashes);
            }
            _ => j += 1,
        }
    }
    Some(chars.len())
}

/// The end of a char literal (`'a'`, `'\n'`) or a lifetime (`'a`) at `i`.
fn char_end(chars: &[char], i: usize) -> usize {
    match (chars.get(i + 1), chars.get(i + 2)) {
        (Some('\\'), _) => {
            let mut j = i + 2;
            while j < chars.len() && j < i + 12 {
                j += 1;
                if chars[j - 1] == '\'' && j > i + 3 {
                    return j;
                }
            }
            i + 1
        }
        (Some(_), Some('\'')) => i + 3,
        _ => {
            i + 1
                + chars[i + 1..]
                    .iter()
                    .take_while(|c| c.is_alphanumeric() || **c == '_')
                    .count()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<&str> {
        s.lines().collect()
    }

    fn group(old: u32, old_len: u32, new: u32, new_len: u32) -> Reformatted {
        Reformatted {
            old,
            old_len,
            new,
            new_len,
            added: Vec::new(),
            removed: Vec::new(),
        }
    }

    fn span(line: u32, start: u32, end: u32) -> TokenSpan {
        TokenSpan { line, start, end }
    }

    #[test]
    fn a_rewrapped_call_is_reformatted() {
        let old = lines("fn f() {\n    let x = g(a, b);\n    x + 1\n}");
        let new = lines("fn f() {\n    let x = g(\n        a,\n        b,\n    );\n    x + 2\n}");
        assert_eq!(find(&old, &new), [group(1, 1, 1, 4)]);
    }

    #[test]
    fn a_rewrapped_call_with_a_new_argument_has_the_argument_added() {
        // As in dora#4273: an argument, and rustfmt splits the call.
        let old = lines("    .spawn_inner(&mut logger, op_rx, finished_tx)\n    .await");
        let new = lines(
            "    .spawn_inner(\n        &mut logger,\n        op_rx,\n        finished_tx,\n        \
             &AtomicU32::new(0),\n    )\n    .await",
        );
        let mut g = group(0, 1, 0, 6);
        // Only the argument: the comma after `finished_tx` comes with it.
        g.added = vec![span(4, 8, 26)];
        assert_eq!(find(&old, &new), [g.clone()]);
        assert!(g.adds_line(4, new[4]));
        assert!(!g.changes_new_line(3));
    }

    #[test]
    fn a_rewrapped_call_with_a_replaced_argument() {
        let old = lines("let x = g(a, b);");
        let new = lines("let x = g(\n    a,\n    c,\n);");
        let mut g = group(0, 1, 0, 4);
        g.added = vec![span(2, 4, 5)];
        // `b` would go after `a,`: it is shown there, struck out.
        g.removed = vec![RemovedTokens {
            old: span(0, 13, 14),
            at: span(1, 6, 6),
            text: " b".into(),
        }];
        assert_eq!(find(&old, &new), [g]);
    }

    #[test]
    fn an_edit_without_a_rewrap_or_with_too_many_changes_is_a_line_diff() {
        // The same lines: the line diff marks the changed words already.
        let old = lines("let x = g(a, b);\nlet y = 1;");
        let new = lines("let x = g(a, c);\nlet y = 1;");
        assert_eq!(find(&old, &new), []);
        // A statement moved up: its `let` is kept by the token diff.
        let old = lines("f(x);\nlet a = g(1);\nlet b = 2;");
        let new = lines("let a = g(1);\nf(x, a);\nlet b = 2;");
        assert_eq!(find(&old, &new), []);
        // Mostly new tokens.
        let old = lines("f(a);");
        let new = lines("f(\n    compute_something_else(x, y, z),\n    more_of_it,\n);");
        assert_eq!(find(&old, &new), []);
    }

    #[test]
    fn strings_and_comments_keep_their_text() {
        // Whitespace inside a string is part of its value.
        let old = lines(r#"let s = "a  b";"#);
        let new = lines(r#"let s = "a b";"#);
        assert_eq!(find(&old, &new), []);
        // Line comments are compared line by line, block comments as a
        // whole.
        let old = lines("// one two\n// three\nx();");
        let new = lines("// one\n// two three\nx();");
        assert_eq!(find(&old, &new), []);
        let old = lines("/* one two\n   three */\nx();");
        let new = lines("/* one\n   two three */\nx();");
        assert_eq!(find(&old, &new), [group(0, 2, 0, 2)]);
    }

    #[test]
    fn tokens_of_literals_and_lifetimes() {
        let t: Vec<String> = tokens(&[r##"f::<'a>('x', '\n', r#"a "b"#, b"c\"d", x_1)"##])
            .into_iter()
            .map(|t| t.text)
            .collect();
        assert_eq!(
            t,
            [
                "f",
                ":",
                ":",
                "<",
                "'a",
                ">",
                "(",
                "'x'",
                ",",
                "'\\n'",
                ",",
                "r#\"a \"b\"#",
                ",",
                "b\"c\\\"d\"",
                ",",
                "x_1",
                ")"
            ]
        );
    }
}
