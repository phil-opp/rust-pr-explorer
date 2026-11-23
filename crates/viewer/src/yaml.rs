//! YAML syntax highlighting for line diffs, with the expressions of GitHub
//! Actions workflows (`${{ … }}`) marked wherever they appear.
//!
//! Lines are highlighted one by one, as they come in a diff. The only state
//! carried from one line to the next is whether a block scalar (`run: |`)
//! is open: lines indented deeper than its key are its content.

use crate::code::esc;

/// What the lines before this one leave open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// Inside a block scalar whose key (or `-`) starts at this column.
    block: Option<usize>,
}

/// Highlights one line of YAML and updates `state` for the next line.
pub fn line_html(state: &mut State, line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let indent = chars.iter().take_while(|&&c| c == ' ').count();
    if let Some(parent) = state.block {
        if indent == chars.len() || indent > parent {
            return text_html(&chars, "");
        }
        state.block = None;
    }
    let mut out = esc(&" ".repeat(indent));
    let mut i = indent;
    if i == 0 && chars.first() == Some(&'%') {
        out.push_str(&span("attr", &chars));
        return out;
    }
    if i == 0 && (starts_with(&chars, "---") || starts_with(&chars, "...")) && ends_token(&chars, 3)
    {
        out.push_str(&span("kw", &chars[..3]));
        i = 3;
    }
    // Sequence entries, possibly nested on one line: `- - a`.
    let mut node = i;
    loop {
        let start = skip_spaces(&chars, i, &mut out);
        if chars.get(start) == Some(&'-') && ends_token(&chars, start + 1) {
            node = start;
            out.push('-');
            i = start + 1;
        } else {
            i = start;
            break;
        }
    }
    if let Some(end) = key_end(&chars, i) {
        node = i;
        let key: String = chars[i..end].iter().collect();
        let trimmed = key.trim_end();
        out.push_str(&format!(r#"<span class="yk">{}</span>"#, esc(trimmed)));
        out.push_str(&esc(&key[trimmed.len()..]));
        out.push(':');
        i = end + 1;
    }
    if value_html(&chars, i, &mut out) {
        state.block = Some(node);
    }
    out
}

/// Highlights a value and what follows it on the line. Returns whether it
/// opens a block scalar.
fn value_html(chars: &[char], mut i: usize, out: &mut String) -> bool {
    loop {
        i = skip_spaces(chars, i, out);
        let Some(&c) = chars.get(i) else {
            return false;
        };
        match c {
            '#' => {
                out.push_str(&span("cm", &chars[i..]));
                return false;
            }
            '&' | '*' | '!' => {
                let end = token_end(chars, i);
                out.push_str(&span("attr", &chars[i..end]));
                i = end;
            }
            '|' | '>' => {
                let mut end = i + 1;
                while chars
                    .get(end)
                    .is_some_and(|c| c.is_ascii_digit() || *c == '+' || *c == '-')
                {
                    end += 1;
                }
                if !ends_token(chars, end) {
                    // Not a block indicator; let the plain scalar take it.
                    return plain_html(chars, i, out);
                }
                out.push_str(&span("kw", &chars[i..end]));
                let rest = skip_spaces(chars, end, out);
                if rest < chars.len() {
                    out.push_str(&span("cm", &chars[rest..]));
                }
                return true;
            }
            '"' | '\'' => {
                let end = quoted_end(chars, i);
                out.push_str(&text_html(&chars[i..end], "str"));
                i = end;
            }
            '[' | '{' => {
                flow_html(chars, i, out);
                return false;
            }
            _ => return plain_html(chars, i, out),
        }
    }
}

/// A plain (unquoted) scalar up to a comment or the end of the line.
fn plain_html(chars: &[char], i: usize, out: &mut String) -> bool {
    let end = (i..chars.len())
        .find(|&j| chars[j] == '#' && j > 0 && chars[j - 1] == ' ')
        .unwrap_or(chars.len());
    let text: String = chars[i..end].iter().collect();
    let trimmed = text.trim_end();
    out.push_str(&scalar_html(&chars[i..i + trimmed.chars().count()]));
    out.push_str(&esc(&text[trimmed.len()..]));
    if end < chars.len() {
        out.push_str(&span("cm", &chars[end..]));
    }
    false
}

/// A flow collection (`[a, b]`, `{a: b}`), to the end of the line.
fn flow_html(chars: &[char], mut i: usize, out: &mut String) {
    while i < chars.len() {
        let c = chars[i];
        match c {
            '[' | ']' | '{' | '}' | ',' | ':' | ' ' => {
                out.push_str(&esc(&c.to_string()));
                i += 1;
            }
            '#' if i > 0 && chars[i - 1] == ' ' => {
                out.push_str(&span("cm", &chars[i..]));
                return;
            }
            '"' | '\'' => {
                let end = quoted_end(chars, i);
                out.push_str(&text_html(&chars[i..end], "str"));
                i = end;
            }
            _ => {
                let mut end = i;
                while end < chars.len()
                    && !matches!(chars[end], '[' | ']' | '{' | '}' | ',')
                    && !(chars[end] == ':' && ends_token(chars, end + 1))
                    && !(chars[end] == '#' && chars[end - 1] == ' ')
                {
                    end += 1;
                }
                let text: String = chars[i..end].iter().collect();
                let n = text.trim_end().chars().count();
                if chars.get(end) == Some(&':') {
                    out.push_str(&span("yk", &chars[i..i + n]));
                } else {
                    out.push_str(&scalar_html(&chars[i..i + n]));
                }
                out.push_str(&esc(&" ".repeat(end - i - n)));
                i = end;
            }
        }
    }
}

/// A plain scalar: a keyword, a number, or a string.
fn scalar_html(chars: &[char]) -> String {
    let s: String = chars.iter().collect();
    let keyword = matches!(
        s.as_str(),
        "true" | "True" | "TRUE" | "false" | "False" | "FALSE" | "null" | "Null" | "NULL" | "~"
    );
    if keyword {
        span("kw", chars)
    } else if is_number(&s) {
        span("num", chars)
    } else {
        text_html(chars, "str")
    }
}

fn is_number(s: &str) -> bool {
    let s = s.strip_prefix(['-', '+']).unwrap_or(s);
    if matches!(s, ".inf" | ".Inf" | ".INF" | ".nan" | ".NaN" | ".NAN") {
        return true;
    }
    if let Some(hex) = s.strip_prefix("0x") {
        return !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    if let Some(oct) = s.strip_prefix("0o") {
        return !oct.is_empty() && oct.chars().all(|c| c.is_digit(8));
    }
    let (mantissa, exp) = s.split_once(['e', 'E']).unwrap_or((s, "0"));
    let exp = exp.strip_prefix(['-', '+']).unwrap_or(exp);
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    (!int.is_empty() || !frac.is_empty())
        && int.chars().all(|c| c.is_ascii_digit())
        && frac.chars().all(|c| c.is_ascii_digit())
        && !exp.is_empty()
        && exp.chars().all(|c| c.is_ascii_digit())
}

/// Text in `class` (none if empty), with `${{ … }}` expressions marked.
fn text_html(chars: &[char], class: &str) -> String {
    let mut out = String::new();
    let mut plain = 0;
    let mut i = 0;
    let flush = |out: &mut String, part: &[char]| {
        if part.is_empty() {
        } else if class.is_empty() {
            out.push_str(&esc(&part.iter().collect::<String>()));
        } else {
            out.push_str(&span(class, part));
        }
    };
    while i < chars.len() {
        if starts_with(&chars[i..], "${{") {
            let close = (i + 3..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '}' && chars[j + 1] == '}');
            if let Some(close) = close {
                flush(&mut out, &chars[plain..i]);
                out.push_str(&span("expr", &chars[i..close + 2]));
                i = close + 2;
                plain = i;
                continue;
            }
        }
        i += 1;
    }
    flush(&mut out, &chars[plain..]);
    out
}

/// Where a mapping key starting at `i` ends: the position of its `:`.
fn key_end(chars: &[char], i: usize) -> Option<usize> {
    let c = *chars.get(i)?;
    if matches!(
        c,
        '[' | '{' | '#' | '|' | '>' | '&' | '*' | '!' | '%' | '@' | '`'
    ) {
        return None;
    }
    if c == '"' || c == '\'' {
        let mut j = quoted_end(chars, i);
        while chars.get(j) == Some(&' ') {
            j += 1;
        }
        return (chars.get(j) == Some(&':') && ends_token(chars, j + 1)).then_some(j);
    }
    for j in i..chars.len() {
        match chars[j] {
            ':' if ends_token(chars, j + 1) => return Some(j),
            '#' if chars[j - 1] == ' ' => return None,
            _ => {}
        }
    }
    None
}

/// The end of a quoted string starting at `i` (the end of the line if it
/// isn't closed there).
fn quoted_end(chars: &[char], i: usize) -> usize {
    let quote = chars[i];
    let mut j = i + 1;
    while j < chars.len() {
        match chars[j] {
            '\\' if quote == '"' => j += 2,
            '\'' if quote == '\'' && chars.get(j + 1) == Some(&'\'') => j += 2,
            c if c == quote => return j + 1,
            _ => j += 1,
        }
    }
    chars.len()
}

fn token_end(chars: &[char], i: usize) -> usize {
    (i..chars.len())
        .find(|&j| chars[j] == ' ')
        .unwrap_or(chars.len())
}

fn ends_token(chars: &[char], i: usize) -> bool {
    chars.get(i).is_none_or(|&c| c == ' ' || c == '\t')
}

fn starts_with(chars: &[char], s: &str) -> bool {
    s.chars().enumerate().all(|(k, c)| chars.get(k) == Some(&c))
}

fn skip_spaces(chars: &[char], mut i: usize, out: &mut String) -> usize {
    while chars.get(i).is_some_and(|&c| c == ' ' || c == '\t') {
        out.push(chars[i]);
        i += 1;
    }
    i
}

fn span(class: &str, chars: &[char]) -> String {
    format!(
        r#"<span class="{class}">{}</span>"#,
        esc(&chars.iter().collect::<String>())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        let mut state = State::default();
        text.lines().map(|l| line_html(&mut state, l)).collect()
    }

    #[test]
    fn keys_values_and_comments() {
        assert_eq!(
            lines("name: CI # the workflow")[0],
            r#"<span class="yk">name</span>: <span class="str">CI</span> <span class="cm"># the workflow</span>"#
        );
        assert_eq!(
            lines("  - uses: actions/checkout@v4")[0],
            r#"  - <span class="yk">uses</span>: <span class="str">actions/checkout@v4</span>"#
        );
        assert_eq!(
            lines("timeout-minutes: 30")[0],
            r#"<span class="yk">timeout-minutes</span>: <span class="num">30</span>"#
        );
        assert_eq!(
            lines("fail-fast: false")[0],
            r#"<span class="yk">fail-fast</span>: <span class="kw">false</span>"#
        );
        assert_eq!(
            lines(r#""on": [push, "pull_request"]"#)[0],
            r#"<span class="yk">&quot;on&quot;</span>: [<span class="str">push</span>, <span class="str">&quot;pull_request&quot;</span>]"#
        );
        assert_eq!(
            lines("# only a comment")[0],
            r#"<span class="cm"># only a comment</span>"#
        );
        // A colon inside a value or URL is not a key.
        assert_eq!(
            lines("url: https://example.com")[0],
            r#"<span class="yk">url</span>: <span class="str">https://example.com</span>"#
        );
        assert_eq!(lines("- a#b")[0], r#"- <span class="str">a#b</span>"#);
        assert_eq!(
            lines("os: { linux: 1 }")[0],
            r#"<span class="yk">os</span>: { <span class="yk">linux</span>: <span class="num">1</span> }"#
        );
    }

    #[test]
    fn expressions_are_marked() {
        assert_eq!(
            lines("if: ${{ github.event_name == 'push' }}")[0],
            r#"<span class="yk">if</span>: <span class="expr">${{ github.event_name == 'push' }}</span>"#
        );
        assert_eq!(
            lines(r#"key: "v-${{ matrix.os }}""#)[0],
            r#"<span class="yk">key</span>: <span class="str">&quot;v-</span><span class="expr">${{ matrix.os }}</span><span class="str">&quot;</span>"#
        );
    }

    #[test]
    fn block_scalars() {
        let html = lines(
            "steps:\n  - run: |\n      cargo test # all\n\n      echo ${{ env.X }}\n  - name: done",
        );
        assert_eq!(
            html[1],
            r#"  - <span class="yk">run</span>: <span class="kw">|</span>"#
        );
        assert_eq!(html[2], "      cargo test # all");
        assert_eq!(html[3], "");
        assert_eq!(
            html[4],
            r#"      echo <span class="expr">${{ env.X }}</span>"#
        );
        assert_eq!(
            html[5],
            r#"  - <span class="yk">name</span>: <span class="str">done</span>"#
        );
        // `>-` folds; content must be deeper than the key.
        let html = lines("a: >-\n  text: not a key\nb: 1");
        assert_eq!(html[1], "  text: not a key");
        assert_eq!(
            html[2],
            r#"<span class="yk">b</span>: <span class="num">1</span>"#
        );
    }

    #[test]
    fn documents_anchors_and_tags() {
        assert_eq!(
            lines("--- # doc")[0],
            r#"<span class="kw">---</span> <span class="cm"># doc</span>"#
        );
        assert_eq!(
            lines("base: &base !!map")[0],
            r#"<span class="yk">base</span>: <span class="attr">&amp;base</span> <span class="attr">!!map</span>"#
        );
        assert_eq!(
            lines("<<: *base")[0],
            r#"<span class="yk">&lt;&lt;</span>: <span class="attr">*base</span>"#
        );
    }
}
