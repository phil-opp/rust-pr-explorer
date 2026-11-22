//! Signature diffs aligned on syntax instead of text.
//!
//! The old and new signature texts (as rendered by the analysis) are parsed
//! again with `syn`. Parameters, generic parameters, where-clauses, fields
//! and variants are matched by name; matched pieces whose text differs are
//! compared as type trees, so that `f64` → `Result<f64, Error>` becomes one
//! wrap instead of two unrelated insertions. Text between the pieces (and
//! comments such as `/* private fields */`) is taken from the new
//! signature as is, using the byte ranges of the parsed nodes.

use model::{DiffNote, DiffSpan, Kind, SigDiff};
use proc_macro2::Span;
use std::ops::Range;
use syn::spanned::Spanned;

/// Diffs two signatures of the same kind. `None` if they can't be parsed
/// (then the viewer falls back to a token diff).
pub fn diff(kind: Kind, old: &str, new: &str) -> Option<SigDiff> {
    let mut cx = Cx::default();
    let (po, pn) = (Parsed::new(kind, old)?, Parsed::new(kind, new)?);
    let (old_sections, new_sections) = (po.sections()?, pn.sections()?);
    if old_sections.len() != new_sections.len() {
        return None;
    }
    let mut edits: Vec<Edit> = Vec::new();
    for (o, n) in old_sections.iter().zip(&new_sections) {
        cx.section(old, new, o, n, &mut edits);
    }
    edits.sort_by_key(|e| (e.range.start, e.range.end > e.range.start));
    let mut spans = Vec::new();
    let mut cursor = 0;
    for e in edits {
        if e.range.start > cursor {
            spans.push(same(&new[cursor..e.range.start]));
        }
        spans.extend(e.spans);
        cursor = cursor.max(e.range.end);
    }
    if cursor < new.len() {
        spans.push(same(&new[cursor..]));
    }
    Some(SigDiff {
        spans: merge_same(spans),
        notes: cx.notes,
    })
}

/// A signature parsed for diffing.
enum Parsed {
    /// A function with ` {}` appended, so that it parses as an item.
    Fn(syn::ItemFn, String),
    Data(syn::Item, String),
}

impl Parsed {
    fn new(kind: Kind, text: &str) -> Option<Parsed> {
        match kind {
            Kind::Fn | Kind::Method | Kind::TraitMethod => {
                let (f, source) = parse_fn(text)?;
                Some(Parsed::Fn(f, source))
            }
            Kind::Struct | Kind::Enum | Kind::Union => {
                Some(Parsed::Data(syn::parse_str(text).ok()?, text.to_string()))
            }
            _ => None,
        }
    }

    fn sections(&self) -> Option<Vec<Section<'_>>> {
        match self {
            Parsed::Fn(f, source) => Some(fn_sections(f, source)),
            Parsed::Data(item, text) => data_sections(item, text),
        }
    }
}

/// Replacement of a range of the new text (empty for insertions of
/// removed text).
struct Edit {
    range: Range<usize>,
    spans: Vec<DiffSpan>,
}

/// A matchable part of a signature.
struct Piece<'a> {
    key: String,
    range: Range<usize>,
    /// The piece's type, if it has one (parameters, fields, return type).
    ty: Option<&'a syn::Type>,
}

/// A list of pieces: e.g. the parameters of a function.
struct Section<'a> {
    /// Singular name used in notes, e.g. "parameter".
    label: &'static str,
    pieces: Vec<Piece<'a>>,
    /// Where the section's text starts (also for an empty section) and ends.
    /// A section that exists on one side only is added or removed whole.
    anchor: usize,
    end: usize,
    /// Separator between pieces; empty for line-based sections.
    sep: &'static str,
    /// Prefix kept in notes, e.g. `-> ` of a return type, stripped.
    strip: &'static str,
}

fn range_of(span: Span) -> Range<usize> {
    span.byte_range()
}

fn parse_fn(text: &str) -> Option<(syn::ItemFn, String)> {
    let trimmed = text.trim_end();
    let body_less = trimmed
        .strip_suffix("{ ... }")
        .or_else(|| trimmed.strip_suffix(';'))
        .unwrap_or(trimmed)
        .trim_end();
    let source = format!("{body_less} {{}}");
    let f = syn::parse_str::<syn::ItemFn>(&source).ok()?;
    Some((f, source))
}

