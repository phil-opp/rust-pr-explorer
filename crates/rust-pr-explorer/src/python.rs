//! Structural comparison of Python files: which functions and classes were
//! added, removed or changed, and how (header, docstring, body).
//!
//! A small tokenizer tracks strings (all quote and prefix forms), comments,
//! brackets and line continuations, so that logical lines are exact and a
//! `def` inside a string or inside brackets is never taken for a
//! definition. Blocks follow indentation, as in Python itself.

use crate::parts::{Def, compare_defs};
use model::{PartChange, Status};
use std::collections::BTreeMap;

/// A logical line: physical lines `start..=end` (0-based).
#[derive(Debug, Clone)]
struct Logical {
    start: usize,
    end: usize,
    indent: usize,
    /// Its tokens, for comparing: no comments, and whitespace kept only
    /// between two words (`a,b` and `a, b` are equal).
    norm: String,
    /// Byte offset in `norm` of the first `:` outside brackets, if any.
    colon: Option<usize>,
    /// The same for display: whitespace runs collapsed to one space.
    text: String,
    text_colon: Option<usize>,
}

/// Splits source into logical lines. Lines that hold only comments or
/// whitespace are not logical lines.
fn logical_lines(source: &str) -> Vec<Logical> {
    let lines: Vec<&str> = source.split('\n').collect();
    let mut out = Vec::new();
    // Tokenizer state across physical lines.
    let mut depth: i32 = 0;
    let mut string: Option<(char, bool)> = None; // (quote, triple)
    let mut cur: Option<Logical> = None;
    for (n, raw) in lines.iter().enumerate() {
        let line = raw.trim_end_matches('\r');
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        let starting = cur.is_none() && string.is_none();
        if starting {
            let indent = chars
                .iter()
                .take_while(|c| **c == ' ' || **c == '\t')
                .map(|c| if *c == '\t' { 8 } else { 1 })
                .sum();
            let rest: String = chars.iter().skip_while(|c| c.is_whitespace()).collect();
            if rest.is_empty() || rest.starts_with('#') {
                continue;
            }
            cur = Some(Logical {
                start: n,
                end: n,
                indent,
                norm: String::new(),
                colon: None,
                text: String::new(),
                text_colon: None,
            });
        }
        let Some(l) = cur.as_mut() else { continue };
        l.end = n;
        // Whitespace seen before the next token (a line break inside
        // brackets counts).
        let mut space = !l.norm.is_empty();
        let mut continued = false;
        // Adds a token character, with the whitespace before it.
        fn token(l: &mut Logical, space: &mut bool, c: char) {
            if *space {
                if !l.text.is_empty() && !l.text.ends_with(' ') {
                    l.text.push(' ');
                }
                if ends_with_word(&l.norm)
                    && (c.is_alphanumeric() || c == '_' || c == '"' || c == '\'')
                {
                    l.norm.push(' ');
                }
            }
            *space = false;
            l.norm.push(c);
            l.text.push(c);
        }
        while i < chars.len() {
            let c = chars[i];
            if let Some((q, triple)) = string {
                // Inside a string everything counts, as written.
                l.norm.push(c);
                l.text.push(c);
                if c == '\\' && i + 1 < chars.len() {
                    l.norm.push(chars[i + 1]);
                    l.text.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                if c == q {
                    if !triple {
                        string = None;
                    } else if i + 2 < chars.len() && chars[i + 1] == q && chars[i + 2] == q {
                        for _ in 0..2 {
                            l.norm.push(q);
                            l.text.push(q);
                        }
                        i += 2;
                        string = None;
                    }
                }
                i += 1;
                continue;
            }
            match c {
                '#' => break,
                '\\' if i + 1 == chars.len() => {
                    continued = true;
                    i += 1;
                    continue;
                }
                '\'' | '"' => {
                    let triple = i + 2 < chars.len() && chars[i + 1] == c && chars[i + 2] == c;
                    token(l, &mut space, c);
                    if triple {
                        for _ in 0..2 {
                            l.norm.push(c);
                            l.text.push(c);
                        }
                        i += 2;
                    }
                    string = Some((c, triple));
                }
                ' ' | '\t' => space = true,
                _ => {
                    match c {
                        '(' | '[' | '{' => depth += 1,
                        ')' | ']' | '}' => depth = (depth - 1).max(0),
                        ':' if depth == 0 && l.colon.is_none() => {
                            token(l, &mut space, c);
                            l.colon = Some(l.norm.len() - 1);
                            l.text_colon = Some(l.text.len() - 1);
                            i += 1;
                            continue;
                        }
                        _ => {}
                    }
                    token(l, &mut space, c);
                }
            }
            i += 1;
        }
        // A single-quoted string can't span lines; a triple-quoted one can.
        if let Some((_, false)) = string {
            string = None;
        }
        let open = string.is_some() || depth > 0 || continued;
        if open {
            if string.is_some() {
                l.norm.push('\n');
                l.text.push('\n');
            }
        } else {
            out.push(cur.take().unwrap());
        }
    }
    if let Some(l) = cur {
        out.push(l);
    }
    out
}

fn ends_with_word(s: &str) -> bool {
    s.chars()
        .last()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

fn header(norm: &str) -> Option<(&'static str, String)> {
    for (prefix, kind) in [
        ("async def ", "async def"),
        ("def ", "def"),
        ("class ", "class"),
    ] {
        if let Some(rest) = norm.strip_prefix(prefix) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                return Some((kind, name));
            }
        }
    }
    None
}

