//! Structural comparison of JavaScript files: which functions, classes,
//! methods, top-level variables and test blocks were added, removed or
//! changed, and how (header, JSDoc comment, body).
//!
//! A tokenizer follows strings, template literals (with their `${…}`
//! expressions), regular expression literals and comments, so that braces
//! and keywords inside them never count. Statements end at `;`, or at a
//! line break where the statement can end (automatic semicolon
//! insertion, approximated).
//!
//! Parts are:
//! - function declarations, and `const f = (…) => …` or `= function …`;
//! - classes, and their methods, getters, setters, fields and static
//!   blocks (`Store.get`);
//! - other top-level `const`/`let`/`var` declarations;
//! - top-level calls whose first argument is a string, as test frameworks
//!   write them (`describe("parser", () => { it("parses", …) })`), with
//!   the calls of that kind inside their callback as parts of their own
//!   (kind `suite` or `test` for the usual names, else `call`).
//!
//! Everything else at the top level (imports, other statements) is
//! compared as a whole.

use crate::parts::{Def, compare_defs};
use model::{PartChange, Status};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum K {
    Ident,
    Punct,
    /// Strings, template literals, regular expressions.
    Literal,
    Num,
}

#[derive(Debug, Clone)]
struct Tok {
    kind: K,
    text: String,
    /// 0-based line of its first and last char.
    line: usize,
    end_line: usize,
    /// Char offsets in the source.
    start: usize,
    end: usize,
}

/// A `/** … */` comment.
#[derive(Debug, Clone)]
struct Doc {
    text: String,
    line: usize,
    end_line: usize,
}

const PUNCTS: &[&str] = &[
    ">>>=", "===", "!==", "**=", "<<=", ">>=", ">>>", "...", "&&=", "||=", "??=", "=>", "==", "!=",
    "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
    "**", "<<", ">>",
];

/// Keywords after which a `/` starts a regular expression.
const BEFORE_REGEX: &[&str] = &[
    "return",
    "typeof",
    "instanceof",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "case",
    "do",
    "else",
    "yield",
    "await",
];

struct Lexer {
    chars: Vec<char>,
    /// Line of each char.
    lines: Vec<usize>,
}

impl Lexer {
    fn new(text: &str) -> Lexer {
        let chars: Vec<char> = text.chars().collect();
        let mut lines = Vec::with_capacity(chars.len() + 1);
        let mut l = 0;
        for c in &chars {
            lines.push(l);
            if *c == '\n' {
                l += 1;
            }
        }
        lines.push(l);
        Lexer { chars, lines }
    }

    fn at(&self, i: usize) -> char {
        self.chars.get(i).copied().unwrap_or('\0')
    }

    /// The end (exclusive) of a quoted string starting at `i`.
    fn string_end(&self, i: usize) -> usize {
        let q = self.chars[i];
        let mut j = i + 1;
        while j < self.chars.len() {
            match self.chars[j] {
                '\\' => j += 1,
                c if c == q => return j + 1,
                '\n' => return j,
                _ => {}
            }
            j += 1;
        }
        // A backslash as the last character steps past the end.
        j.min(self.chars.len())
    }

    /// The end (exclusive) of a template literal starting at `i`.
    fn template_end(&self, i: usize) -> usize {
        let mut j = i + 1;
        while j < self.chars.len() {
            match self.chars[j] {
                '\\' => j += 1,
                '`' => return j + 1,
                '$' if self.at(j + 1) == '{' => {
                    j = self.expr_end(j + 2);
                    continue;
                }
                _ => {}
            }
            j += 1;
        }
        // A backslash as the last character steps past the end.
        j.min(self.chars.len())
    }