fn fn_sections<'a>(f: &'a syn::ItemFn, text: &str) -> Vec<Section<'a>> {
    let sig = &f.sig;
    let ident_end = range_of(sig.ident.span()).end;
    let paren = sig.paren_token.span;
    let params_start = range_of(paren.open()).end;
    let params_end = range_of(paren.close()).start;
    let after_params = range_of(paren.close()).end;
    let head = Section {
        label: "signature",
        pieces: vec![Piece {
            key: "head".into(),
            range: 0..ident_end,
            ty: None,
        }],
        anchor: 0,
        end: ident_end,
        sep: "",
        strip: "",
    };
    let generics_end = sig
        .generics
        .gt_token
        .map_or(ident_end, |gt| range_of(gt.span).end);
    let generics = Section {
        label: "generic parameter",
        pieces: sig
            .generics
            .params
            .iter()
            .map(|p| Piece {
                key: match p {
                    syn::GenericParam::Lifetime(l) => l.lifetime.to_string(),
                    syn::GenericParam::Type(t) => t.ident.to_string(),
                    syn::GenericParam::Const(c) => c.ident.to_string(),
                },
                range: range_of(p.span()),
                ty: None,
            })
            .collect(),
        anchor: ident_end,
        end: generics_end,
        sep: ", ",
        strip: "",
    };
    let params = Section {
        label: "parameter",
        pieces: sig
            .inputs
            .iter()
            .map(|arg| match arg {
                syn::FnArg::Receiver(r) => Piece {
                    key: "self".into(),
                    range: range_of(r.span()),
                    ty: None,
                },
                syn::FnArg::Typed(pt) => Piece {
                    key: text[range_of(pt.pat.span())].to_string(),
                    range: range_of(pt.span()),
                    ty: Some(&pt.ty),
                },
            })
            .collect(),
        anchor: params_start,
        end: params_end,
        sep: ", ",
        strip: "",
    };
    let (ret_pieces, ret_end) = match &sig.output {
        syn::ReturnType::Default => (Vec::new(), after_params),
        syn::ReturnType::Type(arrow, ty) => {
            let r = range_of(arrow.span().join(ty.span()).unwrap_or(ty.span()));
            let end = r.end;
            (
                vec![Piece {
                    key: "return type".into(),
                    range: r,
                    ty: Some(ty),
                }],
                end,
            )
        }
    };
    let ret = Section {
        label: "return type",
        pieces: ret_pieces,
        anchor: after_params,
        end: ret_end,
        sep: "",
        strip: "-> ",
    };
    let (where_pieces, where_end) = match &sig.generics.where_clause {
        None => (Vec::new(), ret_end),
        Some(w) => (
            w.predicates
                .iter()
                .map(|p| Piece {
                    key: match p {
                        syn::WherePredicate::Type(t) => {
                            text[range_of(t.bounded_ty.span())].to_string()
                        }
                        syn::WherePredicate::Lifetime(l) => l.lifetime.to_string(),
                        _ => text[range_of(p.span())].to_string(),
                    },
                    range: range_of(p.span()),
                    ty: None,
                })
                .collect(),
            range_of(w.span()).end,
        ),
    };
    let where_ = Section {
        label: "where-clause",
        pieces: where_pieces,
        anchor: ret_end,
        end: where_end,
        sep: ", ",
        strip: "",
    };
    vec![head, generics, params, ret, where_]
}

