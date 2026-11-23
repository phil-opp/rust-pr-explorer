//! CSS files compared by selector. A stylesheet is parsed into rules; the
//! declarations of every rule for the same selector (in the same `@media`,
//! `@supports`, … context) are merged into the properties that apply,
//! following the cascade within the file: a later declaration wins, unless
//! an earlier one is `!important` and it isn't. The two versions are then
//! compared selector by selector, property by property.
//!
//! The parser is forgiving, as browsers are: it follows strings, comments,
//! brackets and nesting, and never fails. Native CSS nesting (`&`) is
//! resolved into full selectors.

use model::{PropChange, PropValue, Status, StyleChange};
use std::collections::BTreeMap;

/// One declaration as written: `color: red !important`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Decl {
    name: String,
    /// Normalized value, with `!important` if given.
    value: String,
    important: bool,
    /// 1-based.
    line: usize,
}

/// What a stylesheet says about one selector in one context.
#[derive(Debug, Default)]
struct Entry {
    /// First lines of the rules that mention it, in order.
    rules: Vec<usize>,
    decls: Vec<Decl>,
    /// A statement at-rule (`@import …;`) rather than a rule.
    statement: bool,
}

/// Key: (context, selector).
type Sheet = BTreeMap<(Vec<String>, String), Entry>;

/// The parsed stylesheet, with the order in which keys first appear.
struct Parsed {
    sheet: Sheet,
    order: Vec<(Vec<String>, String)>,
}

/// The source with comments replaced by spaces (newlines are kept, so
/// lines don't move), as chars.
fn blank_comments(text: &str) -> Vec<char> {
    let mut out: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < out.len() {
        let c = out[i];
        match quote {
            Some(q) => {
                if c == '\\' {
                    i += 1;
                } else if c == q || c == '\n' {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '/' && out.get(i + 1) == Some(&'*') => {
                let mut j = i;
                while j < out.len() && !(out[j] == '*' && out.get(j + 1) == Some(&'/') && j > i) {
                    if out[j] != '\n' {
                        out[j] = ' ';
                    }
                    j += 1;
                }
                for k in j..(j + 2).min(out.len()) {
                    out[k] = ' ';
                }
                i = j + 1;
            }
            None => {}
        }
        i += 1;
    }
    out
}

/// Collapses whitespace, removes it inside brackets, and puts exactly one
/// space after commas and (in selectors) around combinators.
fn normalize(s: &str, selector: bool) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut depth = 0usize;
    let mut space = false;
    let tight_after = |c: char| matches!(c, '(' | '[');
    for c in s.trim().chars() {
        if let Some(q) = quote {
            out.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        if c.is_whitespace() {
            space = true;
            continue;
        }
        let combinator = selector && depth == 0 && matches!(c, '>' | '+' | '~');
        if space {
            let last = out.chars().last();
            // After a combinator, which brings its own space.
            let skip = last.is_none_or(|l| tight_after(l) || l == ' ')
                || matches!(c, ')' | ']' | ',')
                || combinator;
            if !skip {
                out.push(' ');
            }
            space = false;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if combinator {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push(c);
            out.push(' ');
            space = false;
            continue;
        }
        out.push(c);
        if c == ',' {
            out.push(' ');
        }
    }
    // A combinator directly before the end (`a >`) leaves a space.
    out.trim_end().to_string()
}

/// Splits at commas outside brackets and strings.
fn split_top(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' => depth += 1,
                ')' | ']' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    out.push(std::mem::take(&mut cur));
                    continue;
                }
                _ => {}
            },
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter()
        .map(|s| normalize(&s, true))
        .filter(|s| !s.is_empty())
        .collect()
}

/// The selectors of a nested rule, with `&` replaced by each parent (or
/// the parent prepended as an ancestor where there is no `&`).
fn nest(parents: &[String], own: &[String]) -> Vec<String> {
    if parents.is_empty() {
        return own.to_vec();
    }
    let mut out = Vec::new();
    for p in parents {
        for s in own {
            out.push(if s.contains('&') {
                s.replace('&', p)
            } else {
                format!("{p} {s}")
            });
        }
    }
    out
}

struct Parser<'a> {
    src: &'a [char],
    pos: usize,
    line: usize,
    out: Parsed,
}

