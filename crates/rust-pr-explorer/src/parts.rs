//! Files split into parts that are compared one by one and shown as boxes:
//! Python definitions (see `python.rs`), YAML keys, and the code of an
//! analyzed Rust file that belongs to no item (tests, macro calls,
//! `extern` blocks).

use model::{PartChange, SourceLine, Status};
use proc_macro2::{Delimiter, LineColumn, TokenStream, TokenTree};
use quote::ToTokens;
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeMap;
use syn::spanned::Spanned;

/// A part with its extent.
#[derive(Debug, Clone, Default)]
pub struct Def {
    /// Qualified name, plus `#n` for the n-th repetition of a name.
    pub key: String,
    pub name: String,
    pub kind: String,
    /// Header with decorators or attributes, for display.
    pub sig: String,
    /// The header's tokens, for comparing.
    pub sig_norm: String,
    /// The doc comment or docstring, if any.
    pub doc: Option<String>,
    /// The rest, normalized: equal bodies are equal here even if their
    /// formatting or comments differ.
    pub body: String,
    /// Physical lines of the whole part (0-based, inclusive), decorators
    /// and attributes included.
    pub first: usize,
    pub last: usize,
    /// Physical lines that belong to nested parts.
    pub nested: Vec<(usize, usize)>,
}

/// Makes keys unique: the n-th repetition of a name gets `#n`.
fn unique_key(counts: &mut BTreeMap<String, usize>, name: &str) -> String {
    let n = counts.entry(name.to_string()).or_default();
    *n += 1;
    if *n == 1 {
        name.to_string()
    } else {
        format!("{name}#{n}")
    }
}

/// The physical lines of a part, without its nested parts.
fn own_lines(text: &[&str], d: &Def) -> String {
    (d.first..=d.last.min(text.len().saturating_sub(1)))
        .filter(|i| !d.nested.iter().any(|(a, b)| a <= i && i <= b))
        .map(|i| format!("{}\n", text[i]))
        .collect()
}

fn diff_lines(old: &str, new: &str) -> Vec<SourceLine> {
    TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|c| {
            let tag = match c.tag() {
                ChangeTag::Equal => ' ',
                ChangeTag::Insert => '+',
                ChangeTag::Delete => '-',
            };
            SourceLine(tag, c.value().trim_end_matches('\n').to_string())
        })
        .collect()
}

/// For each thing of the old version, the next one after it that the new
/// version still has (`kept`). Things that only the old version has sort
/// right before that one, so they show where they were among their old
/// siblings instead of after everything else. Since parts come parent
/// first, a removed part never lands among the new children of a part it
/// didn't belong to.
pub fn next_kept<T>(old: &[T], kept: impl Fn(&T) -> bool) -> Vec<Option<&T>> {
    let mut out = vec![None; old.len()];
    let mut next = None;
    for (i, x) in old.iter().enumerate().rev() {
        out[i] = next;
        if kept(x) {
            next = Some(x);
        }
    }
    out
}

