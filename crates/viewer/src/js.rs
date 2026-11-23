//! JavaScript syntax highlighting for line diffs.
//!
//! Lines are highlighted one by one, as they come in a diff. What is
//! carried from one line to the next is whether a block comment or a
//! template literal is open. Regular expression literals are told from
//! division by the token before them.

use crate::code::esc;

/// What the lines before this one leave open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    comment: bool,
    template: bool,
}

const KEYWORDS: &[&str] = &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "of",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

const LITERALS: &[&str] = &["true", "false", "null", "undefined", "NaN", "Infinity"];

/// Highlights one line of JavaScript and updates `state` for the next.
pub fn line_html(state: &mut State, line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let span = |out: &mut String, class: &str, s: &[char]| {
        let s: String = s.iter().collect();
        if class.is_empty() {
            out.push_str(&esc(&s));
        } else {
            out.push_str(&format!(r#"<span class="{class}">{}</span>"#, esc(&s)));
        }
    };
    let mut i = 0;
    // Whether a `/` here starts a regular expression.
    let mut regex_ok = true;
    if state.comment {
        let close = find(&chars, 0, "*/");
        let end = close.map_or(chars.len(), |e| e + 2);
        span(&mut out, "cm", &chars[..end]);
        state.comment = close.is_none();
        i = end;
    } else if state.template {
        let end = template_end(&chars, 0);
        span(&mut out, "str", &chars[..end.min(chars.len())]);
        state.template = end > chars.len();
        i = end.min(chars.len());
        regex_ok = false;
    }
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            span(&mut out, "cm", &chars[i..]);
            break;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            match find(&chars, i + 2, "*/") {
                Some(e) => {
                    span(&mut out, "cm", &chars[i..e + 2]);
                    i = e + 2;
                }
                None => {
                    span(&mut out, "cm", &chars[i..]);
                    state.comment = true;
                    break;
                }
            }
            continue;
        }
        if c == '"' || c == '\'' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != c {
                if chars[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(chars.len());
            span(&mut out, "str", &chars[i..end]);
            i = end;
            regex_ok = false;
            continue;
        }
        if c == '`' {
            let end = template_end(&chars, i + 1);
            span(&mut out, "str", &chars[i..end.min(chars.len())]);
            if end > chars.len() {
                state.template = true;
                break;
            }
            i = end;
            regex_ok = false;
            continue;
        }
        if c == '/' && regex_ok {
            let mut j = i + 1;
            let mut class = false;
            let mut end = None;
            while j < chars.len() {
                match chars[j] {
                    '\\' => j += 1,
                    '[' => class = true,
                    ']' => class = false,
                    '/' if !class => {
                        j += 1;
                        while j < chars.len() && chars[j].is_alphanumeric() {
                            j += 1;
                        }
                        end = Some(j);
                        break;
                    }
                    _ => {}
                }
                j += 1;
            }
            if let Some(end) = end {
                span(&mut out, "str", &chars[i..end]);
                i = end;
                regex_ok = false;
                continue;
            }
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()))
        {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || matches!(chars[j], '.' | '_')) {
                j += 1;
            }
            span(&mut out, "num", &chars[i..j]);
            i = j;
            regex_ok = false;
            continue;
        }
        if c.is_alphabetic() || matches!(c, '_' | '$' | '#') {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || matches!(chars[j], '_' | '$')) {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            // Property names after `.` are not keywords.
            let after_dot = out.ends_with('.') && !out.ends_with("..");
            let call = chars[j..].iter().find(|c| !c.is_whitespace()) == Some(&'(');
            let class = if after_dot {
                if call { "jfn" } else { "" }
            } else if KEYWORDS.contains(&word.as_str()) {
                "kw"
            } else if LITERALS.contains(&word.as_str()) {
                "num"
            } else if call {
                "jfn"
            } else {
                ""
            };
            span(&mut out, class, &chars[i..j]);
            regex_ok =
                KEYWORDS.contains(&word.as_str()) && !matches!(word.as_str(), "this" | "super");
            i = j;
            continue;
        }
        regex_ok = !matches!(c, ')' | ']' | '}');
        out.push_str(&esc(&c.to_string()));
        i += 1;
    }
    out
}

fn find(chars: &[char], from: usize, pat: &str) -> Option<usize> {
    let p: Vec<char> = pat.chars().collect();
    (from..chars.len().saturating_sub(p.len() - 1)).find(|&i| chars[i..i + p.len()] == p[..])
}

/// The end (after the closing backtick) of a template literal whose
/// content starts at `i`; past the end of the line if it stays open.
/// `${…}` expressions are taken as part of it.
fn template_end(chars: &[char], i: usize) -> usize {
    let mut j = i;
    let mut depth = 0;
    while j < chars.len() {
        match chars[j] {
            '\\' => j += 1,
            '`' if depth == 0 => return j + 1,
            '$' if chars.get(j + 1) == Some(&'{') => {
                depth += 1;
                j += 1;
            }
            '}' if depth > 0 => depth -= 1,
            _ => {}
        }
        j += 1;
    }
    chars.len() + 1
}

/// Highlights several lines (a part's header).
pub fn text_html(text: &str) -> String {
    let mut state = State::default();
    text.split('\n')
        .map(|l| line_html(&mut state, l))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_code_across_lines() {
        let mut s = State::default();
        assert_eq!(
            line_html(&mut s, "const re = /a\\/b/g; // c"),
            r#"<span class="kw">const</span> re = <span class="str">/a\/b/g</span>; <span class="cm">// c</span>"#
        );
        assert_eq!(
            line_html(&mut s, "x = a / b / c; s.get(1);"),
            r#"x = a / b / c; s.<span class="jfn">get</span>(<span class="num">1</span>);"#
        );
        assert_eq!(
            line_html(&mut s, "const t = `a ${b}"),
            r#"<span class="kw">const</span> t = <span class="str">`a ${b}</span>"#
        );
        assert!(s.template);
        assert_eq!(
            line_html(&mut s, "c` + /* x"),
            r#"<span class="str">c`</span> + <span class="cm">/* x</span>"#
        );
        assert!(!s.template && s.comment);
        assert_eq!(
            line_html(&mut s, "*/ return null"),
            r#"<span class="cm">*/</span> <span class="kw">return</span> <span class="num">null</span>"#
        );
    }
}