impl Parser<'_> {
    fn bump(&mut self) -> Option<char> {
        let c = *self.src.get(self.pos)?;
        if c == '\n' {
            self.line += 1;
        }
        self.pos += 1;
        Some(c)
    }

    /// Reads up to `{`, `;` or `}` outside strings and brackets. Returns
    /// the text, the line where its first non-blank character is, and the
    /// terminator (consumed, except `}`), or `None` at the end.
    fn prelude(&mut self) -> (String, usize, Option<char>) {
        let mut text = String::new();
        let mut start = None;
        let mut depth = 0usize;
        let mut quote: Option<char> = None;
        while let Some(&c) = self.src.get(self.pos) {
            if start.is_none() && !c.is_whitespace() {
                start = Some(self.line);
            }
            if let Some(q) = quote {
                self.bump();
                text.push(c);
                if c == '\\' {
                    if let Some(n) = self.bump() {
                        text.push(n);
                    }
                } else if c == q || c == '\n' {
                    quote = None;
                }
                continue;
            }
            match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' => depth += 1,
                ')' | ']' => depth = depth.saturating_sub(1),
                '{' | ';' if depth == 0 => {
                    self.bump();
                    return (text, start.unwrap_or(self.line), Some(c));
                }
                '}' => return (text, start.unwrap_or(self.line), Some('}')),
                _ => {}
            }
            self.bump();
            text.push(c);
        }
        (text, start.unwrap_or(self.line), None)
    }

    /// The entry of a selector, for a rule (or statement) that starts on
    /// `line`.
    fn rule(&mut self, ctx: &[String], sel: &str, line: usize) -> &mut Entry {
        let key = (ctx.to_vec(), sel.to_string());
        if !self.out.sheet.contains_key(&key) {
            self.out.order.push(key.clone());
        }
        let e = self.out.sheet.entry(key).or_default();
        if e.rules.last() != Some(&line) {
            e.rules.push(line);
        }
        e
    }

    /// Parses the items of a block (or of the whole sheet) up to its `}`.
    /// Declarations go to `owners` (the selectors of the enclosing rule).
    fn block(&mut self, ctx: &[String], owners: &[String]) {
        loop {
            let (text, line, end) = self.prelude();
            let t = text.trim();
            match end {
                Some('{') => {
                    if let Some(at) = t.strip_prefix('@') {
                        let at = normalize(at, false);
                        let name = at.split([' ', '(']).next().unwrap_or("").to_lowercase();
                        let label = format!("@{at}");
                        // Blocks of declarations: `@font-face`, `@page`,
                        // `@property --x`, `@counter-style x`, ….
                        let holds_decls = matches!(
                            name.as_str(),
                            "font-face"
                                | "page"
                                | "property"
                                | "counter-style"
                                | "font-palette-values"
                                | "viewport"
                                | "view-transition"
                        );
                        if holds_decls && owners.is_empty() {
                            self.rule(ctx, &label, line);
                            self.block(ctx, std::slice::from_ref(&label));
                        } else {
                            let mut inner = ctx.to_vec();
                            inner.push(label);
                            self.block(&inner, owners);
                        }
                    } else {
                        let sels = nest(owners, &split_top(t));
                        for s in &sels {
                            self.rule(ctx, s, line);
                        }
                        self.block(ctx, &sels);
                    }
                    // The block's `}`.
                    self.bump();
                }
                Some(';') | Some('}') | None => {
                    if !t.is_empty() {
                        self.statement(ctx, owners, t, line);
                    }
                    if end != Some(';') {
                        return;
                    }
                }
                Some(_) => unreachable!(),
            }
        }
    }

    fn statement(&mut self, ctx: &[String], owners: &[String], t: &str, line: usize) {
        if t.starts_with('@') {
            // `@import`, `@charset`, `@layer a, b;`, `@apply` …
            let label = normalize(t, false);
            let e = self.rule(ctx, &label, line);
            e.statement = true;
            return;
        }
        let Some((name, value)) = t.split_once(':') else {
            return;
        };
        let name = name.trim();
        if name.is_empty() || owners.is_empty() {
            return;
        }
        let name = if name.starts_with("--") {
            name.to_string()
        } else {
            name.to_lowercase()
        };
        let mut value = normalize(value, false);
        let mut important = false;
        let lower = value.to_lowercase();
        if let Some(at) = lower.rfind('!')
            && lower[at + 1..].trim() == "important"
        {
            value = format!("{} !important", value[..at].trim_end());
            important = true;
        }
        for o in owners {
            // Its rule was entered when its block started.
            let key = (ctx.to_vec(), o.clone());
            let Some(e) = self.out.sheet.get_mut(&key) else {
                continue;
            };
            e.decls.push(Decl {
                name: name.clone(),
                value: value.clone(),
                important,
                line,
            });
        }
    }
}