    /// The end (after its `}`) of a `${…}` expression starting at `i`.
    fn expr_end(&self, i: usize) -> usize {
        let mut depth = 0;
        let mut j = i;
        while j < self.chars.len() {
            match self.chars[j] {
                '"' | '\'' => {
                    j = self.string_end(j);
                    continue;
                }
                '`' => {
                    j = self.template_end(j);
                    continue;
                }
                '/' if self.at(j + 1) == '/' => {
                    while j < self.chars.len() && self.chars[j] != '\n' {
                        j += 1;
                    }
                    continue;
                }
                '/' if self.at(j + 1) == '*' => {
                    j = self.block_comment_end(j);
                    continue;
                }
                '{' => depth += 1,
                '}' if depth == 0 => return j + 1,
                '}' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        j
    }

    fn block_comment_end(&self, i: usize) -> usize {
        let mut j = i + 2;
        while j < self.chars.len() && !(self.chars[j] == '*' && self.at(j + 1) == '/') {
            j += 1;
        }
        (j + 2).min(self.chars.len())
    }

    /// The end of a regular expression literal starting at `i`, or `None`
    /// if the line ends first.
    fn regex_end(&self, i: usize) -> Option<usize> {
        let mut j = i + 1;
        let mut class = false;
        while j < self.chars.len() {
            match self.chars[j] {
                '\\' => j += 1,
                '\n' => return None,
                '[' => class = true,
                ']' => class = false,
                '/' if !class => {
                    j += 1;
                    while j < self.chars.len() && self.chars[j].is_alphanumeric() {
                        j += 1;
                    }
                    return Some(j);
                }
                _ => {}
            }
            j += 1;
        }
        None
    }

    fn tokens(&self) -> (Vec<Tok>, Vec<Doc>) {
        let mut toks: Vec<Tok> = Vec::new();
        let mut docs = Vec::new();
        let n = self.chars.len();
        let mut i = 0;
        // A `#!` line at the start.
        if self.at(0) == '#' && self.at(1) == '!' {
            while i < n && self.chars[i] != '\n' {
                i += 1;
            }
        }
        while i < n {
            let c = self.chars[i];
            if c.is_whitespace() {
                i += 1;
                continue;
            }
            let start = i;
            let kind = if c == '/' && self.at(i + 1) == '/' {
                while i < n && self.chars[i] != '\n' {
                    i += 1;
                }
                continue;
            } else if c == '/' && self.at(i + 1) == '*' {
                i = self.block_comment_end(i);
                if self.at(start + 2) == '*' && self.at(start + 3) != '/' {
                    docs.push(Doc {
                        text: self.chars[start..i]
                            .iter()
                            .collect::<String>()
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" "),
                        line: self.lines[start],
                        end_line: self.lines[i.saturating_sub(1)],
                    });
                }
                continue;
            } else if c == '"' || c == '\'' {
                i = self.string_end(i);
                K::Literal
            } else if c == '`' {
                i = self.template_end(i);
                K::Literal
            } else if c == '/'
                && regex_allowed(toks.last())
                && let Some(end) = self.regex_end(i)
            {
                i = end;
                K::Literal
            } else if c.is_ascii_digit() || (c == '.' && self.at(i + 1).is_ascii_digit()) {
                while i < n
                    && (self.chars[i].is_alphanumeric() || matches!(self.chars[i], '.' | '_'))
                {
                    i += 1;
                }
                K::Num
            } else if c.is_alphabetic() || c == '_' || c == '$' || c == '#' || c == '@' {
                i += 1;
                while i < n
                    && (self.chars[i].is_alphanumeric()
                        || self.chars[i] == '_'
                        || self.chars[i] == '$')
                {
                    i += 1;
                }
                K::Ident
            } else {
                let rest: String = self.chars[i..(i + 4).min(n)].iter().collect();
                let len = PUNCTS
                    .iter()
                    .find(|p| rest.starts_with(*p))
                    .map_or(1, |p| p.len());
                i += len;
                K::Punct
            };
            toks.push(Tok {
                kind,
                text: self.chars[start..i].iter().collect(),
                line: self.lines[start],
                end_line: self.lines[i.saturating_sub(1).max(start)],
                start,
                end: i,
            });
        }
        (toks, docs)
    }
}

fn regex_allowed(prev: Option<&Tok>) -> bool {
    match prev {
        None => true,
        Some(t) => match t.kind {
            K::Punct => !matches!(t.text.as_str(), ")" | "]" | "}" | "++" | "--"),
            K::Ident => BEFORE_REGEX.contains(&t.text.as_str()),
            _ => false,
        },
    }
}

/// Tokens as text: a space only between two words.
fn norm(toks: &[Tok]) -> String {
    let mut out = String::new();
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '#');
    for t in toks {
        if out.chars().last().is_some_and(word) && t.text.chars().next().is_some_and(word) {
            out.push(' ');
        }
        out.push_str(&t.text);
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Top,
    Class,
    /// The callback of a test-style call: only calls of that kind are
    /// parts.
    Call,
}

struct Outline<'a> {
    toks: &'a [Tok],
    docs: &'a [Doc],
    chars: &'a [char],
    /// Index of the matching bracket of each opening bracket.
    close: Vec<usize>,
    defs: Vec<Def>,
    /// Token ranges of each def's header and whole extent.
    spans: Vec<(usize, usize, usize)>,
    counts: BTreeMap<String, usize>,
    /// Top-level tokens outside every part.
    module: Vec<Tok>,
}

