//! End-to-end tests of the analysis on the demo fixture and small inline crates.

use crate::analyze::{Analyzer, Kind};
use crate::merge::{self, Output, Status};
use std::path::Path;

fn analyze_dirs(base: &Path, head: &Path) -> Output {
    let base_an = Analyzer::run(base, &crate::source::find_crates(base));
    let head_an = Analyzer::run(head, &crate::source::find_crates(head));
    merge::merge(
        base_an,
        head_an,
        merge::Meta {
            title: String::new(),
            base: String::new(),
            head: String::new(),
            url: None,
        },
    )
}

fn demo() -> Output {
    analyze_dirs(
        Path::new("fixtures/demo/base"),
        Path::new("fixtures/demo/head"),
    )
}

/// Writes a one-crate tree with the given files into a temp dir.
fn write_crate(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"t\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    for (path, text) in files {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

#[test]
fn statuses_in_demo() {
    let out = demo();
    let status = |id: &str| out.items[id].status;
    assert_eq!(status("tinycalc::env::Env"), Status::Added);
    assert_eq!(status("tinycalc::eval::lookup"), Status::Added);
    assert_eq!(status("tinycalc::ast::Op::precedence"), Status::Unchanged);
    let evaluate = &out.items["tinycalc::eval::evaluate"];
    assert_eq!(evaluate.status, Status::Changed);
    assert!(evaluate.changes.signature && evaluate.changes.body && evaluate.changes.docs);
    let factor = &out.items["tinycalc::parser::Parser::parse_factor"];
    assert!(!factor.changes.signature && factor.changes.body);
}

#[test]
fn import_style_and_formatting_are_not_changes() {
    let out = demo();
    // `use crate::Error` → `crate::Error` in the signature, and reformatting.
    let new = &out.items["tinycalc::parser::Parser::new"];
    assert_eq!(new.status, Status::Unchanged);
    assert!(new.changes.cosmetic);
    let lex = &out.items["tinycalc::parser::lexer::lex_number"];
    assert_eq!(lex.status, Status::Unchanged);
    assert!(lex.changes.cosmetic);
}

#[test]
fn moved_items_are_detected() {
    let out = demo();
    let moved = &out.items["tinycalc::ast::format_number"];
    assert_eq!(moved.status, Status::Changed);
    assert_eq!(
        moved.changes.moved_from.as_deref(),
        Some("tinycalc::eval::format_number")
    );
    assert!(!out.items.contains_key("tinycalc::eval::format_number"));
}

#[test]
fn calls_and_uses() {
    let out = demo();
    let calc = &out.items["tinycalc::calc"].data;
    let callees: Vec<_> = calc.calls.iter().map(|c| c.to.as_str()).collect();
    assert_eq!(
        callees,
        ["tinycalc::eval::evaluate", "tinycalc::parser::parse"]
    );
    // `env.get(..)` resolves through the parameter type `&Env`.
    let lookup = &out.items["tinycalc::eval::lookup"].data;
    assert!(
        lookup
            .calls
            .iter()
            .any(|c| c.to == "tinycalc::env::Env::get" && !c.inferred)
    );
    // `self.tokens.get(..)` must not be mistaken for `Env::get`.
    let peek = &out.items["tinycalc::parser::Parser::peek"].data;
    assert!(peek.calls.is_empty());
    let uses: Vec<_> = calc.uses.iter().map(|u| u.to.as_str()).collect();
    assert!(uses.contains(&"tinycalc::env::Env"));
}

#[test]
fn visibility_and_public_api() {
    let out = demo();
    let item = |id: &str| &out.items[id].data;
    let tokenize = item("tinycalc::parser::lexer::tokenize");
    assert_eq!(tokenize.vis_label, "pub(super)");
    assert_eq!(tokenize.vis_scope.as_deref(), Some("tinycalc::parser"));
    assert!(!tokenize.public);
    // Public through `pub use eval::evaluate` even though `eval` is private.
    assert!(item("tinycalc::eval::evaluate").public);
    assert!(!item("tinycalc::eval::apply").public);
    assert!(item("tinycalc::parser::Parser::parse_expr").public);
    assert!(!item("tinycalc::parser::Parser::next").public);
    let env = item("tinycalc::env::Env");
    assert!(env.sig.contains("vars: HashMap<String, f64>"));
    assert!(
        env.sig_public
            .as_deref()
            .unwrap()
            .contains("/* private fields */")
    );
}

#[test]
fn signatures_use_short_names_with_full_paths_on_hover() {
    let out = demo();
    let evaluate = &out.items["tinycalc::eval::evaluate"].data;
    assert_eq!(
        evaluate.sig,
        "pub fn evaluate(expr: &Expr, env: &Env) -> Result<f64, Error>"
    );
    assert_eq!(
        evaluate.sig_refs["Env"].id.as_deref(),
        Some("tinycalc::env::Env")
    );
    assert_eq!(evaluate.sig_refs["Result"].path, "core::result::Result");
}

#[test]
fn trait_impls_and_intra_doc_links() {
    let out = demo();
    let fmt = &out.items["tinycalc::ast::Expr::<impl Display>::fmt"].data;
    assert_eq!(fmt.kind, Kind::Method);
    assert_eq!(fmt.impl_header.as_deref(), Some("impl Display for Expr"));
    let expr = &out.items["tinycalc::ast::Expr"].data;
    assert!(
        expr.children
            .contains(&"tinycalc::ast::Expr::<impl Display>".to_string())
    );
    let env = &out.items["tinycalc::env::Env"].data;
    assert_eq!(
        env.doc_refs["crate::evaluate"].id.as_deref(),
        Some("tinycalc::eval::evaluate")
    );
}

#[test]
fn renamed_import_resolves_and_changing_target_is_a_change() {
    let base = write_crate(&[(
        "src/lib.rs",
        "mod a { pub struct X; }\nmod b { pub struct X; }\nuse a::X as Y;\npub fn f(_: Y) {}\n",
    )]);
    let head = write_crate(&[(
        "src/lib.rs",
        "mod a { pub struct X; }\nmod b { pub struct X; }\nuse b::X as Y;\npub fn f(_: Y) {}\n",
    )]);
    let out = analyze_dirs(base.path(), head.path());
    let f = &out.items["t::f"];
    // Same text, but `Y` now names a different type.
    assert_eq!(f.status, Status::Changed);
    assert!(f.changes.signature);
    assert_eq!(f.data.sig_refs["Y"].id.as_deref(), Some("t::b::X"));
}

#[test]
fn glob_imports_super_and_nested_files() {
    let dir = write_crate(&[
        ("src/lib.rs", "pub mod outer;\npub struct Root;\n"),
        (
            "src/outer.rs",
            "pub mod inner;\nuse inner::*;\npub fn g() -> Thing { make() }\n",
        ),
        (
            "src/outer/inner.rs",
            "pub struct Thing;\npub fn make() -> Thing { let _ = super::super::Root; Thing }\n",
        ),
    ]);
    let out = analyze_dirs(dir.path(), dir.path());
    let g = &out.items["t::outer::g"].data;
    assert_eq!(
        g.sig_refs["Thing"].id.as_deref(),
        Some("t::outer::inner::Thing")
    );
    assert!(g.calls.iter().any(|c| c.to == "t::outer::inner::make"));
    let make = &out.items["t::outer::inner::make"].data;
    assert!(make.uses.iter().any(|u| u.to == "t::Root"));
    assert!(out.items.values().all(|i| i.status == Status::Unchanged));
}