fn parse(text: &str) -> Parsed {
    let src = blank_comments(text);
    let mut p = Parser {
        src: &src,
        pos: 0,
        line: 1,
        out: Parsed {
            sheet: BTreeMap::new(),
            order: Vec::new(),
        },
    };
    // A stray `}` at the top level ends nothing: skip it and go on.
    while p.pos < src.len() {
        p.block(&[], &[]);
        p.bump();
    }
    p.out
}

/// By property: the declaration that applies, and all of them in order.
type Props = BTreeMap<String, (Decl, Vec<Decl>)>;

/// The declaration that applies for each property, in the order the
/// properties first appear, and all declarations of each.
fn effective(e: &Entry) -> (Vec<String>, Props) {
    let mut order = Vec::new();
    let mut out = Props::new();
    for d in &e.decls {
        match out.get_mut(&d.name) {
            Some((win, all)) => {
                if d.important || !win.important {
                    *win = d.clone();
                }
                all.push(d.clone());
            }
            None => {
                order.push(d.name.clone());
                out.insert(d.name.clone(), (d.clone(), vec![d.clone()]));
            }
        }
    }
    (order, out)
}

/// Why a property's declarations changed while the value that applies
/// didn't.
fn no_effect_note(win: &Decl, old: &[Decl], new: &[Decl]) -> String {
    let values = |l: &[Decl]| -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for d in l {
            *m.entry(d.value.clone()).or_default() += 1;
        }
        m
    };
    let (ov, nv) = (values(old), values(new));
    let added: Vec<&Decl> = new
        .iter()
        .filter(|d| nv.get(&d.value) > ov.get(&d.value))
        .collect();
    if let Some(a) = added.iter().find(|a| a.value != win.value) {
        return format!(
            "Line {} sets `{}`, but line {} overrides it: no effect.",
            a.line, a.value, win.line
        );
    }
    if let Some(a) = added.iter().find(|a| a.line != win.line) {
        return format!(
            "Line {} repeats the value of line {}: no effect.",
            a.line, win.line
        );
    }
    if let Some(a) = added.first() {
        let other = new.iter().find(|d| d.line != a.line && d.value == a.value);
        return match other {
            Some(o) => format!(
                "Line {} repeats the value of line {}: no effect.",
                a.line, o.line
            ),
            None => "Declarations changed; the value that applies is the same.".into(),
        };
    }
    "A declaration was removed; the value that applies is the same.".into()
}