impl Outline<'_> {
    fn is(&self, i: usize, text: &str) -> bool {
        self.toks.get(i).is_some_and(|t| t.text == text)
    }

    fn ident(&self, i: usize) -> bool {
        self.toks.get(i).is_some_and(|t| t.kind == K::Ident)
    }

    /// The source between two tokens (inclusive), as written.
    fn source(&self, from: usize, to: usize) -> String {
        let (a, b) = (self.toks[from].start, self.toks[to].end);
        self.chars[a..b]
            .iter()
            .collect::<String>()
            .trim()
            .to_string()
    }

    /// The last token of the statement starting at `i`, before `to`.
    fn stmt_end(&self, i: usize, to: usize) -> usize {
        let mut j = i;
        while j < to {
            let t = &self.toks[j];
            if matches!(t.text.as_str(), "(" | "[" | "{") && t.kind == K::Punct {
                j = self.close[j];
            }
            if j >= to {
                return to - 1;
            }
            if self.is(j, ";") {
                return j;
            }
            let Some(next) = self.toks.get(j + 1).filter(|_| j + 1 < to) else {
                return j;
            };
            let cur = &self.toks[j];
            if next.line > cur.end_line && ends_expr(cur) && starts_stmt(next) {
                return j;
            }
            j += 1;
        }
        to.saturating_sub(1)
    }

    /// The first token at bracket depth 0 in `from..to` with this text.
    fn find(&self, from: usize, to: usize, text: &str) -> Option<usize> {
        let mut j = from;
        while j < to {
            if self.is(j, text) {
                return Some(j);
            }
            if matches!(self.toks[j].text.as_str(), "(" | "[" | "{")
                && self.toks[j].kind == K::Punct
            {
                j = self.close[j];
            }
            j += 1;
        }
        None
    }

    /// Whether tokens from `i` start a function value: `function`,
    /// `async function`, `(…) =>`, `x =>`, `async (…) =>`.
    fn function_value(&self, i: usize) -> bool {
        let i = if self.is(i, "async") && !self.is(i + 1, "=>") {
            i + 1
        } else {
            i
        };
        if self.is(i, "function") {
            return true;
        }
        if self.is(i, "(") {
            return self.is(self.close[i] + 1, "=>");
        }
        self.ident(i) && self.is(i + 1, "=>")
    }

    /// The `{` of a function body in `from..=to`, if any: after `=>` or
    /// after a parameter list.
    fn body_brace(&self, from: usize, to: usize) -> Option<usize> {
        let arrow = self.find(from, to + 1, "=>");
        let start = match arrow {
            Some(a) => a + 1,
            None => {
                let p = self.find(from, to + 1, "(")?;
                self.close[p] + 1
            }
        };
        (start <= to && self.is(start, "{")).then_some(start)
    }

    /// The JSDoc comment that ends on the line before token `i`.
    fn doc_before(&self, i: usize) -> Option<&Doc> {
        let line = self.toks[i].line;
        let prev_end = i
            .checked_sub(1)
            .map(|p| self.toks[p].end_line as isize)
            .unwrap_or(-1);
        self.docs
            .iter()
            .find(|d| (d.end_line + 1 == line || d.end_line == line) && d.line as isize > prev_end)
    }

    fn push(
        &mut self,
        parent: Option<usize>,
        name: String,
        kind: &str,
        first: usize,
        head_end: usize,
        last: usize,
    ) -> usize {
        let n = self.counts.entry(name.clone()).or_default();
        *n += 1;
        let key = if *n == 1 {
            name.clone()
        } else {
            format!("{name}#{n}")
        };
        let doc = self.doc_before(first).cloned();
        let first_line = doc.as_ref().map_or(self.toks[first].line, |d| d.line);
        let last_line = self.toks[last].end_line;
        if let Some(p) = parent {
            self.defs[p].nested.push((first_line, last_line));
        }
        self.defs.push(Def {
            key,
            name,
            kind: kind.to_string(),
            sig: self.source(first, head_end),
            sig_norm: norm(&self.toks[first..=head_end]),
            doc: doc.map(|d| d.text),
            body: String::new(),
            first: first_line,
            last: last_line,
            nested: Vec::new(),
        });
        self.spans.push((first, head_end, last));
        self.defs.len() - 1
    }

    /// Finds the parts among the statements of tokens `from..to`.
    fn scan(&mut self, from: usize, to: usize, prefix: &str, mode: Mode, parent: Option<usize>) {
        let mut i = from;
        while i < to {
            if self.is(i, ";") {
                i += 1;
                continue;
            }
            let end = match mode {
                Mode::Class => self.member(i, to, prefix, parent),
                Mode::Top => self.top(i, to, prefix, parent),
                Mode::Call => self
                    .call(i, to, prefix, parent)
                    .unwrap_or_else(|| self.stmt_end(i, to)),
            };
            i = end + 1;
        }
    }

    /// A top-level statement; returns its last token.
    fn top(&mut self, i: usize, to: usize, prefix: &str, parent: Option<usize>) -> usize {
        let mut j = i;
        while matches!(self.toks[j].text.as_str(), "export" | "default") && j + 1 < to {
            j += 1;
        }
        let is_async = self.is(j, "async") && self.is(j + 1, "function");
        if is_async {
            j += 1;
        }
        if self.is(j, "function") {
            let generator = self.is(j + 1, "*");
            let n = if generator { j + 2 } else { j + 1 };
            let name = if self.ident(n) && !self.is(n, "(") {
                self.toks[n].text.clone()
            } else {
                "default".into()
            };
            if let Some(p) = self.find(n, to, "(")
                && self.is(self.close[p] + 1, "{")
            {
                let brace = self.close[p] + 1;
                let kind = match (is_async, generator) {
                    (true, true) => "async function*",
                    (true, false) => "async function",
                    (false, true) => "function*",
                    _ => "function",
                };
                self.push(
                    parent,
                    format!("{prefix}{name}"),
                    kind,
                    i,
                    brace - 1,
                    self.close[brace],
                );
                return self.close[brace];
            }
        }
        if self.is(j, "class") {
            let name = if self.ident(j + 1) && !self.is(j + 1, "extends") {
                self.toks[j + 1].text.clone()
            } else {
                "default".into()
            };
            if let Some(brace) = self.find(j + 1, to, "{") {
                let close = self.close[brace];
                let qual = format!("{prefix}{name}");
                let d = self.push(parent, qual.clone(), "class", i, brace - 1, close);
                self.scan(brace + 1, close, &format!("{qual}."), Mode::Class, Some(d));
                return close;
            }
        }
        if matches!(self.toks[j].text.as_str(), "const" | "let" | "var")
            && self.ident(j + 1)
            && self.is(j + 2, "=")
        {
            let end = self.stmt_end(j, to);
            let name = format!("{prefix}{}", self.toks[j + 1].text);
            if self.function_value(j + 3) {
                // Up to the body: its `{`, or the expression after `=>`.
                let head_end = match self.body_brace(j + 3, end) {
                    Some(b) => b - 1,
                    None => self.find(j + 3, end + 1, "=>").unwrap_or(j + 1),
                };
                self.push(parent, name, "function", i, head_end, end);
            } else {
                // Objects and calls are compared as a whole.
                let kind = self.toks[j].text.clone();
                self.push(parent, name, &kind, i, j + 1, end);
            }
            return end;
        }
        if let Some(end) = self.call(i, to, prefix, parent) {
            return end;
        }
        let end = self.stmt_end(i, to);
        self.module.extend_from_slice(&self.toks[i..=end]);
        end
    }

    /// A call statement whose first argument is a string
    /// (`it("works", () => {…})`), as a part; its callback's calls of the
    /// same kind are parts nested in it. Returns its last token.
    fn call(&mut self, i: usize, to: usize, prefix: &str, parent: Option<usize>) -> Option<usize> {
        // `describe.only(`, `test.each(…)(` are not followed.
        let mut j = i;
        while self.ident(j) && self.is(j + 1, ".") && self.ident(j + 2) {
            j += 2;
        }
        if !(self.ident(j) && self.is(j + 1, "(")) {
            return None;
        }
        let arg = self.toks.get(j + 2).filter(|t| t.kind == K::Literal)?;
        if !(arg.text.starts_with('"') || arg.text.starts_with('\'') || arg.text.starts_with('`')) {
            return None;
        }
        let paren = j + 1;
        let close = self.close[paren];
        let end = self.stmt_end(i, to);
        let label = arg.text.trim_matches(['"', '\'', '`']).to_string();
        let callee = norm(&self.toks[i..=j]);
        let name = format!("{prefix}{callee}({label:?})");
        // The callback's body, if the call has one.
        let brace = (paren + 1..close).find(|&k| {
            self.is(k, "{")
                && (self.is(k - 1, "=>") || (self.is(k - 1, ")") && self.function_head(k - 1)))
        });
        let head_end = brace.map_or(close, |b| b - 1);
        // Test frameworks' blocks (Jest, Mocha, Vitest, node:test, …) by
        // what they are.
        let kind = match self.toks[i].text.as_str() {
            "describe" | "suite" | "context" => "suite",
            "it" | "test" | "specify" | "bench" => "test",
            _ => "call",
        };
        let d = self.push(parent, name.clone(), kind, i, head_end, end);
        if let Some(b) = brace {
            self.scan(
                b + 1,
                self.close[b],
                &format!("{name} › "),
                Mode::Call,
                Some(d),
            );
        }
        Some(end)
    }

    /// Whether the `)` at `k` closes the parameters of a `function`.
    fn function_head(&self, k: usize) -> bool {
        let open = (0..k).rev().find(|&o| self.close[o] == k);
        open.is_some_and(|o| {
            (o >= 1 && self.is(o - 1, "function")) || (o >= 2 && self.is(o - 2, "function"))
        })
    }

    /// A class member; returns its last token.
    fn member(&mut self, i: usize, to: usize, prefix: &str, parent: Option<usize>) -> usize {
        let mut j = i;
        let mut kind = "method";
        let mut is_static = false;
        loop {
            let t = self.toks[j].text.as_str();
            // A modifier, unless it is the member's own name (`get() {}`).
            let modifier = matches!(t, "static" | "async" | "get" | "set" | "*" | "accessor")
                && !self.is(j + 1, "(")
                && !self.is(j + 1, "=")
                && !self.is(j + 1, ";");
            if !modifier || j + 1 >= to {
                break;
            }
            match t {
                "get" => kind = "getter",
                "set" => kind = "setter",
                "static" => is_static = true,
                _ => {}
            }
            j += 1;
        }
        if is_static && self.is(j, "{") {
            let close = self.close[j];
            self.push(
                parent,
                format!("{prefix}static"),
                "static block",
                i,
                j - 1,
                close,
            );
            return close;
        }
        let (name, after) = if self.is(j, "[") {
            (self.source(j, self.close[j]), self.close[j] + 1)
        } else {
            (self.toks[j].text.clone(), j + 1)
        };
        let name = format!("{prefix}{}", name.trim_matches(['"', '\'']));
        if self.is(after, "(") && self.is(self.close[after] + 1, "{") {
            let brace = self.close[after] + 1;
            let kind = if name.ends_with(".constructor") {
                "constructor"
            } else {
                kind
            };
            self.push(parent, name, kind, i, brace - 1, self.close[brace]);
            return self.close[brace];
        }
        // A field, with or without a value.
        let end = self.stmt_end(i, to);
        let kind = if self.is(after, "=") && self.function_value(after + 1) {
            "method"
        } else {
            "field"
        };
        let head_end = if kind == "method" {
            match self.body_brace(after + 1, end) {
                Some(b) => b - 1,
                None => self.find(after + 1, end + 1, "=>").unwrap_or(after),
            }
        } else {
            j
        };
        self.push(parent, name, kind, i, head_end, end);
        end
    }
}