/// The whole line(s) a range is on, including the line break.
fn line_range(text: &str, r: Range<usize>) -> Range<usize> {
    let start = text[..r.start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[r.end..]
        .find('\n')
        .map_or(text.len(), |i| r.end + i + 1);
    start..end
}

fn data_sections<'a>(item: &'a syn::Item, text: &str) -> Option<Vec<Section<'a>>> {
    let (fields, variants): (
        Option<&syn::Fields>,
        Option<&syn::punctuated::Punctuated<syn::Variant, syn::Token![,]>>,
    ) = match item {
        syn::Item::Struct(s) => (Some(&s.fields), None),
        syn::Item::Enum(e) => (None, Some(&e.variants)),
        syn::Item::Union(_) => (None, None),
        _ => return None,
    };
    let named_fields = |named: &'a syn::FieldsNamed| -> (Vec<Piece<'a>>, usize, usize) {
        let pieces = named
            .named
            .iter()
            .map(|f| Piece {
                key: f.ident.as_ref().map(|i| i.to_string()).unwrap_or_default(),
                range: line_range(text, range_of(f.span())),
                ty: Some(&f.ty),
            })
            .collect();
        let open = range_of(named.brace_token.span.open()).end;
        let close = range_of(named.brace_token.span.close()).start;
        (pieces, open, close)
    };
    let (body_start, section) = match (item, fields, variants) {
        (syn::Item::Union(u), _, _) => {
            let (pieces, open, close) = named_fields(&u.fields);
            (open - 1, line_section("field", pieces, text, open, close))
        }
        (_, Some(syn::Fields::Named(named)), _) => {
            let (pieces, open, close) = named_fields(named);
            (open - 1, line_section("field", pieces, text, open, close))
        }
        (_, Some(syn::Fields::Unnamed(unnamed)), _) => {
            let open = range_of(unnamed.paren_token.span.open()).end;
            let close = range_of(unnamed.paren_token.span.close()).start;
            let pieces = unnamed
                .unnamed
                .iter()
                .enumerate()
                .map(|(i, f)| Piece {
                    key: i.to_string(),
                    range: range_of(f.span()),
                    ty: Some(&f.ty),
                })
                .collect();
            (
                open - 1,
                Section {
                    label: "field",
                    pieces,
                    anchor: open,
                    end: close,
                    sep: ", ",
                    strip: "",
                },
            )
        }
        (_, Some(syn::Fields::Unit), _) => {
            let end = text.trim_end().trim_end_matches(';').len();
            (
                end,
                Section {
                    label: "field",
                    pieces: Vec::new(),
                    anchor: end,
                    end,
                    sep: ", ",
                    strip: "",
                },
            )
        }
        (syn::Item::Enum(e), _, Some(variants)) => {
            let open = range_of(e.brace_token.span.open()).end;
            let close = range_of(e.brace_token.span.close()).start;
            let pieces = variants
                .iter()
                .map(|v| Piece {
                    key: v.ident.to_string(),
                    range: line_range(text, range_of(v.span())),
                    ty: None,
                })
                .collect();
            (open - 1, line_section("variant", pieces, text, open, close))
        }
        _ => return None,
    };
    let head = Section {
        label: "definition",
        pieces: vec![Piece {
            key: "head".into(),
            range: 0..body_start,
            ty: None,
        }],
        anchor: 0,
        end: body_start,
        sep: "",
        strip: "",
    };
    Some(vec![head, section])
}

/// A section of one piece per line (fields or variants of a multi-line
/// definition). Single-line definitions keep inline pieces.
fn line_section<'a>(
    label: &'static str,
    mut pieces: Vec<Piece<'a>>,
    text: &str,
    open: usize,
    close: usize,
) -> Section<'a> {
    let multi_line = text[open..close].contains('\n');
    if !multi_line {
        // `{ a: u8, b: u8 }` on one line: the pieces are not whole lines.
        for p in &mut pieces {
            let r = p.range.clone();
            let trimmed = text[r.clone()].trim();
            let start = r.start + text[r.clone()].find(trimmed).unwrap_or(0);
            p.range = start..start + trimmed.trim_end_matches(',').len();
        }
    }
    Section {
        label,
        pieces,
        anchor: open,
        end: close,
        sep: if multi_line { "" } else { ", " },
        strip: "",
    }
}

#[derive(Default)]
struct Cx {
    next_g: u32,
    notes: Vec<DiffNote>,
}

/// How a type changed, for notes.
enum TypeChange {
    Same,
    Wrapped(String),
    Unwrapped(String),
    Changed,
}

impl Cx {
    fn g(&mut self) -> u32 {
        self.next_g += 1;
        self.next_g
    }

    fn note(&mut self, text: String, groups: Vec<u32>) {
        self.notes.push(DiffNote { text, groups });
    }

