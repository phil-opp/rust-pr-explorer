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
fn different_code_with_the_same_name_is_not_a_move() {
    let base = write_crate(&[("src/lib.rs", "mod a { pub fn f() -> u8 { 1 } }\nmod b {}\n")]);
    let head = write_crate(&[("src/lib.rs", "mod a {}\nmod b { pub fn f() -> u8 { 2 } }\n")]);
    let out = analyze_dirs(base.path(), head.path());
    assert_eq!(out.items["t::a::f"].status, Status::Removed);
    assert_eq!(out.items["t::b::f"].status, Status::Added);
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
    assert_eq!(calc.unresolved_calls, 0);
    // Without type information, method calls are not resolved (never
    // guessed), but counted.
    let lookup = &out.items["tinycalc::eval::lookup"].info;
    assert!(lookup.calls.is_empty());
    assert_eq!(lookup.unresolved_calls, 2); // env.get(..), name.to_string()
    let uses: Vec<_> = calc.uses.iter().map(|u| u.to.as_str()).collect();
    assert!(uses.contains(&"tinycalc::env::Env"));
}

#[test]
fn shadowing_and_name_spaces() {
    let dir = write_crate(&[(
        "src/lib.rs",
        "pub mod helper {}\n\
         pub fn helper() {}\n\
         pub struct T;\n\
         pub fn x() {}\n\
         pub fn f<T>(t: T) -> T { helper(); let x = 1; let _ = x; t }\n\
         pub fn g() { fn x() {} x(); }\n",
    )]);
    let out = analyze_dirs(dir.path(), dir.path());
    let f = &out.items["t::f"].info;
    // `helper()` is the function, not the module of the same name.
    assert!(f.calls.iter().any(|c| c.to == "t::helper"));
    // `T` is the generic parameter, not the struct; `x` is the local.
    assert!(!f.sig_refs.contains_key("T"));
    assert!(!f.uses.iter().any(|u| u.to == "t::T"));
    assert!(!f.body_refs.contains_key("x"));
    // `x()` calls the function declared inside `g`, not `t::x`.
    let g = &out.items["t::g"].info;
    assert!(g.calls.is_empty());
}

#[test]
fn unknown_globs_hide_the_prelude() {
    let dir = write_crate(&[(
        "src/lib.rs",
        "use somewhere::*;\npub fn f() -> Result<u8, u8> { Ok(1) }\npub fn g() -> Vec<u8> { Vec::new() }\n",
    )]);
    let out = analyze_dirs(dir.path(), dir.path());
    // `somewhere::*` might bring its own `Result`, so no claim is made.
    let f = &out.items["t::f"].info;
    assert!(!f.sig_refs.contains_key("Result"));
}

#[test]
fn glob_reexports_are_public() {
    let dir = write_crate(&[
        ("src/lib.rs", "mod inner;\npub use inner::*;\n"),
        ("src/inner.rs", "pub struct Thing;\nstruct Hidden;\n"),
    ]);
    let out = analyze_dirs(dir.path(), dir.path());
    assert!(out.items["t::inner::Thing"].info.public);
    assert!(!out.items["t::inner::Hidden"].info.public);
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
fn only_real_paths_are_linked() {
    let out = demo();
    // `Op(Op)`: the variant name is not a path; only the type is linked.
    let token = &out.items["tinycalc::parser::lexer::Token"].info;
    let sig: Vec<char> = token.sig.chars().collect();
    let linked: Vec<String> = token
        .sig_links
        .iter()
        .map(|&o| {
            sig[o as usize..]
                .iter()
                .take_while(|c| c.is_alphanumeric())
                .collect()
        })
        .collect();
    assert_eq!(linked, ["String", "Op"]);
    let variant = token.sig[..token.sig.find("Op(Op)").unwrap()]
        .chars()
        .count() as u32;
    assert!(!token.sig_links.contains(&variant));
    assert!(token.sig_links.contains(&(variant + 3)));
    // In the source, links are placed by position: `Parser` in `Parser {`
    // of `Parser::new`, but not the field name `pos`.
    let new = &out.items["tinycalc::parser::Parser::new"].info;
    assert!(
        new.source_refs
            .values()
            .any(|r| r.path == "tinycalc::parser::Parser")
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
    let calls = |a: &crate::analyze::Analysis, id: &str| -> Vec<String> {
        a.items[id]
            .info
            .calls
            .iter()
            .map(|c| c.to.clone())
            .collect()
    };
    // `Parser::new(input)?.parse_expr()`: the method call needs types.
    assert_eq!(
        calls(&analysis, "tinycalc::parser::parse"),
        ["tinycalc::parser::Parser::new"]
    );
    assert_eq!(
        analysis.items["tinycalc::parser::parse"]
            .info
            .unresolved_calls,
        1
    );
    let stats = crate::semantic::refine(
        root,
        std::slice::from_ref(root),
        Trust::Untrusted,
        &mut analysis,
    )
    .unwrap();
    assert!(stats.functions > 10, "{stats:?}");
    assert_eq!(stats.unresolved_sites, 0, "{stats:?}");
    // Names from the standard library are described by rust-analyzer,
    // under the path the code uses for them.
    let string = &analysis.externals["alloc::string::String"];
    assert_eq!(string.kind, model::Kind::Struct);
    assert_eq!(string.module, "alloc::string");
    assert!(
        string.sig.starts_with("pub struct String"),
        "{}",
        string.sig
    );
    assert!(string.docs.starts_with("A UTF-8"), "{}", string.docs);
    let display = &analysis.externals["std::fmt::Display"];
    assert_eq!(
        (display.kind, display.module.as_str()),
        (model::Kind::Trait, "core::fmt")
    );
    assert_eq!(
        calls(&analysis, "tinycalc::parser::parse"),
        [
            "tinycalc::parser::Parser::new",
            "tinycalc::parser::Parser::parse_expr"
        ]
    );
    // `op.precedence()` on a local whose type comes from a pattern.
    assert!(
        calls(&analysis, "tinycalc::parser::Parser::parse_binary")
            .contains(&"tinycalc::ast::Op::precedence".to_string())
    );
    // `op.symbol()` inside `write!(…)`: checked with "go to definition".
    assert!(
        calls(&analysis, "tinycalc::ast::Expr::<impl Display>::fmt")
            .contains(&"tinycalc::ast::Op::symbol".to_string())
    );
    // Method calls are linked by position in the source.
    let parse = &analysis.items["tinycalc::parser::parse"].info;
    assert!(
        parse
            .source_refs
            .values()
            .any(|r| r.id.as_deref() == Some("tinycalc::parser::Parser::parse_expr"))
    );
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

#[test]
fn long_signatures_wrap_like_rustfmt() {
    let dir = write_crate(&[(
        "src/lib.rs",
        "pub struct Expr; pub struct Env; pub struct Error;\n\
         pub fn evaluate_expression(expression: &Expr, environment: &Env) -> Result<f64, Error> { todo!() }\n\
         pub fn short(a: u8) -> u8 { a }\n\
         pub fn bounded<T>(t: T) -> T where T: Clone + Send { t }\n",
    )]);
    let out = analyze_dirs(dir.path(), dir.path());
    assert_eq!(
        out.items["t::evaluate_expression"].info.sig,
        "pub fn evaluate_expression(\n    expression: &Expr,\n    environment: &Env,\n) -> Result<f64, Error>"
    );
    assert_eq!(out.items["t::short"].info.sig, "pub fn short(a: u8) -> u8");
    assert_eq!(
        out.items["t::bounded"].info.sig,
        "pub fn bounded<T>(t: T) -> T\nwhere\n    T: Clone + Send,"
    );
}