/// Compares the parts of two versions of a file, matched by key. Returns
/// those that changed, in the order of the head version, with removed ones
/// where they were (see [`next_kept`]).
pub fn compare_defs(a: &[Def], b: &[Def], old: &str, new: &str) -> Vec<PartChange> {
    let (old_lines, new_lines): (Vec<&str>, Vec<&str>) =
        (old.split('\n').collect(), new.split('\n').collect());
    let old_by_key: BTreeMap<&str, &Def> = a.iter().map(|d| (d.key.as_str(), d)).collect();
    let new_by_key: BTreeMap<&str, &Def> = b.iter().map(|d| (d.key.as_str(), d)).collect();
    // Parts by their first line, removed ones right before the part that
    // followed them; the sort keeps each version's order among its own.
    let kept = |o: &Def| new_by_key.contains_key(o.key.as_str());
    let mut order: Vec<(usize, bool, &str)> = a
        .iter()
        .zip(next_kept(a, kept))
        .filter(|(o, _)| !kept(o))
        .map(|(o, next)| {
            let at = next.map_or(usize::MAX, |n| new_by_key[n.key.as_str()].first);
            (at, false, o.key.as_str())
        })
        .chain(b.iter().map(|d| (d.first, true, d.key.as_str())))
        .collect();
    order.sort_by_key(|&(line, kept, _)| (line, kept));
    let mut out = Vec::new();
    for (_, kept, key) in order {
        if !kept {
            let o = old_by_key[key];
            out.push(PartChange {
                name: o.name.clone(),
                kind: o.kind.clone(),
                status: Status::Removed,
                sig: o.sig.clone(),
                line: o.first + 1,
                lines: diff_lines(&own_lines(&old_lines, o), ""),
                ..Default::default()
            });
            continue;
        }
        let d = new_by_key[key];
        let src = own_lines(&new_lines, d);
        match old_by_key.get(key) {
            None => out.push(PartChange {
                name: d.name.clone(),
                kind: d.kind.clone(),
                status: Status::Added,
                sig: d.sig.clone(),
                line: d.first + 1,
                lines: diff_lines("", &src),
                ..Default::default()
            }),
            Some(o) => {
                let old_src = own_lines(&old_lines, o);
                let sig_changed = o.sig_norm != d.sig_norm || o.kind != d.kind;
                let doc_changed = o.doc != d.doc;
                let body_changed = o.body != d.body;
                // Blank lines between nested parts don't count.
                let solid = |t: &str| {
                    t.lines()
                        .filter(|l| !l.trim().is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let cosmetic =
                    !sig_changed && !doc_changed && !body_changed && solid(&old_src) != solid(&src);
                if !(sig_changed || doc_changed || body_changed || cosmetic) {
                    continue;
                }
                out.push(PartChange {
                    name: d.name.clone(),
                    kind: d.kind.clone(),
                    status: if cosmetic {
                        Status::Unchanged
                    } else {
                        Status::Changed
                    },
                    sig: d.sig.clone(),
                    old_sig: sig_changed.then(|| o.sig.clone()),
                    doc_changed,
                    body_changed,
                    cosmetic,
                    line: d.first + 1,
                    lines: diff_lines(&old_src, &src),
                });
            }
        }
    }
    out
}

/// A range of lines, 1-based and inclusive.
pub type Span = (usize, usize);

/// The parts of a Rust file: every item except imports and modules, with
/// the items of inline modules as parts of their own (`tests::adds`).
/// Also returns the line spans of imports (`use`, `extern crate`), module
/// declarations (`mod name;`) and the header and closing line of inline
/// modules, with their attributes: they show in the items they affect, not
/// as code of their own. Empty if the file doesn't parse.
pub fn rust_outline(text: &str) -> (Vec<Def>, Vec<Span>) {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut defs = Vec::new();
    let mut decls = Vec::new();
    let mut counts = BTreeMap::new();
    if let Ok(file) = syn::parse_file(text) {
        rust_walk(&lines, &file.items, "", &mut counts, &mut defs, &mut decls);
    }
    (defs, decls)
}

fn rust_walk(
    lines: &[&str],
    items: &[syn::Item],
    prefix: &str,
    counts: &mut BTreeMap<String, usize>,
    defs: &mut Vec<Def>,
    decls: &mut Vec<Span>,
) {
    use syn::Item;
    for item in items {
        let attrs = item_attrs(item);
        let start = attrs
            .iter()
            .map(|a| a.span().start().line)
            .chain([item.span().start().line])
            .min()
            .unwrap_or(1);
        let end = item.span().end().line;
        let qual = |name: &str| format!("{prefix}{name}");
        let (kind, name) = match item {
            Item::Use(_) | Item::ExternCrate(_) => {
                decls.push((start, end));
                continue;
            }
            Item::Mod(m) => {
                match &m.content {
                    None => decls.push((start, end)),
                    Some((brace, inner)) => {
                        decls.push((start, brace.span.open().start().line));
                        decls.push((end, end));
                        rust_walk(
                            lines,
                            inner,
                            &qual(&format!("{}::", m.ident)),
                            counts,
                            defs,
                            decls,
                        );
                    }
                }
                continue;
            }
            Item::Fn(f) => {
                let is = |name: &str| {
                    attrs
                        .iter()
                        .any(|a| a.path().segments.last().is_some_and(|s| s.ident == name))
                };
                let kind = if is("test") {
                    "test"
                } else if is("bench") {
                    "bench"
                } else {
                    "fn"
                };
                (kind, f.sig.ident.to_string())
            }
            Item::Struct(i) => ("struct", i.ident.to_string()),
            Item::Enum(i) => ("enum", i.ident.to_string()),
            Item::Union(i) => ("union", i.ident.to_string()),
            Item::Trait(i) => ("trait", i.ident.to_string()),
            Item::TraitAlias(i) => ("trait", i.ident.to_string()),
            Item::Type(i) => ("type", i.ident.to_string()),
            Item::Const(i) => ("const", i.ident.to_string()),
            Item::Static(i) => ("static", i.ident.to_string()),
            Item::Impl(i) => {
                let mut name = String::from("impl ");
                if let Some((path, _)) = &i.trait_ {
                    name.push_str(&tokens_text(path.to_token_stream()));
                    name.push_str(" for ");
                }
                name.push_str(&tokens_text(i.self_ty.to_token_stream()));
                ("impl", name)
            }
            Item::Macro(m) => match &m.ident {
                Some(ident) => ("macro", ident.to_string()),
                None => (
                    "macro call",
                    format!("{}!", tokens_text(m.mac.path.to_token_stream())),
                ),
            },
            Item::ForeignMod(f) => ("extern", tokens_text(f.abi.to_token_stream())),
            _ => continue,
        };
        let name = qual(&name);
        // The header is everything before the body: the last top-level
        // `{ … }` group (any group for a macro call).
        let mut bare = item.clone();
        strip_attrs(&mut bare);
        let tokens: Vec<TokenTree> = bare.into_token_stream().into_iter().collect();
        let body_at = tokens.iter().rposition(|t| match t {
            TokenTree::Group(g) => {
                g.delimiter() == Delimiter::Brace || matches!(item, Item::Macro(_))
            }
            _ => false,
        });
        let head_start = tokens.first().map_or(
            LineColumn {
                line: start,
                column: 0,
            },
            |t| t.span().start(),
        );
        let head_end = body_at.map_or(item.span().end(), |i| tokens[i].span().start());
        let shown: Vec<&syn::Attribute> = attrs.iter().filter(|a| !is_doc(a)).collect();
        let mut sig: Vec<String> = shown
            .iter()
            .map(|a| slice(lines, a.span().start(), a.span().end()))
            .collect();
        sig.push(slice(lines, head_start, head_end));
        let head_tokens: TokenStream = tokens[..body_at.unwrap_or(tokens.len())]
            .iter()
            .cloned()
            .collect();
        let sig_norm = shown
            .iter()
            .map(|a| a.to_token_stream().to_string())
            .chain([head_tokens.to_string()])
            .collect::<Vec<_>>()
            .join("\n");
        let docs: Vec<String> = attrs
            .iter()
            .filter(|a| is_doc(a))
            .map(|a| a.to_token_stream().to_string())
            .collect();
        defs.push(Def {
            key: unique_key(counts, &name),
            name,
            kind: kind.to_string(),
            sig: sig.join("\n").trim_end().to_string(),
            sig_norm,
            doc: (!docs.is_empty()).then(|| docs.join("\n")),
            body: body_at.map(|i| tokens[i].to_string()).unwrap_or_default(),
            first: start.saturating_sub(1),
            last: end.saturating_sub(1),
            nested: Vec::new(),
        });
    }
}

fn is_doc(a: &syn::Attribute) -> bool {
    a.path().is_ident("doc")
}

/// Tokens as text, without the spaces `TokenStream`'s display puts
/// around punctuation (`Vec < u8 >` becomes `Vec<u8>`).
fn tokens_text(t: TokenStream) -> String {
    let s = t.to_string();
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == ' ' {
            let prev = chars[..i].last().copied().unwrap_or(' ');
            let next = chars.get(i + 1).copied().unwrap_or(' ');
            let word = |c: char| c.is_alphanumeric() || c == '_' || c == '"';
            if !(word(prev) && word(next)) && prev != ',' {
                continue;
            }
        }
        out.push(*c);
    }
    out
}

/// The source text between two positions (columns count characters).
fn slice(lines: &[&str], from: LineColumn, to: LineColumn) -> String {
    let mut out = Vec::new();
    for l in from.line..=to.line {
        let Some(text) = lines.get(l.wrapping_sub(1)) else {
            break;
        };
        let chars: Vec<char> = text.chars().collect();
        let a = if l == from.line { from.column } else { 0 };
        let b = if l == to.line {
            to.column.min(chars.len())
        } else {
            chars.len()
        };
        out.push(chars[a.min(b)..b].iter().collect::<String>());
    }
    out.join("\n").trim_end().to_string()
}

fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    use syn::Item;
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

fn strip_attrs(item: &mut syn::Item) {
    use syn::Item;
    let attrs = match item {
        Item::Const(i) => &mut i.attrs,
        Item::Enum(i) => &mut i.attrs,
        Item::ExternCrate(i) => &mut i.attrs,
        Item::Fn(i) => &mut i.attrs,
        Item::ForeignMod(i) => &mut i.attrs,
        Item::Impl(i) => &mut i.attrs,
        Item::Macro(i) => &mut i.attrs,
        Item::Mod(i) => &mut i.attrs,
        Item::Static(i) => &mut i.attrs,
        Item::Struct(i) => &mut i.attrs,
        Item::Trait(i) => &mut i.attrs,
        Item::TraitAlias(i) => &mut i.attrs,
        Item::Type(i) => &mut i.attrs,
        Item::Union(i) => &mut i.attrs,
        Item::Use(i) => &mut i.attrs,
        _ => return,
    };
    attrs.clear();
}

/// A YAML line without its trailing comment (a `#` after a space, outside
/// quotes).
fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && prev.is_whitespace() => return line[..i].trim_end(),
            None => {}
        }
        prev = c;
    }
    line.trim_end()
}