    fn section(&mut self, old: &str, new: &str, o: &Section, n: &Section, edits: &mut Vec<Edit>) {
        let label = n.label;
        // Present on one side only: added or removed whole.
        if o.pieces.is_empty() && !n.pieces.is_empty() {
            let g = self.g();
            edits.push(Edit {
                range: n.anchor..n.end,
                spans: vec![DiffSpan::Added {
                    text: new[n.anchor..n.end].to_string(),
                    g,
                }],
            });
            for p in &n.pieces {
                self.note(
                    format!("{label} {} added", code(&new[p.range.clone()], n.strip)),
                    vec![g],
                );
            }
            return;
        }
        if n.pieces.is_empty() && !o.pieces.is_empty() {
            let g = self.g();
            edits.push(Edit {
                range: n.anchor..n.anchor,
                spans: vec![DiffSpan::Removed {
                    text: old[o.anchor..o.end].to_string(),
                    g,
                }],
            });
            for p in &o.pieces {
                self.note(
                    format!("{label} {} removed", code(&old[p.range.clone()], o.strip)),
                    vec![g],
                );
            }
            return;
        }
        // Matched by key, in the new order.
        let mut matched_old_order = Vec::new();
        for np in &n.pieces {
            let ntext = &new[np.range.clone()];
            match o.pieces.iter().position(|op| op.key == np.key) {
                Some(oi) => {
                    matched_old_order.push(oi);
                    let op = &o.pieces[oi];
                    let otext = &old[op.range.clone()];
                    if otext == ntext {
                        continue;
                    }
                    let (spans, groups, change) = self.piece(old, new, op, np);
                    let what = if np.key == "head" || np.key == "return type" {
                        label.to_string()
                    } else {
                        format!("{label} `{}`", np.key)
                    };
                    let text = match change {
                        TypeChange::Wrapped(w) => format!("{what} wrapped in `{w}`"),
                        TypeChange::Unwrapped(w) => format!("{what} unwrapped from `{w}`"),
                        TypeChange::Changed | TypeChange::Same => {
                            let (a, b) = match (op.ty, np.ty) {
                                (Some(ot), Some(nt))
                                    if prefix_of(old, op, ot) == prefix_of(new, np, nt) =>
                                {
                                    (&old[range_of(ot.span())], &new[range_of(nt.span())])
                                }
                                _ => (otext, ntext),
                            };
                            format!("{what}: {} → {}", code(a, n.strip), code(b, n.strip))
                        }
                    };
                    self.note(text, groups);
                    edits.push(Edit {
                        range: np.range.clone(),
                        spans,
                    });
                }
                None => {
                    let g = self.g();
                    edits.push(Edit {
                        range: np.range.clone(),
                        spans: marked(ntext, true, g),
                    });
                    self.note(format!("{label} {} added", code(ntext, n.strip)), vec![g]);
                }
            }
        }
        if matched_old_order.windows(2).any(|w| w[0] > w[1]) {
            self.note(format!("{label}s reordered"), Vec::new());
        }
        // Removed pieces go after the piece that preceded them in the old
        // list, if it still exists, else before the first new piece.
        for (oi, op) in o.pieces.iter().enumerate() {
            if n.pieces.iter().any(|np| np.key == op.key) {
                continue;
            }
            let otext = &old[op.range.clone()];
            let pred = o.pieces[..oi]
                .iter()
                .rev()
                .find_map(|prev| n.pieces.iter().find(|np| np.key == prev.key));
            let (pos, text) = match pred {
                Some(p) => (p.range.end, format!("{}{otext}", n.sep)),
                None => (n.pieces[0].range.start, format!("{otext}{}", n.sep)),
            };
            let g = self.g();
            edits.push(Edit {
                range: pos..pos,
                spans: marked(&text, false, g),
            });
            self.note(format!("{label} {} removed", code(otext, o.strip)), vec![g]);
        }
    }

    /// Spans for a matched piece whose text changed.
    fn piece(
        &mut self,
        old: &str,
        new: &str,
        op: &Piece,
        np: &Piece,
    ) -> (Vec<DiffSpan>, Vec<u32>, TypeChange) {
        if let (Some(ot), Some(nt)) = (op.ty, np.ty) {
            let (or, nr) = (range_of(ot.span()), range_of(nt.span()));
            // The text around the type must be the same, e.g. `x: ` and `,`.
            if old[op.range.start..or.start] == new[np.range.start..nr.start]
                && old[or.end..op.range.end] == new[nr.end..np.range.end]
            {
                let first = self.next_g + 1;
                let (inner, change) = self.ty(old, ot, new, nt);
                let groups = (first..=self.next_g).collect();
                let mut spans = vec![same(&new[np.range.start..nr.start])];
                spans.extend(inner);
                spans.push(same(&new[nr.end..np.range.end]));
                return (spans, groups, change);
            }
        }
        let first = self.next_g + 1;
        let spans = self.tokens(&old[op.range.clone()], &new[np.range.clone()]);
        (spans, (first..=self.next_g).collect(), TypeChange::Changed)
    }