/// Whether a logical line is a docstring: a lone string literal.
fn docstring(norm: &str) -> bool {
    let s = norm.trim_start_matches(['r', 'R', 'u', 'U']);
    (s.starts_with('"') || s.starts_with('\'')) && (s.ends_with('"') || s.ends_with('\''))
}

fn outline(text: &str) -> (Vec<Def>, String) {
    let lines = logical_lines(text);
    let mut defs: Vec<Def> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    // Open definitions: (index in `defs`, indent, is a class).
    let mut stack: Vec<(usize, usize, bool)> = Vec::new();
    let mut module = String::new();
    let mut decorators: Vec<&Logical> = Vec::new();
    for (k, l) in lines.iter().enumerate() {
        while stack.last().is_some_and(|&(_, ind, _)| l.indent <= ind) {
            stack.pop();
        }
        if l.norm.starts_with('@') {
            decorators.push(l);
            continue;
        }
        if let Some((kind, name)) = header(&l.norm) {
            let colon = l.colon.unwrap_or(l.norm.len());
            let header_of = |text: &str, end: Option<usize>, ds: &dyn Fn(&Logical) -> String| {
                decorators
                    .iter()
                    .map(|d| ds(d))
                    .chain([text[..end.unwrap_or(text.len())].to_string()])
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let sig = header_of(&l.text, l.text_colon, &|d| d.text.clone());
            let sig_norm = header_of(&l.norm, l.colon, &|d| d.norm.clone());
            let first = decorators.first().map_or(l.start, |d| d.start);
            decorators.clear();
            // Block: following logical lines indented deeper.
            let last = lines[k + 1..]
                .iter()
                .take_while(|n| n.indent > l.indent)
                .last()
                .map_or(l.end, |n| n.end);
            // Qualified name, as `__qualname__`.
            let qual = match stack.last() {
                Some(&(p, _, true)) => format!("{}.{name}", defs[p].name),
                Some(&(p, _, false)) => format!("{}.<locals>.{name}", defs[p].name),
                None => name.clone(),
            };
            let n = counts.entry(qual.clone()).or_default();
            *n += 1;
            let key = if *n == 1 {
                qual.clone()
            } else {
                format!("{qual}#{n}")
            };
            let inline_body = l.norm[colon..].trim_start_matches(':').trim().to_string();
            // Docstring: the first statement of the block.
            let doc = lines[k + 1..]
                .first()
                .filter(|n| n.indent > l.indent && n.start <= last && docstring(&n.norm))
                .map(|n| n.norm.clone());
            if let Some(&(p, _, _)) = stack.last() {
                defs[p].nested.push((first, last));
            }
            defs.push(Def {
                key,
                name: qual,
                kind: kind.to_string(),
                sig,
                sig_norm,
                doc,
                body: inline_body,
                first,
                last,
                nested: Vec::new(),
            });
            stack.push((defs.len() - 1, l.indent, kind == "class"));
            continue;
        }
        for d in decorators.drain(..) {
            module.push_str(&d.norm);
            module.push('\n');
        }
        // A statement belongs to the innermost open definition.
        match stack.last() {
            Some(&(p, ind, _)) => {
                let doc =
                    defs[p].doc.as_ref().is_some_and(|d| *d == l.norm) && defs[p].body.is_empty();
                if !doc {
                    let b = &mut defs[p].body;
                    b.push_str(&format!("{}|{}\n", l.indent.saturating_sub(ind), l.norm));
                } else {
                    // Mark the docstring as seen, so a later identical
                    // string is body.
                    defs[p].body.push('\u{0}');
                }
            }
            None => {
                module.push_str(&format!("{}|{}\n", l.indent, l.norm));
            }
        }
    }
    for d in &mut defs {
        d.body = d.body.replace('\u{0}', "");
    }
    (defs, module)
}

/// Compares two versions of a Python file. Returns the definitions that
/// changed, in the order of the head version (removed ones last), plus a
/// `<module>` entry if top-level code outside definitions changed.
pub fn compare(old: &str, new: &str) -> Vec<PartChange> {
    let (a, a_mod) = outline(old);
    let (b, b_mod) = outline(new);
    let mut out = Vec::new();
    if a_mod != b_mod {
        out.push(PartChange {
            name: "<module>".into(),
            kind: "module".into(),
            status: Status::Changed,
            ..Default::default()
        });
    }
    out.extend(compare_defs(&a, &b, old, new));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = r#"import os

TEMPLATE = """
def not_a_function():
    pass
"""

def greet(name):
    """Say hello."""
    return f"hello {name}"

class Store:
    def get(self, key):
        return self.data[key]

    def put(self, key, value):  # store it
        self.data[key] = value

def gone():
    pass
"#;

    const NEW: &str = r#"import os

TEMPLATE = """
def not_a_function():
    pass
"""

def greet(name, punctuation="!"):
    """Say hello."""
    return f"hello {name}"

class Store:
    def get(self, key):
        return self.data.get(key)

    def put(self, key, value):    # keep it
        self.data[key] = value

    @property
    def size(self):
        def count(
            items,
        ):
            return len(items)
        return count(self.data)
"#;

    fn find<'a>(c: &'a [PartChange], name: &str) -> &'a PartChange {
        c.iter()
            .find(|x| x.name == name)
            .unwrap_or_else(|| panic!("{name}: {c:#?}"))
    }

    #[test]
    fn definitions_are_compared() {
        let c = compare(OLD, NEW);
        let names: Vec<&str> = c.iter().map(|x| x.name.as_str()).collect();
        // `def` inside a string is not a definition; unchanged `Store` is
        // not listed; formatting and comment changes count as cosmetic.
        assert_eq!(
            names,
            [
                "greet",
                "Store.get",
                "Store.put",
                "Store.size",
                "Store.size.<locals>.count",
                "gone"
            ]
        );
        let greet = find(&c, "greet");
        assert_eq!(greet.status, Status::Changed);
        assert_eq!(greet.old_sig.as_deref(), Some("def greet(name)"));
        assert_eq!(greet.sig, "def greet(name, punctuation=\"!\")");
        assert!(!greet.doc_changed && !greet.body_changed);
        let get = find(&c, "Store.get");
        assert!(get.body_changed && get.old_sig.is_none());
        let put = find(&c, "Store.put");
        assert!(put.cosmetic && put.status == Status::Unchanged);
        let size = find(&c, "Store.size");
        assert_eq!(size.status, Status::Added);
        assert_eq!(size.sig, "@property\ndef size(self)");
        assert_eq!(find(&c, "gone").status, Status::Removed);
    }

    #[test]
    fn docstrings_and_module_code() {
        let c = compare(
            "X = 1\ndef f():\n    'Old doc.'\n    return 1\n",
            "X = 2\ndef f():\n    'New doc.'\n    return 1\n",
        );
        assert_eq!(c[0].name, "<module>");
        let f = find(&c, "f");
        assert!(f.doc_changed && !f.body_changed);
    }

    #[test]
    fn brackets_and_continuations_make_one_logical_line() {
        let l = logical_lines("x = [\n  1,  # one\n  2,\n]\ny = 1 + \\\n  2\n");
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].norm, "x=[1,2,]");
        assert_eq!(l[0].text, "x = [ 1, 2, ]");
        assert_eq!((l[1].start, l[1].end), (4, 5));
    }

    #[test]
    fn formatting_is_not_a_change() {
        let c = compare("def f(a,b): return a+b\n", "def f(a, b):  return a + b\n");
        assert!(c.is_empty() || c.iter().all(|x| x.cosmetic), "{c:#?}");
        let c = compare(
            "def f(a,b):\n    return a+b\n",
            "def f(a, b):\n    return a + b  # sum\n",
        );
        assert!(c.iter().all(|x| x.cosmetic), "{c:#?}");
    }
}