fn changes(a: &Parsed, b: &Parsed) -> Vec<(usize, StyleChange)> {
    let mut out = Vec::new();
    let keys = b
        .order
        .iter()
        .chain(a.order.iter().filter(|k| !b.sheet.contains_key(*k)));
    for key in keys {
        let (old, new) = (a.sheet.get(key), b.sheet.get(key));
        let (ctx, sel) = key;
        let mut change = StyleChange {
            selectors: vec![sel.clone()],
            context: ctx.clone(),
            status: match (old, new) {
                (None, _) => Status::Added,
                (_, None) => Status::Removed,
                _ => Status::Changed,
            },
            statement: new.or(old).is_some_and(|e| e.statement),
            rules: new.or(old).map(|e| e.rules.clone()).unwrap_or_default(),
            ..Default::default()
        };
        let empty = Entry::default();
        let (oo, oe) = effective(old.unwrap_or(&empty));
        let (no, ne) = effective(new.unwrap_or(&empty));
        let names: Vec<&String> = no
            .iter()
            .chain(oo.iter().filter(|n| !ne.contains_key(*n)))
            .collect();
        for name in names {
            let (o, n) = (oe.get(name), ne.get(name));
            let value = |d: &Decl| PropValue {
                value: d.value.clone(),
                line: d.line,
            };
            match (o, n) {
                (Some((ow, oall)), Some((nw, nall))) if ow.value == nw.value => {
                    let same = |x: &[Decl], y: &[Decl]| {
                        x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.value == q.value)
                    };
                    if same(oall, nall) {
                        change.unchanged.push(PropChange {
                            name: name.clone(),
                            old: None,
                            new: Some(value(nw)),
                            note: None,
                        });
                    } else {
                        change.props.push(PropChange {
                            name: name.clone(),
                            old: Some(value(ow)),
                            new: Some(value(nw)),
                            note: Some(no_effect_note(nw, oall, nall)),
                        });
                    }
                }
                _ => change.props.push(PropChange {
                    name: name.clone(),
                    old: o.map(|(w, _)| value(w)),
                    new: n.map(|(w, _)| value(w)),
                    note: None,
                }),
            }
        }
        if change.status == Status::Changed && change.props.is_empty() {
            continue;
        }
        let line = change.rules.first().copied().unwrap_or(0);
        let order = if new.is_some() { line } else { usize::MAX };
        out.push((order, change));
    }
    out.sort_by_key(|c| c.0);
    out
}