    /// Compares two types as trees.
    fn ty(
        &mut self,
        old: &str,
        ot: &syn::Type,
        new: &str,
        nt: &syn::Type,
    ) -> (Vec<DiffSpan>, TypeChange) {
        let (or, nr) = (range_of(ot.span()), range_of(nt.span()));
        let (otext, ntext) = (&old[or.clone()], &new[nr.clone()]);
        if otext == ntext {
            return (vec![same(ntext)], TypeChange::Same);
        }
        // Wrapped: the old type (or one of the same shape) is an argument
        // of the new one.
        if let Some(arg) = wrapped_in(old, ot, new, nt) {
            let ar = range_of(arg.span());
            let (inner, _) = self.ty(old, ot, new, arg);
            let wrapper = format!("{}_{}", &new[nr.start..ar.start], &new[ar.end..nr.end]);
            let g = self.g();
            return (
                vec![DiffSpan::Wrap {
                    before: new[nr.start..ar.start].to_string(),
                    inner,
                    after: new[ar.end..nr.end].to_string(),
                    unwrap: false,
                    g,
                }],
                TypeChange::Wrapped(wrapper),
            );
        }
        if let Some(arg) = wrapped_in(new, nt, old, ot) {
            let ar = range_of(arg.span());
            let (inner, _) = self.ty(old, arg, new, nt);
            let wrapper = format!("{}_{}", &old[or.start..ar.start], &old[ar.end..or.end]);
            let g = self.g();
            return (
                vec![DiffSpan::Wrap {
                    before: old[or.start..ar.start].to_string(),
                    inner,
                    after: old[ar.end..or.end].to_string(),
                    unwrap: true,
                    g,
                }],
                TypeChange::Unwrapped(wrapper),
            );
        }
        // Same outer type: compare the arguments.
        if let (Some((oh, oargs)), Some((nh, nargs))) = (path_args(ot), path_args(nt))
            && oh == nh
            && oargs.len() == nargs.len()
            && !oargs.is_empty()
        {
            let mut spans = Vec::new();
            let mut cursor = nr.start;
            for (oa, na) in oargs.iter().zip(&nargs) {
                let ar = range_of(na.span());
                spans.push(same(&new[cursor..ar.start]));
                let (inner, _) = self.ty(old, oa, new, na);
                spans.extend(inner);
                cursor = ar.end;
            }
            spans.push(same(&new[cursor..nr.end]));
            return (spans, TypeChange::Changed);
        }
        if let (syn::Type::Reference(o), syn::Type::Reference(n)) = (ot, nt) {
            let (oe, ne) = (range_of(o.elem.span()), range_of(n.elem.span()));
            if old[or.start..oe.start] == new[nr.start..ne.start] {
                let mut spans = vec![same(&new[nr.start..ne.start])];
                let (inner, _) = self.ty(old, &o.elem, new, &n.elem);
                spans.extend(inner);
                return (spans, TypeChange::Changed);
            }
        }
        let g = self.g();
        (
            vec![
                DiffSpan::Removed {
                    text: otext.to_string(),
                    g,
                },
                DiffSpan::Added {
                    text: ntext.to_string(),
                    g,
                },
            ],
            TypeChange::Changed,
        )
    }

    /// Token-level diff, for pieces without type structure.
    fn tokens(&mut self, old: &str, new: &str) -> Vec<DiffSpan> {
        use similar::{Algorithm, DiffOp, capture_diff_slices};
        let (a, b) = (tokenize(old), tokenize(new));
        let mut spans = Vec::new();
        for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
            match op {
                DiffOp::Equal { new_index, len, .. } => {
                    spans.push(same(&b[new_index..new_index + len].concat()))
                }
                DiffOp::Delete {
                    old_index, old_len, ..
                } => {
                    let g = self.g();
                    spans.push(DiffSpan::Removed {
                        text: a[old_index..old_index + old_len].concat(),
                        g,
                    });
                }
                DiffOp::Insert {
                    new_index, new_len, ..
                } => {
                    let g = self.g();
                    spans.push(DiffSpan::Added {
                        text: b[new_index..new_index + new_len].concat(),
                        g,
                    });
                }
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    let g = self.g();
                    spans.push(DiffSpan::Removed {
                        text: a[old_index..old_index + old_len].concat(),
                        g,
                    });
                    spans.push(DiffSpan::Added {
                        text: b[new_index..new_index + new_len].concat(),
                        g,
                    });
                }
            }
        }
        spans
    }
}

