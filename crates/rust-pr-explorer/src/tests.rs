//! End-to-end tests of the analysis on the demo fixture and small inline crates.

use crate::analyze::Analyzer;
use crate::merge;
use crate::source::Trust;
use model::{Kind, Meta, Output, Status};
use std::path::Path;

fn analyze_dirs(base: &Path, head: &Path) -> Output {
    let base_an = Analyzer::run(base, &crate::source::find_crates(base));
    let head_an = Analyzer::run(head, &crate::source::find_crates(head));
    merge::merge(
        base_an,
        head_an,
        Meta {
            title: String::new(),
            base: String::new(),
            head: String::new(),
            url: None,
            resolution: String::new(),
        },
    )
}

fn fixture(side: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/demo")
        .join(side)
}

fn demo() -> Output {
    // Syntax only; see `semantic_calls` for the rust-analyzer step.
    analyze_dirs(&fixture("base"), &fixture("head"))
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
    let calc = &out.items["tinycalc::calc"].info;
    let callees: Vec<_> = calc.calls.iter().map(|c| c.to.as_str()).collect();
    assert_eq!(
        callees,
        ["tinycalc::eval::evaluate", "tinycalc::parser::parse"]
    );
    // `env.get(..)` resolves through the parameter type `&Env`.
    let lookup = &out.items["tinycalc::eval::lookup"].info;
    assert!(
        lookup
            .calls
            .iter()
            .any(|c| c.to == "tinycalc::env::Env::get" && !c.inferred)
    );
    // `self.tokens.get(..)` must not be mistaken for `Env::get`.
    let peek = &out.items["tinycalc::parser::Parser::peek"].info;
    assert!(peek.calls.is_empty());
    let uses: Vec<_> = calc.uses.iter().map(|u| u.to.as_str()).collect();
    assert!(uses.contains(&"tinycalc::env::Env"));
}

#[test]
fn visibility_and_public_api() {
    let out = demo();
    let item = |id: &str| &out.items[id].info;
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
    let evaluate = &out.items["tinycalc::eval::evaluate"].info;
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
    let fmt = &out.items["tinycalc::ast::Expr::<impl Display>::fmt"].info;
    assert_eq!(fmt.kind, Kind::Method);
    assert_eq!(fmt.impl_header.as_deref(), Some("impl Display for Expr"));
    let expr = &out.items["tinycalc::ast::Expr"].info;
    assert!(
        expr.children
            .contains(&"tinycalc::ast::Expr::<impl Display>".to_string())
    );
    let env = &out.items["tinycalc::env::Env"].info;
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
    assert_eq!(f.info.sig_refs["Y"].id.as_deref(), Some("t::b::X"));
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
    let g = &out.items["t::outer::g"].info;
    assert_eq!(
        g.sig_refs["Thing"].id.as_deref(),
        Some("t::outer::inner::Thing")
    );
    assert!(g.calls.iter().any(|c| c.to == "t::outer::inner::make"));
    let make = &out.items["t::outer::inner::make"].info;
    assert!(make.uses.iter().any(|u| u.to == "t::Root"));
    assert!(out.items.values().all(|i| i.status == Status::Unchanged));
}

/// rust-analyzer resolves calls whose receiver type the syntactic pass
/// cannot know, and replaces guesses.
#[cfg(feature = "semantic")]
#[test]
fn semantic_calls() {
    let checkout =
        crate::source::Checkout::from_dirs(&fixture("base"), &fixture("head"), Trust::Untrusted)
            .unwrap();
    let root = &checkout.head_dir;
    let mut analysis = Analyzer::run(root, &crate::source::find_crates(root));
    let calls = |a: &crate::analyze::Analysis, id: &str| -> Vec<(String, bool)> {
        a.items[id]
            .info
            .calls
            .iter()
            .map(|c| (c.to.clone(), c.inferred))
            .collect()
    };
    // `Parser::new(input)?.parse_expr()`: the receiver is an expression.
    assert_eq!(
        calls(&analysis, "tinycalc::parser::parse"),
        [
            ("tinycalc::parser::Parser::new".to_string(), false),
            ("tinycalc::parser::Parser::parse_expr".to_string(), true),
        ]
    );
    let stats = crate::semantic::refine(
        root,
        std::slice::from_ref(root),
        Trust::Untrusted,
        &mut analysis,
    )
    .unwrap();
    assert!(stats.functions > 10, "{stats:?}");
    assert_eq!(
        calls(&analysis, "tinycalc::parser::parse"),
        [
            ("tinycalc::parser::Parser::new".to_string(), false),
            ("tinycalc::parser::Parser::parse_expr".to_string(), false),
        ]
    );
    // `op.precedence()` on a local whose type comes from a pattern.
    assert!(
        calls(&analysis, "tinycalc::parser::Parser::parse_binary")
            .contains(&("tinycalc::ast::Op::precedence".to_string(), false))
    );
    let parse = &analysis.items["tinycalc::parser::parse"].info;
    assert!(!parse.body_refs[".parse_expr"].inferred);
}

#[test]
fn checkouts_drop_tool_configuration_and_symlinks() {
    let dir = write_crate(&[
        ("src/lib.rs", "pub fn f() {}\n"),
        (".cargo/config.toml", "[build]\nrustc = \"./not-rustc\"\n"),
        ("sub/.cargo/config", "[build]\nrustc-wrapper = \"./wrap\"\n"),
        (
            "rust-toolchain.toml",
            "[toolchain]\npath = \"./toolchain\"\n",
        ),
    ]);
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/hostname", dir.path().join("src/leak.rs")).unwrap();
    let checkout =
        crate::source::Checkout::from_dirs(dir.path(), dir.path(), Trust::Untrusted).unwrap();
    for side in [&checkout.base_dir, &checkout.head_dir] {
        assert!(side.join("src/lib.rs").is_file());
        assert!(!side.join(".cargo/config.toml").exists());
        assert!(!side.join("sub/.cargo/config").exists());
        assert!(!side.join("rust-toolchain.toml").exists());
        assert!(!side.join("src/leak.rs").exists());
    }
    // In a trusted (sandboxed) environment the configuration is kept, but
    // symlinks are still dropped.
    let trusted =
        crate::source::Checkout::from_dirs(dir.path(), dir.path(), Trust::Trusted).unwrap();
    assert!(trusted.head_dir.join(".cargo/config.toml").is_file());
    assert!(trusted.head_dir.join("rust-toolchain.toml").is_file());
    assert!(!trusted.head_dir.join("src/leak.rs").exists());
    // The originals are untouched.
    assert!(dir.path().join(".cargo/config.toml").is_file());
    assert!(dir.path().join("rust-toolchain.toml").is_file());
}