/// What changed in a stylesheet, selector by selector. Selectors with the
/// same changes in the same context (from one rule for `.a, .b`) share
/// one entry. In the order of the new version, removed ones last.
pub fn compare(old: &str, new: &str) -> Vec<StyleChange> {
    let (a, b) = (parse(old), parse(new));
    let mut out: Vec<StyleChange> = Vec::new();
    for (_, c) in changes(&a, &b) {
        let same = |x: &StyleChange| {
            x.context == c.context
                && x.status == c.status
                && x.statement == c.statement
                && x.rules == c.rules
                && x.props == c.props
                && x.unchanged == c.unchanged
        };
        match out.iter_mut().find(|x| same(x)) {
            Some(x) => x.selectors.extend(c.selectors),
            None => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The selectors a stylesheet defines rules for, with their context.
    fn selectors(text: &str) -> BTreeSet<String> {
        parse(text)
            .sheet
            .keys()
            .map(|(ctx, s)| {
                if ctx.is_empty() {
                    s.clone()
                } else {
                    format!("{} {s}", ctx.join(" "))
                }
            })
            .collect()
    }

    fn rows(c: &StyleChange) -> Vec<(String, Option<String>, Option<String>)> {
        c.props
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    p.old.as_ref().map(|v| v.value.clone()),
                    p.new.as_ref().map(|v| v.value.clone()),
                )
            })
            .collect()
    }

    fn s(x: &str) -> Option<String> {
        Some(x.to_string())
    }

    #[test]
    fn parses_rules_comments_strings_and_nesting() {
        let css = "/* a { b } */\n@import url(\"x.css\");\n.a > .b,\n.c+.d { content: \"}\"; color: red }\n@media (max-width: 600px) {\n  .a { margin: 0 }\n}\n.p {\n  color: blue;\n  &:hover { color: red; }\n  .q & { color: green; }\n  span { color: gray; }\n}\n@font-face { font-family: X; src: url(x.woff); }\n@keyframes spin { from { transform: none } to { transform: rotate(1turn) } }\n";
        let got = selectors(css);
        let want: BTreeSet<String> = [
            "@import url(\"x.css\")",
            ".a > .b",
            ".c + .d",
            "@media (max-width: 600px) .a",
            ".p",
            ".p:hover",
            ".q .p",
            ".p span",
            "@font-face",
            "@keyframes spin from",
            "@keyframes spin to",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(got, want);
        let p = parse(css);
        let ab = &p.sheet[&(vec![], ".c + .d".to_string())];
        assert_eq!(ab.decls[0].value, "\"}\"");
        assert_eq!(ab.decls[1].line, 4);
        assert_eq!(p.sheet[&(vec![], ".p".to_string())].rules, [8]);
    }

    #[test]
    fn merges_rules_of_a_selector() {
        let old = "#bar { display: none; gap: 8px; }\n.x { color: red }\n";
        let new = "#bar { display: none; gap: 8px; }\n.x { color: red }\n#bar { display: flex; font-weight: 600; }\n";
        let c = compare(old, new);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].selectors, ["#bar"]);
        assert_eq!(c[0].status, Status::Changed);
        assert_eq!(c[0].rules, [1, 3]);
        assert_eq!(
            rows(&c[0]),
            [
                ("display".into(), s("none"), s("flex")),
                ("font-weight".into(), None, s("600")),
            ]
        );
        let d = &c[0].props[0];
        assert_eq!(
            (d.old.as_ref().unwrap().line, d.new.as_ref().unwrap().line),
            (1, 3)
        );
        assert_eq!(c[0].unchanged.len(), 1);
        assert_eq!(c[0].unchanged[0].name, "gap");
    }

    #[test]
    fn important_and_overridden_declarations() {
        let old = ".a { color: red !important; }\n.a { color: blue; }\n.b { margin: 0 }\n";
        let new = ".a { color: red !important; }\n.a { color: green; }\n.b { margin: 1px; }\n.b { margin: 0 }\n";
        let c = compare(old, new);
        assert_eq!(c.len(), 2);
        // `.a`: red stays, because it is important.
        assert_eq!(
            rows(&c[0]),
            [("color".into(), s("red !important"), s("red !important"))]
        );
        assert!(
            c[0].props[0]
                .note
                .as_deref()
                .unwrap()
                .contains("Line 2 sets `green`")
        );
        // `.b`: the added `1px` is overridden right after.
        assert!(
            c[1].props[0]
                .note
                .as_deref()
                .unwrap()
                .contains("line 4 overrides it")
        );
    }

    #[test]
    fn groups_selectors_and_ignores_formatting() {
        let old = "h1, h2 { margin: 0 }\n.k{color:red}\n";
        let new = "/* Headings. */\nh1, h2 { margin: 0; padding: 0 }\n.k {\n  color: red;\n}\n@import \"x.css\";\n";
        let c = compare(old, new);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].selectors, ["h1", "h2"]);
        assert_eq!(rows(&c[0]), [("padding".into(), None, s("0"))]);
        assert!(c[1].statement && c[1].status == Status::Added);
    }

    #[test]
    fn added_and_removed_selectors() {
        let old = ".gone { color: red; }\n";
        let new = "@media print { .new { display: none } }\n";
        let c = compare(old, new);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].status, Status::Added);
        assert_eq!(c[0].context, ["@media print"]);
        assert_eq!(c[1].status, Status::Removed);
        assert_eq!(rows(&c[1]), [("color".into(), s("red"), None)]);
    }
}