fn prefix_of<'t>(text: &'t str, p: &Piece, ty: &syn::Type) -> &'t str {
    &text[p.range.start..range_of(ty.span()).start]
}

/// The last path segment's name and type arguments, e.g. `Result` and
/// `[f64, Error]` for `std::result::Result<f64, Error>`.
fn path_args(ty: &syn::Type) -> Option<(String, Vec<&syn::Type>)> {
    let syn::Type::Path(tp) = ty else { return None };
    if tp.qself.is_some() {
        return None;
    }
    let last = tp.path.segments.last()?;
    let args = match &last.arguments {
        syn::PathArguments::AngleBracketed(a) => a
            .args
            .iter()
            .filter_map(|arg| match arg {
                syn::GenericArgument::Type(t) => Some(t),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Some((last.ident.to_string(), args))
}

/// If `inner` (from `inner_text`) reappears as a type argument of `outer`,
/// that argument. Exact matches win over arguments of the same shape.
fn wrapped_in<'o>(
    inner_text: &str,
    inner: &syn::Type,
    outer_text: &str,
    outer: &'o syn::Type,
) -> Option<&'o syn::Type> {
    let (_, args) = path_args(outer)?;
    let text = &inner_text[range_of(inner.span())];
    if let Some(a) = args
        .iter()
        .find(|a| &outer_text[range_of(a.span())] == text)
    {
        return Some(a);
    }
    let (inner_head, _) = path_args(inner)?;
    let (outer_head, _) = path_args(outer)?;
    if inner_head == outer_head {
        return None;
    }
    args.into_iter()
        .find(|a| path_args(a).is_some_and(|(h, _)| h == inner_head))
}

fn tokenize(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    for (k, &(_, c)) in chars.iter().enumerate() {
        let next = chars.get(k + 1).map(|&(_, n)| n);
        let joined = next.is_some_and(|n| {
            (c.is_alphanumeric() || c == '_') && (n.is_alphanumeric() || n == '_')
                || c.is_whitespace() && n.is_whitespace()
        });
        if !joined {
            let end = chars.get(k + 1).map_or(s.len(), |&(j, _)| j);
            out.push(&s[start..end]);
            start = end;
        }
    }
    out
}

/// An added or removed text, with surrounding whitespace (indentation,
/// line breaks) kept unmarked.
fn marked(text: &str, added: bool, g: u32) -> Vec<DiffSpan> {
    let core = text.trim();
    let start = text.len() - text.trim_start().len();
    let (lead, rest) = text.split_at(if core.is_empty() { 0 } else { start });
    let (core, trail) = rest.split_at(if core.is_empty() {
        rest.len()
    } else {
        core.len()
    });
    let core = core.to_string();
    let change = if added {
        DiffSpan::Added { text: core, g }
    } else {
        DiffSpan::Removed { text: core, g }
    };
    vec![same(lead), change, same(trail)]
}

fn same(text: &str) -> DiffSpan {
    DiffSpan::Same {
        text: text.to_string(),
    }
}

fn merge_same(spans: Vec<DiffSpan>) -> Vec<DiffSpan> {
    let mut out: Vec<DiffSpan> = Vec::new();
    for s in spans {
        match (out.last_mut(), &s) {
            (_, DiffSpan::Same { text }) if text.is_empty() => {}
            (Some(DiffSpan::Same { text: prev }), DiffSpan::Same { text }) => prev.push_str(text),
            _ => out.push(s),
        }
    }
    out
}

/// Code for a note: whitespace collapsed, trailing comma dropped.
fn code(text: &str, strip: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = collapsed.trim_end_matches(',');
    let t = t.strip_prefix(strip).unwrap_or(t);
    let t = if t.chars().count() > 80 {
        format!("{}…", t.chars().take(79).collect::<String>())
    } else {
        t.to_string()
    };
    format!("`{t}`")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders spans in a compact notation: `[+…+]`, `[-…-]` and `⟦…⟧` for
    /// wraps (`⟦-…-⟧` for unwraps).
    fn show(spans: &[DiffSpan]) -> String {
        spans
            .iter()
            .map(|s| match s {
                DiffSpan::Same { text } => text.clone(),
                DiffSpan::Added { text, .. } => format!("[+{text}+]"),
                DiffSpan::Removed { text, .. } => format!("[-{text}-]"),
                DiffSpan::Wrap {
                    before,
                    inner,
                    after,
                    unwrap,
                    ..
                } => {
                    let m = if *unwrap { "-" } else { "" };
                    format!("⟦{m}{before}{}{after}{m}⟧", show(inner))
                }
            })
            .collect()
    }

    fn notes(d: &SigDiff) -> Vec<&str> {
        d.notes.iter().map(|n| n.text.as_str()).collect()
    }

    #[test]
    fn wrapped_return_type_and_added_parameter() {
        let d = diff(
            Kind::Fn,
            "pub fn evaluate(expr: &Expr) -> f64",
            "pub fn evaluate(expr: &Expr, env: &Env) -> Result<f64, Error>",
        )
        .unwrap();
        assert_eq!(
            show(&d.spans),
            "pub fn evaluate(expr: &Expr, [+env: &Env+]) -> ⟦Result<f64, Error>⟧"
        );
        assert_eq!(
            notes(&d),
            [
                "parameter `env: &Env` added",
                "return type wrapped in `Result<_, Error>`"
            ]
        );
    }

    #[test]
    fn unwrap_and_nested_change() {
        let d = diff(Kind::Fn, "fn f() -> Option<Vec<u8>>", "fn f() -> Vec<u16>").unwrap();
        assert_eq!(show(&d.spans), "fn f() -> ⟦-Option<Vec<[-u8-][+u16+]>>-⟧");
        assert_eq!(notes(&d), ["return type unwrapped from `Option<_>`"]);
    }

    #[test]
    fn parameters_matched_by_name() {
        let d = diff(
            Kind::Method,
            "fn f(&self, a: u8, b: String, c: bool)",
            "fn f(&self, b: &str, a: u16)",
        )
        .unwrap();
        // `c` is shown after `b`, which preceded it in the old order.
        assert_eq!(
            show(&d.spans),
            "fn f(&self, b: [-String-][+&str+][-, c: bool-], a: [-u8-][+u16+])"
        );
        assert_eq!(
            notes(&d),
            [
                "parameter `b`: `String` → `&str`",
                "parameter `a`: `u8` → `u16`",
                "parameters reordered",
                "parameter `c: bool` removed"
            ]
        );
    }

    #[test]
    fn generics_and_return_type_added() {
        let d = diff(Kind::Fn, "fn f(x: u8)", "fn f<T: Send>(x: T) -> T").unwrap();
        assert_eq!(show(&d.spans), "fn f[+<T: Send>+](x: [-u8-][+T+])[+ -> T+]");
    }

    #[test]
    fn enum_variants_by_line() {
        let old = "pub enum Error {\n    A(char),\n    B,\n    C { pos: usize },\n}";
        let new = "pub enum Error {\n    A(char),\n    C { pos: u32 },\n    D(String),\n}";
        let d = diff(Kind::Enum, old, new).unwrap();
        assert_eq!(
            show(&d.spans),
            "pub enum Error {\n    A(char),\n    [-B,-]\n    C { pos: [-usize-][+u32+] },\n    [+D(String),+]\n}"
        );
    }

    #[test]
    fn struct_fields_keep_comments() {
        let old = "pub struct Env {\n    pub lenient: bool,\n    /* private fields */\n}";
        let new = "pub struct Env {\n    pub lenient: Option<bool>,\n    /* private fields */\n}";
        let d = diff(Kind::Struct, old, new).unwrap();
        assert_eq!(
            show(&d.spans),
            "pub struct Env {\n    pub lenient: ⟦Option<bool>⟧,\n    /* private fields */\n}"
        );
        assert_eq!(notes(&d), ["field `lenient` wrapped in `Option<_>`"]);
    }
}