/// Whether a statement can end after this token.
fn ends_expr(t: &Tok) -> bool {
    match t.kind {
        K::Literal | K::Num => true,
        K::Ident => !matches!(
            t.text.as_str(),
            "return"
                | "typeof"
                | "instanceof"
                | "in"
                | "of"
                | "new"
                | "delete"
                | "void"
                | "await"
                | "yield"
                | "extends"
                | "else"
                | "do"
                | "case"
        ),
        K::Punct => matches!(t.text.as_str(), ")" | "]" | "}" | "++" | "--"),
    }
}

/// Whether this token can start a new statement after a line break (as
/// opposed to continuing the expression before it).
fn starts_stmt(t: &Tok) -> bool {
    match t.kind {
        K::Ident => !matches!(
            t.text.as_str(),
            "instanceof" | "in" | "of" | "else" | "catch" | "finally" | "extends" | "as"
        ),
        K::Literal => !t.text.starts_with('`'),
        K::Num => true,
        K::Punct => matches!(t.text.as_str(), "{" | "!" | "~" | "++" | "--" | "..."),
    }
}

/// The parts of a JavaScript file, and its top-level code outside them.
fn outline(text: &str) -> (Vec<Def>, String) {
    let lexer = Lexer::new(text);
    let (toks, docs) = lexer.tokens();
    // Matching brackets; an unmatched one closes at the end.
    let mut close: Vec<usize> = (0..toks.len()).collect();
    let mut stack: Vec<usize> = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if t.kind != K::Punct {
            continue;
        }
        match t.text.as_str() {
            "(" | "[" | "{" => stack.push(i),
            ")" | "]" | "}" => {
                if let Some(o) = stack.pop() {
                    close[o] = i;
                }
            }
            _ => {}
        }
    }
    for o in stack {
        close[o] = toks.len().saturating_sub(1);
    }
    let mut o = Outline {
        toks: &toks,
        docs: &docs,
        chars: &lexer.chars,
        close,
        defs: Vec::new(),
        spans: Vec::new(),
        counts: BTreeMap::new(),
        module: Vec::new(),
    };
    o.scan(0, toks.len(), "", Mode::Top, None);
    // Each part's body: its tokens after the header, without its nested
    // parts.
    let mut defs = o.defs;
    for (k, &(_, head_end, last)) in o.spans.iter().enumerate() {
        let inner: Vec<(usize, usize)> = o
            .spans
            .iter()
            .filter(|s| s.0 > head_end && s.2 <= last)
            .map(|s| (s.0, s.2))
            .collect();
        let body: Vec<Tok> = (head_end + 1..=last)
            .filter(|t| !inner.iter().any(|(a, b)| a <= t && t <= b))
            .map(|t| toks[t].clone())
            .collect();
        defs[k].body = norm(&body);
    }
    (defs, norm(&o.module))
}

