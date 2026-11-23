//! CSS: syntax highlighting for line diffs, and the selector boxes of a
//! changed stylesheet (see [`StyleChange`]).
//!
//! Lines are highlighted one by one, as they come in a diff. The only state
//! carried from one line to the next is whether a comment is open. Whether
//! text is a selector or a declaration is told by how it ends: text before
//! a `{` is a selector (or an at-rule), `name: value` before a `;` or `}`
//! is a declaration.

use crate::code::esc;
use model::{PropChange, Status, StyleChange};

/// What the lines before this one leave open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    comment: bool,
}

/// Highlights one line of CSS and updates `state` for the next line.
pub fn line_html(state: &mut State, line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut class: Vec<&'static str> = vec![""; chars.len()];
    // Code chars (not in comments or strings), by index.
    let mut code = vec![false; chars.len()];
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        if state.comment {
            class[i] = "cm";
            if c == '*' && chars.get(i + 1) == Some(&'/') {
                class[i + 1] = "cm";
                state.comment = false;
                i += 1;
            }
        } else if let Some(q) = quote {
            class[i] = "str";
            if c == '\\' && i + 1 < chars.len() {
                class[i + 1] = "str";
                i += 1;
            } else if c == q {
                quote = None;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            class[i] = "cm";
            class[i + 1] = "cm";
            state.comment = true;
            i += 1;
        } else if c == '"' || c == '\'' {
            class[i] = "str";
            quote = Some(c);
        } else {
            code[i] = true;
        }
        i += 1;
    }
    // Segments between `{`, `}` and `;` outside brackets.
    let mut start = 0;
    let mut depth = 0usize;
    for i in 0..=chars.len() {
        let end = i == chars.len();
        if !end && code[i] {
            match chars[i] {
                '(' | '[' => depth += 1,
                ')' | ']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        if end || (code[i] && depth == 0 && matches!(chars[i], '{' | '}' | ';')) {
            let term = if end { None } else { Some(chars[i]) };
            segment(&chars, &code, &mut class, start, i, term);
            start = i + 1;
        }
    }
    // Group runs of the same class.
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = class[i];
        let mut j = i;
        while j < chars.len() && class[j] == c {
            j += 1;
        }
        let text: String = chars[i..j].iter().collect();
        if c.is_empty() {
            out.push_str(&esc(&text));
        } else {
            out.push_str(&format!(r#"<span class="{c}">{}</span>"#, esc(&text)));
        }
        i = j;
    }
    out
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

/// Classes for the code chars of `chars[from..to]`, which `term` ends.
fn segment(
    chars: &[char],
    code: &[bool],
    class: &mut [&'static str],
    from: usize,
    to: usize,
    term: Option<char>,
) {
    let Some(first) = (from..to).find(|&i| code[i] && !chars[i].is_whitespace()) else {
        return;
    };
    if chars[first] == '@' {
        let mut j = first + 1;
        while j < to && is_ident(chars[j]) {
            j += 1;
        }
        class[first..j].fill("kw");
        value(chars, code, class, j, to);
        return;
    }
    // `name:` followed by a space, the end, or a `;`/`}`: a declaration.
    let mut j = first;
    while j < to && is_ident(chars[j]) {
        j += 1;
    }
    let colon = (j..to).find(|&k| !chars[k].is_whitespace());
    if let Some(k) = colon
        && j > first
        && chars[k] == ':'
        && term != Some('{')
        && (matches!(term, Some(';' | '}')) || chars.get(k + 1).is_none_or(|c| c.is_whitespace()))
    {
        class[first..j].fill("yk");
        value(chars, code, class, k + 1, to);
        return;
    }
    // A selector: up to its last non-blank char.
    let last = (from..to)
        .rev()
        .find(|&i| code[i] && !chars[i].is_whitespace())
        .unwrap_or(first);
    for i in first..=last {
        if code[i] {
            class[i] = "sel";
        }
    }
}

/// Numbers (with units), colors and `!important` in a value.
fn value(chars: &[char], code: &[bool], class: &mut [&'static str], from: usize, to: usize) {
    let mut i = from;
    while i < to {
        if !code[i] {
            i += 1;
            continue;
        }
        let c = chars[i];
        let after_word = i > from && is_ident(chars[i - 1]);
        let starts_num = c.is_ascii_digit()
            || (matches!(c, '.' | '-' | '+')
                && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()));
        if (starts_num || c == '#') && !after_word {
            let mut j = i + 1;
            while j < to && (is_ident(chars[j]) || chars[j] == '.' || chars[j] == '%') {
                j += 1;
            }
            class[i..j].fill("num");
            i = j;
        } else if c == '!' {
            let mut j = i + 1;
            while j < to && (chars[j].is_alphabetic() || chars[j].is_whitespace()) {
                j += 1;
            }
            class[i..j].fill("kw");
            i = j;
        } else {
            i += 1;
        }
    }
}

/// A property's value, highlighted.
fn value_html(v: &str) -> String {
    line_html(&mut State::default(), &format!("x: {v}"))
        .split_once(':')
        .map(|(_, rest)| rest.trim_start().to_string())
        .unwrap_or_default()
}

/// One row of a property table. `old` is whether the table has a column
/// for old values, `lines` whether it has one for the line each value
/// comes from (when the selector has several rules).
fn prop_row(p: &PropChange, old: bool, lines: bool) -> String {
    let old_cell = if old {
        r#"<td class="mf-old"></td>"#
    } else {
        ""
    };
    let name = format!(r#"<td class="mf-name"><code>{}</code></td>"#, esc(&p.name));
    let line = |l: usize, was: bool| {
        if !lines {
            return String::new();
        }
        format!(
            r#"<td class="css-ln">{}line {l}</td>"#,
            if was { "was " } else { "" }
        )
    };
    match (&p.old, &p.new, &p.note) {
        (_, Some(n), Some(note)) => format!(
            r#"<tr class="css-noop"><td class="mf-name"><code>{}</code></td><td colspan="{}" class="mf-new"><code>{}</code><div class="note">{}</div></td>{}</tr>"#,
            esc(&p.name),
            if old { 2 } else { 1 },
            value_html(&n.value),
            note_html(note),
            line(n.line, false)
        ),
        (None, Some(n), None) => format!(
            r#"<tr class="m-add" title="added">{name}{old_cell}<td class="mf-new"><code>{}</code></td>{}</tr>"#,
            value_html(&n.value),
            line(n.line, false)
        ),
        (Some(o), None, _) => format!(
            r#"<tr class="m-del" title="removed">{name}<td class="mf-old"><code><del>{}</del></code></td><td class="mf-new"></td>{}</tr>"#,
            esc(&o.value),
            line(o.line, true)
        ),
        (Some(o), Some(n), None) => format!(
            r#"<tr class="m-chg" title="changed">{name}<td class="mf-old"><code><del>{}</del></code></td><td class="mf-new"><code>{}</code></td>{}</tr>"#,
            esc(&o.value),
            value_html(&n.value),
            line(n.line, false)
        ),
        _ => String::new(),
    }
}

/// A note with `code` in backticks.
fn note_html(note: &str) -> String {
    note.split('`')
        .enumerate()
        .map(|(i, s)| {
            if i % 2 == 1 {
                format!("<code>{}</code>", esc(s))
            } else {
                esc(s)
            }
        })
        .collect()
}

/// Unchanged properties are listed among the changes up to this many,
/// folded below them beyond.
const SAME_INLINE: usize = 5;

fn same_rows(list: &[PropChange], old: bool, lines: bool) -> String {
    let old_cell = if old {
        r#"<td class="mf-old"></td>"#
    } else {
        ""
    };
    list.iter()
        .filter_map(|p| p.new.as_ref().map(|n| (p, n)))
        .map(|(p, n)| {
            format!(
                r#"<tr class="css-same"><td class="mf-name"><code>{}</code></td>{old_cell}<td class="mf-new"><code>{}</code></td>{}</tr>"#,
                esc(&p.name),
                value_html(&n.value),
                if lines {
                    format!(r#"<td class="css-ln">line {}</td>"#, n.line)
                } else {
                    String::new()
                }
            )
        })
        .collect()
}

fn lines_text(rules: &[usize]) -> String {
    match rules {
        [] => String::new(),
        [one] => format!("line {one}"),
        [rest @ .., last] => format!(
            "lines {} and {last}",
            rest.iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A stylesheet's changed selectors, one box each.
pub fn styles_html(list: &[StyleChange]) -> String {
    list.iter()
        .map(|c| {
            let kind = if c.statement {
                "at-rule"
            } else if c.selectors.len() > 1 {
                "selectors"
            } else {
                "selector"
            };
            let title = c
                .selectors
                .iter()
                .map(|s| esc(s))
                .collect::<Vec<_>>()
                .join(r#"<span class="dim">, </span>"#);
            let rules = if c.statement {
                lines_text(&c.rules)
            } else if c.rules.len() > 1 {
                format!("merged from the rules on {}", lines_text(&c.rules))
            } else if c.status == Status::Removed {
                format!("was on {}", lines_text(&c.rules))
            } else {
                format!("rule on {}", lines_text(&c.rules))
            };
            let ctx = if c.context.is_empty() {
                String::new()
            } else {
                format!(
                    r#"<div class="css-ctx">in <code>{}</code></div>"#,
                    c.context
                        .iter()
                        .map(|x| value_html(x))
                        .collect::<Vec<_>>()
                        .join(" › ")
                )
            };
            // A column for old values only if there are any.
            let old = c.props.iter().any(|p| p.old.is_some() && p.note.is_none());
            let lines = c.rules.len() > 1;
            let mut rows: String = c.props.iter().map(|p| prop_row(p, old, lines)).collect();
            let mut more = String::new();
            if c.unchanged.len() <= SAME_INLINE {
                rows.push_str(&same_rows(&c.unchanged, old, lines));
            } else {
                more = format!(
                    r#"<details class="css-more"><summary>{} unchanged properties</summary><table class="manifest css-props">{}</table></details>"#,
                    c.unchanged.len(),
                    same_rows(&c.unchanged, false, lines)
                );
            }
            let table = if rows.is_empty() {
                String::new()
            } else {
                format!(r#"<table class="manifest css-props">{rows}</table>"#)
            };
            let (status_class, badge) = match c.status {
                Status::Added => (
                    "added",
                    r#"<span class="st st-added badge">New</span>"#,
                ),
                Status::Removed => (
                    "removed",
                    r#"<span class="st st-removed badge">Removed</span>"#,
                ),
                _ => ("", ""),
            };
            format!(
                r#"<div class="irow member part css-rule {status_class}"><div class="ihead"><span class="kind">{kind}</span><span class="title-name css-sel">{title}</span>{badge}<span class="dim css-rules">{rules}</span></div>{ctx}{table}{more}</div>"#
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_selectors_declarations_and_comments() {
        let mut s = State::default();
        assert_eq!(
            line_html(&mut s, ".a:hover > b { color: #fff !important; } /* x"),
            r##"<span class="sel">.a:hover &gt; b</span> { <span class="yk">color</span>: <span class="num">#fff</span> <span class="kw">!important</span>; } <span class="cm">/* x</span>"##
        );
        assert!(s.comment);
        assert_eq!(
            line_html(&mut s, "y */ margin: 0 4px;"),
            r#"<span class="cm">y */</span> <span class="yk">margin</span>: <span class="num">0</span> <span class="num">4px</span>;"#
        );
        assert_eq!(
            line_html(&mut s, "@media (max-width: 600px) {"),
            r#"<span class="kw">@media</span> (max-width: <span class="num">600px</span>) {"#
        );
        assert_eq!(
            line_html(&mut s, r#"  content: "a;b";"#),
            r#"  <span class="yk">content</span>: <span class="str">&quot;a;b&quot;</span>;"#
        );
    }
}