/// YAML blocks longer than this are split at their child keys or list
/// items.
const YAML_SPLIT: usize = 12;
/// How deep YAML blocks are split.
const YAML_DEPTH: usize = 4;

/// The parts of a YAML file: its top-level keys, and the child keys or
/// list items of long blocks (`jobs.test` and `jobs.test.steps[Build]` in
/// a GitHub workflow).
pub fn yaml_outline(text: &str) -> Vec<Def> {
    let y = Yaml {
        lines: text.split('\n').collect(),
    };
    let mut out = Vec::new();
    let mut counts = BTreeMap::new();
    for b in y.blocks(0, y.lines.len(), 0, false) {
        let name = unique_key(&mut counts, &b.label);
        y.emit(name, &b, 0, &mut out);
    }
    out
}

/// A key or list item with its lines (0-based, inclusive): `first`
/// includes the comments right above it, `last` is its last non-blank
/// line.
struct Block {
    label: String,
    head: usize,
    first: usize,
    last: usize,
}

struct Yaml<'a> {
    lines: Vec<&'a str>,
}

impl Yaml<'_> {
    fn indent(&self, i: usize) -> usize {
        let l = self.lines[i];
        l.len() - l.trim_start_matches(' ').len()
    }

    fn blank(&self, i: usize) -> bool {
        let t = self.lines[i].trim();
        t.is_empty() || t.starts_with('#') || t == "---" || t == "..."
    }

    /// The key of a `key: …` line (or of the text after a list's `- `).
    fn key_of(t: &str) -> Option<(String, &str)> {
        let t = t.trim();
        if t.starts_with(['-', '{', '[', '#']) {
            return None;
        }
        let colon = t
            .char_indices()
            .find(|&(i, c)| c == ':' && t[i + 1..].chars().next().is_none_or(|n| n == ' '))?
            .0;
        let key = t[..colon].trim().trim_matches(['"', '\'']);
        (!key.is_empty()).then(|| (key.to_string(), strip_comment(t[colon + 1..].trim())))
    }

    fn is_item(&self, i: usize) -> bool {
        let t = self.lines[i].trim_start();
        t == "-" || t.starts_with("- ")
    }

    /// The keys (or with `items`, the list items) at indentation `ind`
    /// within lines `from..to`.
    fn blocks(&self, from: usize, to: usize, ind: usize, items: bool) -> Vec<Block> {
        let heads: Vec<(usize, String)> = (from..to)
            .filter(|&i| !self.blank(i) && self.indent(i) == ind)
            .filter_map(|i| {
                if items {
                    self.is_item(i).then(|| (i, String::new()))
                } else {
                    Self::key_of(self.lines[i]).map(|(k, _)| (i, k))
                }
            })
            .collect();
        let mut out = Vec::new();
        for (n, (i, label)) in heads.iter().enumerate() {
            let mut first = *i;
            while first > from && self.lines[first - 1].trim().starts_with('#') {
                first -= 1;
            }
            let next = heads.get(n + 1).map_or(to, |h| h.0);
            let mut last = next - 1;
            while last > *i && self.blank(last) {
                last -= 1;
            }
            let label = if items {
                self.item_label(*i, last)
                    .unwrap_or_else(|| (n + 1).to_string())
            } else {
                label.clone()
            };
            out.push(Block {
                label,
                head: *i,
                first,
                last,
            });
        }
        out
    }

    /// A list item's `name:` (or else its `id:` or `uses:`), on its first
    /// line or among its keys.
    fn item_label(&self, head: usize, last: usize) -> Option<String> {
        let line = self.lines[head];
        let content = self.indent(head) + 2;
        let first = &line.trim_start()[1..];
        std::iter::once(first)
            .chain(
                (head + 1..=last)
                    .filter(|&i| !self.blank(i) && self.indent(i) == content)
                    .map(|i| self.lines[i]),
            )
            .filter_map(Self::key_of)
            .filter(|(k, v)| ["name", "id", "uses"].contains(&k.as_str()) && !v.is_empty())
            .min_by_key(|(k, _)| k != "name")
            .map(|(_, v)| v.trim_matches(['"', '\'']).to_string())
    }

    /// Adds the part for block `b`, split into its children if it is long.
    fn emit(&self, name: String, b: &Block, depth: usize, out: &mut Vec<Def>) {
        let parent = self.indent(b.head);
        let children = (b.head + 1..=b.last)
            .find(|&i| !self.blank(i))
            // List items (a workflow's steps) are not split further.
            .filter(|_| {
                b.last - b.first + 1 > YAML_SPLIT && depth < YAML_DEPTH && !self.is_item(b.head)
            })
            .map(|i| {
                let (ind, items) = (self.indent(i), self.is_item(i));
                if (ind > parent || (items && ind == parent)) && ind > 0 {
                    self.blocks(b.head + 1, b.last + 1, ind, items)
                } else {
                    Vec::new()
                }
            })
            .unwrap_or_default();
        let children = if children.len() < 2 {
            Vec::new()
        } else {
            children
        };
        let nested: Vec<Span> = children.iter().map(|c| (c.first, c.last)).collect();
        let sig = self.lines[b.head].trim().to_string();
        let body = (b.head + 1..=b.last)
            .filter(|i| !nested.iter().any(|(x, y)| x <= i && i <= y))
            .filter(|&i| !self.blank(i))
            .map(|i| format!("{}\n", strip_comment(self.lines[i])))
            .collect();
        out.push(Def {
            key: name.clone(),
            name: name.clone(),
            kind: "key".into(),
            sig_norm: strip_comment(&sig)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            sig,
            doc: None,
            body,
            first: b.first,
            last: b.last,
            nested,
        });
        let mut counts = BTreeMap::new();
        for c in &children {
            let label = unique_key(&mut counts, &c.label);
            let child = if self.is_item(c.head) {
                format!("{name}[{label}]")
            } else {
                format!("{name}.{label}")
            };
            self.emit(child, c, depth + 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(c: &[PartChange]) -> Vec<(&str, &str, Status)> {
        c.iter()
            .map(|c| (c.name.as_str(), c.kind.as_str(), c.status))
            .collect()
    }

    #[test]
    fn rust_parts() {
        let old = "use std::fmt;\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn adds() {\n        assert_eq!(1 + 1, 2);\n    }\n\n    /// Old doc.\n    #[test]\n    fn keeps() {\n        assert!(true);\n    }\n\n    fn helper() {}\n}\n\nthread_local! { static N: u8 = 0; }\n\nextern \"C\" {\n    fn abs(x: i32) -> i32;\n}\n";
        let new = "use std::fmt;\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn adds() {\n        assert_eq!(1 + 2, 3);\n    }\n\n    /// New doc.\n    #[test]\n    fn keeps() {\n        // checked\n        assert!(true);\n    }\n\n    #[tokio::test]\n    async fn waits() {}\n}\n\nthread_local! { static N: u8 = 0; }\n\nextern \"C\" {\n    fn abs(x: i64) -> i64;\n}\n";
        let (a, decls) = rust_outline(old);
        // `use std::fmt`, the module's header, its `use` and its `}`.
        assert_eq!(decls, [(1, 1), (3, 4), (19, 19), (5, 5)]);
        let (b, _) = rust_outline(new);
        let c = compare_defs(&a, &b, old, new);
        assert_eq!(
            names(&c),
            [
                ("tests::adds", "test", Status::Changed),
                ("tests::keeps", "test", Status::Changed),
                ("tests::waits", "test", Status::Added),
                // Where it was: at the end of `tests`, not of the file.
                ("tests::helper", "fn", Status::Removed),
                ("extern \"C\"", "extern", Status::Changed),
            ]
        );
        assert!(c[0].body_changed && !c[0].doc_changed);
        assert!(c[1].doc_changed && !c[1].body_changed);
        assert_eq!(c[2].sig, "#[tokio::test]\nasync fn waits()");
        assert_eq!(c[0].sig, "#[test]\nfn adds()");
        assert_eq!(c[0].line, 7);
    }

    #[test]
    fn yaml_parts() {
        let steps = |build: &str| {
            "      - run: a\n".repeat(6)
                + &format!("      - name: Build\n        run: |\n          {build}\n")
                + &"      - run: b\n".repeat(6)
        };
        let old = format!(
            "name: CI\non: [push]\n\njobs:\n  # Checks.\n  test:\n    runs-on: ubuntu\n    steps:\n{}  docs:\n    runs-on: ubuntu\n",
            steps("cargo build")
        );
        let new = format!(
            "name: CI\non: [push, pull_request]\n\njobs:\n  # Checks.\n  test:\n    runs-on: ubuntu  # same\n    steps:\n{}  docs:\n    runs-on: macos\n",
            steps("cargo build --release")
        );
        let (a, b) = (yaml_outline(&old), yaml_outline(&new));
        let keys: Vec<&str> = b.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(
            keys[..7],
            [
                "name",
                "on",
                "jobs",
                "jobs.test",
                "jobs.test.runs-on",
                "jobs.test.steps",
                "jobs.test.steps[1]"
            ]
        );
        assert!(keys.contains(&"jobs.test.steps[Build]") && keys.contains(&"jobs.test.steps[13]"));
        assert_eq!(b[3].first, 4, "the comment above `test` belongs to it");
        let c = compare_defs(&a, &b, &old, &new);
        assert_eq!(
            names(&c),
            [
                ("on", "key", Status::Changed),
                ("jobs.test.runs-on", "key", Status::Unchanged),
                ("jobs.test.steps[Build]", "key", Status::Changed),
                ("jobs.docs", "key", Status::Changed),
            ]
        );
        assert!(c[1].cosmetic);
        assert_eq!(c[2].sig, "- name: Build");
    }

    #[test]
    fn removed_parts_stay_among_their_siblings() {
        let push = format!(
            "name: CI\non:\n  push:\n    branches:\n{}",
            "      - main\n".repeat(8)
        );
        let old = push.clone()
            + "  pull_request_target:\n    types: [opened]\n  workflow_dispatch:\n\npermissions:\n  contents: read\n  pull-requests: write\n  issues: write\n\njobs:\n  check:\n    runs-on: ubuntu\n";
        let new = push
            + "  pull_request:\n    types: [opened]\n  schedule:\n    - cron: '17 * * * *'\n  workflow_dispatch:\n\npermissions:\n  contents: read\n  pull-requests: write\n  issues: write\n\njobs:\n  check:\n    runs-on: macos\n";
        let c = compare_defs(&yaml_outline(&old), &yaml_outline(&new), &old, &new);
        assert_eq!(
            names(&c),
            [
                ("on.pull_request", "key", Status::Added),
                ("on.schedule", "key", Status::Added),
                ("on.pull_request_target", "key", Status::Removed),
                ("jobs", "key", Status::Changed),
            ]
        );
        // The last child of a part goes after that part's new children,
        // not before them, and a removed part at the end stays there.
        let old = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n\n    fn helper() {}\n}\n\nthread_local! { static N: u8 = 0; }\n\nmacro_rules! m { () => {} }\n";
        let new = "fn a() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n\n    #[test]\n    fn u() {}\n}\n\nthread_local! { static N: u8 = 0; }\n";
        let ((a, _), (b, _)) = (rust_outline(old), rust_outline(new));
        let c = compare_defs(&a, &b, old, new);
        assert_eq!(
            names(&c),
            [
                ("tests::u", "test", Status::Added),
                ("tests::helper", "fn", Status::Removed),
                ("m", "macro", Status::Removed),
            ]
        );
    }
}