/// Compares two versions of a JavaScript file. Returns the parts that
/// changed, in the order of the head version (removed ones last), plus a
/// `<module>` entry if top-level code outside parts changed.
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
    // In bundled or minified code, parts share lines: a change on a line
    // then looks like a formatting change of every other part on it, which
    // it isn't. Formatting-only changes are only shown for parts that have
    // their lines to themselves.
    let within = |inner: &Def, outer: &Def| {
        inner.name.starts_with(&format!("{}.", outer.name))
            || inner.name.starts_with(&format!("{} › ", outer.name))
    };
    let shares_line = |d: &Def| {
        b.iter().any(|o| {
            !std::ptr::eq(o, d)
                && !within(o, d)
                && !within(d, o)
                && o.first <= d.last
                && d.first <= o.last
        })
    };
    out.extend(compare_defs(&a, &b, old, new).into_iter().filter(|c| {
        !c.cosmetic
            || !b
                .iter()
                .filter(|d| d.name == c.name && d.first + 1 == c.line)
                .any(shares_line)
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<(String, String)> {
        outline(text)
            .0
            .into_iter()
            .map(|d| (d.key, d.kind))
            .collect()
    }

    #[test]
    fn finds_parts() {
        let src = r#"#!/usr/bin/env node
import { x } from "./x.js";

/** Adds. */
export function add(a, b) {
  const s = `${a} + ${b} = ${(() => { return "}"; })()}`;
  return a + b;
}

export default async function () {}

const re = /[/{]+/g, n = 1;
const double = (x) => x * 2
let inc = async x => { return x + 1; };

class Store extends Base {
  #items = [];
  static count = 0;
  static { init(); }
  constructor(items) { super(); this.#items = items; }
  get size() { return this.#items.length; }
  async *[Symbol.asyncIterator]() {}
  add = (item) => { this.#items.push(item); };
  get() { return 1; }
}

describe("store", () => {
  it('adds', async function () {
    expect(add(1, 2)).toBe(3);
  });
  test.skip("skips", () => {});
});

window.addEventListener("load", main);
"#;
        let k = keys(src);
        let k: Vec<(&str, &str)> = k.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        assert_eq!(
            k,
            [
                ("add", "function"),
                ("default", "async function"),
                ("re", "const"),
                ("double", "function"),
                ("inc", "function"),
                ("Store", "class"),
                ("Store.#items", "field"),
                ("Store.count", "field"),
                ("Store.static", "static block"),
                ("Store.constructor", "constructor"),
                ("Store.size", "getter"),
                ("Store.[Symbol.asyncIterator]", "method"),
                ("Store.add", "method"),
                ("Store.get", "method"),
                ("describe(\"store\")", "suite"),
                ("describe(\"store\") › it(\"adds\")", "test"),
                ("describe(\"store\") › test.skip(\"skips\")", "test"),
                ("window.addEventListener(\"load\")", "call"),
            ]
        );
        let (defs, module) = outline(src);
        assert_eq!(defs[0].first, 3, "the JSDoc comment belongs to `add`");
        assert_eq!(defs[0].doc.as_deref(), Some("/** Adds. */"));
        assert_eq!(defs[0].sig, "export function add(a, b)");
        assert_eq!(defs[0].last, 7);
        assert_eq!(defs[3].sig, "const double = (x) =>");
        assert_eq!(defs[4].sig, "let inc = async x =>");
        assert_eq!(module, "import{x}from\"./x.js\";");
        // The test's own lines are not part of the suite's body.
        assert!(!defs[14].body.contains("expect"));
    }

    #[test]
    fn compares_parts() {
        let old = "/** Old. */\nfunction a() {\n  return 1;\n}\n\nfunction b(x) { return x; }\n\nclass C {\n  m() { return 1; }\n  n() {}\n}\n\nconst gone = () => 0;\n";
        let new = "/** New. */\nfunction a() {\n  // why\n  return 1;\n}\n\nfunction b(x, y) { return x; }\n\nclass C {\n  m() { return 2; }\n  n() {}\n}\n\nexport const added = 1;\n";
        let c = compare(old, new);
        let got: Vec<(&str, &str, Status)> = c
            .iter()
            .map(|p| (p.name.as_str(), p.kind.as_str(), p.status))
            .collect();
        assert_eq!(
            got,
            [
                ("a", "function", Status::Changed),
                ("b", "function", Status::Changed),
                ("C.m", "method", Status::Changed),
                ("added", "const", Status::Added),
                ("gone", "function", Status::Removed),
            ]
        );
        assert!(c[0].doc_changed && !c[0].body_changed);
        assert_eq!(c[1].old_sig.as_deref(), Some("function b(x)"));
        assert!(c[2].body_changed);
    }

    #[test]
    fn statements_end_at_line_breaks() {
        let src = "const a = 1\nconst b = a\n  + 2\nfoo()\n[1, 2].map(f)\nconst c = 3\n";
        let k = keys(src);
        let names: Vec<&str> = k.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
        let (defs, module) = outline(src);
        assert_eq!((defs[1].first, defs[1].last), (1, 2));
        // `foo()` and `[1, 2]…` on the next line are one statement, as in
        // JavaScript.
        assert_eq!(module, "foo()[1,2].map(f)");
    }

    #[test]
    fn no_formatting_changes_on_shared_lines() {
        let old = "function a(){return 1}function b(){return 2}\nfunction c() {\n  return 3;\n}\n";
        let new = "function a(){return 1}function b(){return 22}\nfunction c() {\n  return 3; // three\n}\n";
        let c = compare(old, new);
        let names: Vec<&str> = c.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["b", "c"]);
        assert!(!c[0].cosmetic && c[1].cosmetic);
    }

    #[test]
    fn never_panics_on_trailing_backslash() {
        for src in ["'\\", "`\\", "`${'\\", "x = \"a\\"] {
            outline(src);
        }
    }

    #[test]
    fn never_panics_on_cut_off_code() {
        let src = include_str!("../../../ci/smoke.mjs");
        let lines: Vec<&str> = src.split('\n').collect();
        for n in 0..lines.len() {
            outline(&lines[..n].join("\n"));
        }
        let chars: Vec<char> = src.chars().collect();
        for n in (0..chars.len()).step_by(7) {
            outline(&chars[..n].iter().collect::<String>());
        }
    }
}
