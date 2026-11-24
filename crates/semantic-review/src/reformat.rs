//! Finds the lines of a source diff that were only reformatted, as rustfmt
//! does when it rewraps a call or a struct literal, so that the viewer can
//! show them as unchanged.
//!
//! Both versions are split into Rust tokens and the tokens are diffed, as
//! difftastic does. Lines are then grouped: a removed and an added line
//! that share an unchanged token belong together. A group none of whose
//! lines has an added or removed token was only reformatted.

use model::Reformatted;
use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};
use std::time::{Duration, Instant};

/// Bodies with more tokens than this are left as line diffs.
const MAX_TOKENS: usize = 50_000;

/// The groups of lines of `old` and `new` (the two versions of an item's
/// source, line by line) that hold the same tokens and differ only in
/// whitespace, line breaks and trailing commas.
pub fn find(old: &[&str], new: &[&str]) -> Vec<Reformatted> {
    let (ot, nt) = (tokens(old), tokens(new));
    if ot.len() > MAX_TOKENS || nt.len() > MAX_TOKENS {
        return Vec::new();
    }
    let ok: Vec<&str> = ot.iter().map(|t| t.text.as_str()).collect();
    let nk: Vec<&str> = nt.iter().map(|t| t.text.as_str()).collect();
    let deadline = Instant::now() + Duration::from_millis(200);
    let ops = capture_diff_slices_deadline(Algorithm::Patience, &ok, &nk, Some(deadline));

    // Lines with an added or removed token.
    let mut old_dirty = vec![false; old.len()];
    let mut new_dirty = vec![false; new.len()];
    // Groups of lines joined by unchanged tokens: old and new line ranges,
    // both inclusive.
    let mut groups: Vec<[usize; 4]> = Vec::new();
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
                    Some(g) if o.first <= g[1] || n.first <= g[3] => {
                        g[1] = g[1].max(o.last);
                        g[3] = g[3].max(n.last);
                    }
                    _ => groups.push([o.first, o.last, n.first, n.last]),
                }
            }
        } else {
            for t in &ot[op.old_range()] {
                old_dirty[t.first..=t.last].fill(true);
            }
            for t in &nt[op.new_range()] {
                new_dirty[t.first..=t.last].fill(true);
            }
        }
    }

    let mut out = Vec::new();
    for [mut os, mut oe, mut ns, mut ne] in groups {
        if old_dirty[os..=oe].contains(&true) || new_dirty[ns..=ne].contains(&true) {
            continue;
        }
        // Lines that are the same at either end are plain unchanged lines.
        while os <= oe && ns <= ne && old[os] == new[ns] {
            os += 1;
            ns += 1;
        }
        while os <= oe && ns <= ne && old[oe] == new[ne] {
            if oe == 0 || ne == 0 {
                break;
            }
            oe -= 1;
            ne -= 1;
        }
        if os > oe || ns > ne {
            continue;
        }
        out.push(Reformatted {
            old: os as u32,
            old_len: (oe - os + 1) as u32,
            new: ns as u32,
            new_len: (ne - ns + 1) as u32,
        });
    }
    out
}

/// A token and the lines it spans.
#[derive(Debug)]
struct Token {
    text: String,
    first: usize,
    last: usize,
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
        }
    }

    #[test]
    fn a_rewrapped_call_is_reformatted() {
        let old = lines("fn f() {\n    let x = g(a, b);\n    x + 1\n}");
        let new = lines("fn f() {\n    let x = g(\n        a,\n        b,\n    );\n    x + 2\n}");
        assert_eq!(find(&old, &new), [group(1, 1, 1, 4)]);
    }

    #[test]
    fn a_rewrapped_line_with_a_change_is_not() {
        let old = lines("let x = g(a, b);");
        let new = lines("let x = g(\n    a,\n    c,\n);");
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
