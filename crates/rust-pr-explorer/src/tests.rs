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
            pr: None,
            project: None,
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
fn moved_and_edited_items_are_detected() {
    let f = |extra: &str| {
        format!(
            "pub fn total(xs: &[u32], scale: u32) -> u32 {{\n\
             let mut sum = 0;\n\
             for x in xs {{ sum += x * scale; }}\n\
             {extra}sum\n\
             }}\n"
        )
    };
    let base = write_crate(&[(
        "src/lib.rs",
        &format!(
            "mod a {{ {} pub struct S {{ pub x: u8, pub y: u8, pub z: u8 }} }}\nmod b {{}}\n",
            f("")
        ),
    )]);
    let head = write_crate(&[(
        "src/lib.rs",
        &format!(
            "mod a {{}}\nmod b {{ {} pub struct S {{ pub x: u8, pub y: u8, pub z: u8, pub w: u8 }} }}\n",
            f("if sum > 100 { return 100; }\n")
        ),
    )]);
    let out = analyze_dirs(base.path(), head.path());
    let total = &out.items["t::b::total"];
    assert_eq!(total.status, Status::Changed);
    assert_eq!(total.changes.moved_from.as_deref(), Some("t::a::total"));
    assert!(total.changes.body && !total.changes.signature);
    let s = &out.items["t::b::S"];
    assert_eq!(s.changes.moved_from.as_deref(), Some("t::a::S"));
    assert!(s.changes.signature);
    assert!(!out.items.contains_key("t::a::total"));
    assert!(!out.items.contains_key("t::a::S"));
}

#[test]
fn rewritten_items_with_the_same_name_are_not_a_move() {
    let base = write_crate(&[(
        "src/lib.rs",
        "mod a { pub fn parse(s: &str) -> Option<u32> { s.trim().parse().ok() } }\nmod b {}\n",
    )]);
    let head = write_crate(&[(
        "src/lib.rs",
        "mod a {}\nmod b { pub fn parse(s: &str) -> Option<u32> { \
         let mut n = 0; for c in s.chars() { n = n * 10 + c.to_digit(10)?; } Some(n) } }\n",
    )]);
    let out = analyze_dirs(base.path(), head.path());
    assert_eq!(out.items["t::a::parse"].status, Status::Removed);
    assert_eq!(out.items["t::b::parse"].status, Status::Added);
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
    // Both are kept; the function, defined second, is tagged with its
    // kind.
    assert!(
        f.calls.iter().any(|c| c.to == "t::helper [fn]"),
        "{:?}",
        f.calls
    );
    assert!(out.items.contains_key("t::helper"));
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
fn public_paths_follow_reexports() {
    let dir = write_crate(&[
        (
            "src/lib.rs",
            "mod inner;\npub mod api;\npub use inner::*;\npub use inner::Thing as Alias;\n",
        ),
        ("src/inner.rs", "pub struct Thing;\nstruct Hidden;\n"),
        (
            "src/api.rs",
            "pub use crate::inner::Thing;\npub fn f() {}\n",
        ),
    ]);
    let out = analyze_dirs(dir.path(), dir.path());
    assert_eq!(
        out.items["t::inner::Thing"].info.public_paths,
        ["t::Alias", "t::Thing", "t::api::Thing"]
    );
    assert_eq!(out.items["t::api::f"].info.public_paths, ["t::api::f"]);
    assert!(out.items["t::inner::Hidden"].info.public_paths.is_empty());
    // The private definition path is not a public path.
    assert!(out.items["t::inner"].info.public_paths.is_empty());
}

#[test]
fn reexport_changes_are_changes() {
    let base = write_crate(&[
        ("src/lib.rs", "mod inner;\npub use inner::Old;\n"),
        ("src/inner.rs", "pub struct Old;\npub struct New;\n"),
    ]);
    let head = write_crate(&[
        ("src/lib.rs", "mod inner;\npub use inner::New;\n"),
        ("src/inner.rs", "pub struct Old;\npub struct New;\n"),
    ]);
    let out = analyze_dirs(base.path(), head.path());
    let new = &out.items["t::inner::New"];
    assert_eq!(new.status, Status::Changed);
    assert_eq!(new.changes.exports_added, ["t::New"]);
    assert!(new.info.public);
    let old = &out.items["t::inner::Old"];
    assert_eq!(old.status, Status::Changed);
    assert_eq!(old.changes.exports_removed, ["t::Old"]);
    assert!(!old.info.public);
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
        // `exists()` would follow the link.
        assert!(std::fs::symlink_metadata(side.join("src/leak.rs")).is_err());
    }
    // In a trusted (sandboxed) environment the configuration is kept, but
    // symlinks are still dropped.
    let trusted =
        crate::source::Checkout::from_dirs(dir.path(), dir.path(), Trust::Trusted).unwrap();
    assert!(trusted.head_dir.join(".cargo/config.toml").is_file());
    assert!(trusted.head_dir.join("rust-toolchain.toml").is_file());
    assert!(std::fs::symlink_metadata(trusted.head_dir.join("src/leak.rs")).is_err());
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

#[test]
fn long_trait_methods_wrap_like_rustfmt() {
    let dir = write_crate(&[(
        "src/lib.rs",
        "pub struct Key; pub struct Value; pub struct Error;\n\
         pub trait Visitor { fn visit_pair(&mut self, key: Key, value: Value) -> Result<(), Error>; fn short(&self); }\n",
    )]);
    let out = analyze_dirs(dir.path(), dir.path());
    assert_eq!(
        out.items["t::Visitor"].info.sig,
        "pub trait Visitor {\n    fn visit_pair(\n        &mut self,\n        key: Key,\n        value: Value,\n    ) -> Result<(), Error>;\n    fn short(&self);\n}"
    );
}

#[test]
fn binaries_tests_and_build_scripts_are_analyzed() {
    let dir = write_crate(&[
        ("src/lib.rs", "pub fn api() {}\n"),
        ("src/main.rs", "fn main() { t::api(); }\n"),
        (
            "src/bin/tool.rs",
            "pub fn helper() {}\nfn main() { helper(); }\n",
        ),
        ("build.rs", "fn main() {}\n"),
        (
            "tests/it.rs",
            "use t::api;\nfn setup() {}\n#[test]\nfn works() { setup(); api(); }\n",
        ),
        ("examples/demo/main.rs", "fn main() {}\n"),
    ]);
    let out = analyze_dirs(dir.path(), dir.path());
    let mut crates = out.crates.clone();
    crates.sort();
    assert_eq!(
        crates,
        [
            "bin:t",
            "bin:tool",
            "build:t",
            "example:demo",
            "t",
            "test:it"
        ]
    );
    // A binary's `pub` items are not public API.
    assert!(!out.items["bin:tool::helper"].info.public);
    assert!(out.items["t::api"].info.public);
    // The other targets use the library by its name.
    let main = &out.items["bin:t::main"].info;
    let main_calls: Vec<&str> = main.calls.iter().map(|c| c.to.as_str()).collect();
    assert_eq!(main_calls, ["t::api"]);
    // In tests, `#[test]` functions are what the target is about.
    let works = &out.items["test:it::works"].info;
    let calls: Vec<&str> = works.calls.iter().map(|c| c.to.as_str()).collect();
    assert_eq!(calls, ["t::api", "test:it::setup"]);
}

#[test]
fn traits_with_associated_types() {
    let base = write_crate(&[("src/lib.rs", "pub trait Shape { type Unit; }\n")]);
    let head = write_crate(&[("src/lib.rs", "pub trait Shape { type Unit: Copy; }\n")]);
    let out = analyze_dirs(base.path(), head.path());
    let unit = &out.items["t::Shape::Unit"];
    assert_eq!(unit.info.sig, "type Unit: Copy;");
    assert_eq!(unit.status, Status::Changed);
}

#[test]
fn files_outside_the_checkout_are_not_read() {
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.rs");
    std::fs::write(&secret, "pub fn leaked() {}\n").unwrap();
    let dir = write_crate(&[
        (
            "src/lib.rs",
            &format!(
                "#[path = \"{}\"]\npub mod m;\n#[path = \"lib.rs\"]\npub mod again;\npub fn ok() {{}}\n",
                secret.display()
            ),
        ),
        ("src/bin/b.rs", "fn main() {}\n"),
    ]);
    // A manifest target pointing outside, too.
    let manifest = dir.path().join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        format!(
            "{text}[[bin]]\nname = \"x\"\npath = \"{}\"\n",
            secret.display()
        ),
    )
    .unwrap();
    let crates = crate::source::find_crates(dir.path());
    let analysis = Analyzer::run(dir.path(), &crates);
    assert!(analysis.items.contains_key("t::ok"));
    assert!(
        analysis.items.keys().all(|id| !id.contains("leaked")),
        "{:?}",
        analysis.items.keys()
    );
    let warnings = analysis.warnings.join("\n");
    assert!(warnings.contains("outside the checkout"), "{warnings}");
    // `#[path = "lib.rs"]` inside lib.rs: refused, no endless recursion.
    assert!(warnings.contains("included inside itself"), "{warnings}");
}

#[test]
fn one_file_as_two_modules() {
    // rustc accepts this: two unrelated modules from one file.
    let dir = write_crate(&[
        (
            "src/lib.rs",
            "#[path = \"common.rs\"]\npub mod a;\n#[path = \"common.rs\"]\npub mod b;\n",
        ),
        ("src/common.rs", "pub fn shared() {}\n"),
    ]);
    let crates = crate::source::find_crates(dir.path());
    let analysis = Analyzer::run(dir.path(), &crates);
    assert!(analysis.items.contains_key("t::a::shared"));
    assert!(analysis.items.contains_key("t::b::shared"));
    assert!(analysis.warnings.is_empty(), "{:?}", analysis.warnings);
}

/// Files that each include the next file twice double the number of
/// modules with every file; the analysis stops at a limit.
#[test]
fn repeated_includes_are_limited() {
    let mut files = vec![(
        "src/lib.rs".to_string(),
        "#[path = \"f1.rs\"]\npub mod a;\n#[path = \"f1.rs\"]\npub mod b;\n".to_string(),
    )];
    let levels = 18;
    for n in 1..levels {
        files.push((
            format!("src/f{n}.rs"),
            format!(
                "#[path = \"f{0}.rs\"]\npub mod a;\n#[path = \"f{0}.rs\"]\npub mod b;\npub fn f() {{}}\n",
                n + 1
            ),
        ));
    }
    files.push((format!("src/f{levels}.rs"), "pub fn f() {}\n".to_string()));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let dir = write_crate(&refs);
    let start = std::time::Instant::now();
    let crates = crate::source::find_crates(dir.path());
    let analysis = Analyzer::run(dir.path(), &crates);
    assert!(start.elapsed().as_secs() < 30, "{:?}", start.elapsed());
    let modules = analysis
        .items
        .values()
        .filter(|i| i.info.kind == model::Kind::Mod)
        .count();
    // Every file once, plus the limited repeats.
    assert!(modules <= levels + 1 + 1000, "{modules}");
    let limit: Vec<&String> = analysis
        .warnings
        .iter()
        .filter(|w| w.contains("further modules"))
        .collect();
    assert_eq!(limit.len(), 1, "{:?}", analysis.warnings);
}

#[test]
fn embedded_data_cannot_end_its_script_element() {
    let json = serde_json::to_string("Strips `<!--<script>` prefixes. </script>").unwrap();
    let page = crate::render_page("t", &json);
    let start = page
        .find(r#"<script id="data" type="application/json">"#)
        .unwrap();
    let end = start + page[start..].find("</script>").unwrap();
    let data = &page[start..end];
    assert!(!data[1..].contains('<'), "{data}");
    // Still the same JSON.
    let inner = &data[data.find('>').unwrap() + 1..];
    assert_eq!(
        serde_json::from_str::<String>(inner).unwrap(),
        "Strips `<!--<script>` prefixes. </script>"
    );
}

/// In `git` mode, files come from a worktree: symlinks committed to the
/// repository are removed from the copies (not just never copied), and
/// `export-ignore` can't hide files. Everything removed is still listed.
#[cfg(unix)]
#[test]
fn git_checkouts_list_and_remove_what_they_must() {
    let dir = write_crate(&[("src/lib.rs", "pub fn f() {}\n")]);
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "base"]);
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "mod hidden;\npub fn f() {}\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/hidden.rs"), "pub fn h() {}\n").unwrap();
    std::fs::write(
        dir.path().join(".gitattributes"),
        "src/hidden.rs export-ignore\n.gitattributes export-ignore\n",
    )
    .unwrap();
    std::fs::create_dir(dir.path().join(".cargo")).unwrap();
    std::fs::write(
        dir.path().join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = \"x\"\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("rust-toolchain.toml"), "[toolchain]\n").unwrap();
    std::os::unix::fs::symlink("/etc/hostname", dir.path().join("src/leak.rs")).unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "head"]);
    let checkout =
        crate::source::Checkout::from_git(dir.path(), "HEAD~1", "HEAD", Trust::Untrusted).unwrap();
    let head = &checkout.head_dir;
    assert!(head.join("src/hidden.rs").is_file());
    assert!(std::fs::symlink_metadata(head.join("src/leak.rs")).is_err());
    assert!(!head.join(".cargo/config.toml").exists());
    assert!(!head.join("rust-toolchain.toml").exists());
    let listed: Vec<(&str, &str)> = checkout
        .files
        .iter()
        .map(|f| (f.path.as_str(), f.lang.as_str()))
        .collect();
    for expected in [
        (".cargo/config.toml", "toml"),
        ("rust-toolchain.toml", "toml"),
        ("src/hidden.rs", "rust"),
        ("src/leak.rs", "symlink"),
    ] {
        assert!(listed.contains(&expected), "{expected:?} in {listed:?}");
    }
    // The symlink is listed by its target, never followed.
    let leak = checkout
        .files
        .iter()
        .find(|f| f.path == "src/leak.rs")
        .unwrap();
    assert_eq!(leak.hunks[1].1, "symbolic link to /etc/hostname");

    // A subdirectory of the repository: only it is analyzed and listed,
    // and configuration above it is still removed.
    let sub = crate::source::Checkout::from_git(
        &dir.path().join("src"),
        "HEAD~1",
        "HEAD",
        Trust::Untrusted,
    )
    .unwrap();
    assert!(sub.head_dir.join("hidden.rs").is_file());
    assert!(
        !sub.head_dir
            .parent()
            .unwrap()
            .join(".cargo/config.toml")
            .exists()
    );
    let listed: Vec<&str> = sub.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(listed, ["hidden.rs", "leak.rs", "lib.rs"]);
    assert!(sub.title.ends_with(" in src"), "{}", sub.title);
    // Review marks are kept per project: the repository and the
    // subdirectory are different projects, and the key is not a path.
    let key = checkout.project.as_deref().unwrap();
    assert_eq!(key.len(), 16);
    assert_ne!(sub.project.as_deref(), Some(key));

    // A bare repository has no top-level directory: it is cloned as it is.
    let bare = tempfile::tempdir().unwrap();
    let bare_path = bare.path().join("repo.git");
    let out = std::process::Command::new("git")
        .args(["clone", "-q", "--bare"])
        .arg(dir.path())
        .arg(&bare_path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let from_bare =
        crate::source::Checkout::from_git(&bare_path, "HEAD~1", "HEAD", Trust::Untrusted).unwrap();
    assert!(from_bare.head_dir.join("src/hidden.rs").is_file());
    assert_eq!(from_bare.files.len(), checkout.files.len());
}

/// In `dirs` mode, build output and downloaded packages are neither copied
/// nor listed.
#[test]
fn dirs_checkouts_skip_build_output() {
    let base = write_crate(&[("src/lib.rs", "pub fn f() {}\n")]);
    let head = write_crate(&[
        ("src/lib.rs", "pub fn f() {}\npub fn g() {}\n"),
        ("target/debug/demo.d", "x\n"),
        ("node_modules/x/i.js", "x\n"),
        ("sub/target/out.txt", "x\n"),
    ]);
    let checkout =
        crate::source::Checkout::from_dirs(base.path(), head.path(), Trust::Untrusted).unwrap();
    let listed: Vec<&str> = checkout.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(listed, ["src/lib.rs"]);
    // Another pair of directories is another project.
    let other =
        crate::source::Checkout::from_dirs(head.path(), base.path(), Trust::Untrusted).unwrap();
    assert!(checkout.project.is_some());
    assert_ne!(other.project, checkout.project);
}

#[test]
fn variants_of_one_path_are_all_kept() {
    let base = write_crate(&[(
        "src/lib.rs",
        "pub struct W<T>(T);\npub trait Show { fn show(&self) -> u8; }\n\
         #[cfg(unix)]\npub fn sep() -> char { '/' }\n#[cfg(windows)]\npub fn sep() -> char { '\\\\' }\n\
         impl Show for W<u8> { fn show(&self) -> u8 { 1 } }\nimpl Show for W<u16> { fn show(&self) -> u8 { 2 } }\n",
    )]);
    let head = write_crate(&[(
        "src/lib.rs",
        "pub struct W<T>(T);\npub trait Show { fn show(&self) -> u8; }\n\
         #[cfg(unix)]\npub fn sep() -> char { '/' }\n#[cfg(windows)]\npub fn sep() -> char { '|' }\n\
         impl Show for W<u8> { fn show(&self) -> u8 { 1 } }\nimpl Show for W<u16> { fn show(&self) -> u8 { 3 } }\n",
    )]);
    let out = analyze_dirs(base.path(), head.path());
    let changed: Vec<&str> = out
        .items
        .values()
        .filter(|i| i.status == Status::Changed && i.info.kind != Kind::Mod)
        .map(|i| i.info.id.as_str())
        .collect();
    assert_eq!(
        changed,
        [
            "t::W::<impl Show for W<u16>>::show",
            "t::sep [cfg(windows)]"
        ]
    );
}

#[test]
fn ambiguous_associated_functions_are_not_resolved() {
    let dir = write_crate(&[(
        "src/lib.rs",
        "pub struct S;\nimpl From<u8> for S { fn from(_: u8) -> S { S } }\n\
         impl From<String> for S { fn from(_: String) -> S { S } }\n\
         pub fn make(s: String) -> S { S::from(s) }\n",
    )]);
    let out = analyze_dirs(dir.path(), dir.path());
    let make = &out.items["t::make"].info;
    assert!(make.calls.is_empty(), "{:?}", make.calls);
    assert_eq!(make.unresolved_calls, 1);
}

#[test]
fn glob_reexports_forward_reexports() {
    let dir = write_crate(&[
        ("src/lib.rs", "mod imp;\npub use imp::*;\n"),
        ("src/imp/mod.rs", "mod a;\npub use a::Foo;\n"),
        ("src/imp/a.rs", "pub struct Foo { pub x: u8 }\n"),
    ]);
    let out = analyze_dirs(dir.path(), dir.path());
    let foo = &out.items["t::imp::a::Foo"].info;
    assert!(foo.public);
    assert!(
        foo.public_paths.contains(&"t::Foo".to_string()),
        "{:?}",
        foo.public_paths
    );
}

#[test]
fn swapping_targets_with_the_same_name_is_a_change() {
    let base = write_crate(&[(
        "src/lib.rs",
        "use std::{fmt, io};\npub mod a { pub fn run() {} }\npub mod b { pub fn run() {} }\n\
         pub fn f(_a: io::Error, _b: fmt::Error) {}\npub fn g() { a::run(); a::run(); b::run(); }\n",
    )]);
    let head = write_crate(&[(
        "src/lib.rs",
        "use std::{fmt, io};\npub mod a { pub fn run() {} }\npub mod b { pub fn run() {} }\n\
         pub fn f(_a: fmt::Error, _b: io::Error) {}\npub fn g() { a::run(); b::run(); b::run(); }\n",
    )]);
    let out = analyze_dirs(base.path(), head.path());
    let f = &out.items["t::f"];
    assert_eq!(f.status, Status::Changed);
    assert!(f.changes.signature && !f.changes.cosmetic);
    let g = &out.items["t::g"];
    assert!(g.changes.body && !g.changes.cosmetic);
}

#[test]
fn renamed_modules_keep_their_items() {
    let base = write_crate(&[(
        "src/lib.rs",
        "pub mod old {\n    pub struct S { pub t: T }\n    pub struct T;\n    impl S { pub fn new() -> Self { make() } }\n    fn make() -> S { S { t: T } }\n}\n",
    )]);
    let head = write_crate(&[(
        "src/lib.rs",
        "pub mod new {\n    pub struct S { pub t: T }\n    pub struct T;\n    impl S { pub fn new() -> Self { make() } }\n    fn make() -> S { S { t: T } }\n}\n",
    )]);
    let out = analyze_dirs(base.path(), head.path());
    for id in ["t::new::S", "t::new::T", "t::new::S::new", "t::new::make"] {
        let it = &out.items[id];
        assert_eq!(
            it.changes.moved_from.as_deref(),
            Some(id.replacen("::new::", "::old::", 1).as_str()),
            "{id}"
        );
        // Only moved: references to items that moved along are no change.
        let c = &it.changes;
        assert!(!c.signature && !c.body && !c.docs, "{id}: {c:?}");
        assert!(it.sig_diff.is_none(), "{id}");
    }
    assert!(
        !out.items
            .values()
            .any(|i| i.status == Status::Removed && i.info.kind != Kind::Mod)
    );
}

#[test]
fn changes_outside_items_are_counted() {
    let base = write_crate(&[(
        "src/lib.rs",
        "use std::cell::Cell;\n\nthread_local! { static N: Cell<u8> = Cell::new(0); }\n\nextern \"C\" {\n    fn abs(x: i32) -> i32;\n}\n\npub fn f() -> u8 {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { assert_eq!(super::f(), 1); }\n}\n",
    )]);
    let head = write_crate(&[(
        "src/lib.rs",
        "use std::cell::Cell;\n\nthread_local! { static N: Cell<u16> = Cell::new(0); }\n\nextern \"C\" {\n    fn abs(x: i64) -> i64;\n}\n\npub fn f() -> u8 {\n    2\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { assert_eq!(super::f(), 2); }\n}\n",
    )]);
    let checkout =
        crate::source::Checkout::from_dirs(base.path(), head.path(), Trust::Untrusted).unwrap();
    let out = crate::analyze_checkout(&checkout, false, Trust::Untrusted);
    let lib = out.files.iter().find(|f| f.path == "src/lib.rs").unwrap();
    assert!(lib.analyzed);
    // `thread_local!`, the extern fn and the test assertion: 3 lines on
    // each side. The change in `f` is inside an item.
    assert_eq!(lib.outside_items, 6);
    // The demo's changes are all inside items or imports.
    let demo =
        crate::source::Checkout::from_dirs(&fixture("base"), &fixture("head"), Trust::Untrusted)
            .unwrap();
    let out = crate::analyze_checkout(&demo, false, Trust::Untrusted);
    for f in out.files.iter().filter(|f| f.analyzed) {
        assert_eq!(f.outside_items, 0, "{}: {:#?}", f.path, f.hunks);
    }
}

#[test]
fn mutual_glob_imports_stay_fast() {
    let n = 10;
    let mut files: Vec<(String, String)> = vec![(
        "src/lib.rs".into(),
        (0..n)
            .map(|i| format!("mod m{i};\npub use m{i}::*;\n"))
            .collect(),
    )];
    for i in 0..n {
        files.push((
            format!("src/m{i}.rs"),
            format!("use super::*;\npub fn a{i}() {{ b{i}(); x(); }}\npub fn b{i}() {{ Vec::<u8>::new(); }}\n"),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let dir = write_crate(&refs);
    let start = std::time::Instant::now();
    let out = analyze_dirs(dir.path(), dir.path());
    assert!(start.elapsed().as_secs() < 5, "{:?}", start.elapsed());
    let a = &out.items["t::m3::a3"].info;
    assert!(a.calls.iter().any(|c| c.to == "t::m3::b3"));
}

/// Analyzes one tree with rust-analyzer's public API.
#[cfg(feature = "semantic")]
fn analyze_semantic(dir: &Path) -> crate::analyze::Analysis {
    let mut analysis = Analyzer::run(dir, &crate::source::find_crates(dir));
    crate::semantic::refine(dir, &[dir.to_path_buf()], Trust::Untrusted, &mut analysis).unwrap();
    analysis
}

/// rust-analyzer's resolved scopes decide the public API, with
/// `#[doc(hidden)]` and `#[doc(inline)]` taken into account like rustdoc.
#[cfg(feature = "semantic")]
#[test]
fn exact_public_api() {
    let dir = write_crate(&[(
        "src/lib.rs",
        "pub mod a { pub struct InA; pub(crate) struct Crate; }\n\
         mod private { pub struct Thing; pub struct Twice; pub struct Gen; }\n\
         macro_rules! reexport { () => { pub use crate::private::Gen; } }\nreexport!();\n\
         #[doc(hidden)]\npub mod hidden { pub struct Inside; pub struct Shown; }\n\
         pub use private::Thing;\n\
         pub use a::InA;\n\
         #[doc(inline)]\npub use a::InA as Inlined;\n\
         pub use hidden::Shown;\n\
         #[doc(hidden)]\npub use private::Twice as HiddenTwice;\n\
         pub use private::Twice;\n\
         #[doc(hidden)]\npub fn secret() {}\n\
         pub struct S;\n\
         impl S { pub fn m(&self) {} #[doc(hidden)] pub fn h(&self) {} }\n\
         #[macro_export]\nmacro_rules! mac { () => {} }\n\
         pub use std::collections::HashMap;\n\
         pub use std::fmt::Display as Show;\n",
    )]);
    let an = analyze_semantic(dir.path());
    let paths = |id: &str| an.items[id].info.public_paths.clone();
    let public = |id: &str| an.items[id].info.public;
    // Definition path first, then `#[doc(inline)]`, then other re-exports.
    assert_eq!(paths("t::a::InA"), ["t::a::InA", "t::Inlined", "t::InA"]);
    assert!(!public("t::a::Crate"));
    assert_eq!(paths("t::private::Thing"), ["t::Thing"]);
    // Re-exports generated by macros count too.
    assert_eq!(paths("t::private::Gen"), ["t::Gen"]);
    // A hidden module hides the paths through it, not the re-exports.
    assert!(!public("t::hidden"));
    assert!(!public("t::hidden::Inside"));
    assert_eq!(paths("t::hidden::Shown"), ["t::Shown"]);
    // A hidden `pub use` hides only its own name.
    assert_eq!(paths("t::private::Twice"), ["t::Twice"]);
    assert!(!public("t::secret"));
    assert!(public("t::S::m"));
    assert!(!public("t::S::h"));
    assert_eq!(paths("t::mac"), ["t::mac"]);
    // Other crates' items that the crate re-exports are items of their own.
    let map = &an.items["t::<re-export std::collections::hash::map::HashMap>"].info;
    assert_eq!(map.kind, Kind::Struct);
    assert_eq!(map.public_paths, ["t::HashMap"]);
    assert_eq!(map.module, "t");
    assert_eq!(map.sig, "pub use std::collections::hash::map::HashMap;");
    assert!(map.docs.contains("hash map"), "{}", map.docs);
    let show = &an.items["t::<re-export core::fmt::Display>"].info;
    assert_eq!(show.kind, Kind::Trait);
    assert_eq!(show.sig, "pub use core::fmt::Display as Show;");
    assert_eq!(show.name, "Show");
}

#[test]
fn doc_hidden_is_not_public_api() {
    let dir = write_crate(&[(
        "src/lib.rs",
        "#[doc(hidden)]\npub mod hidden { pub struct Inside; pub struct Shown; }\n\
         mod private { pub struct Twice; }\n\
         pub use hidden::Shown;\n\
         #[doc(hidden)]\npub use private::Twice as HiddenTwice;\n\
         pub use private::Twice;\n\
         #[doc(hidden)]\npub fn secret() {}\n\
         pub struct S;\n\
         impl S { pub fn m(&self) {} #[doc(hidden)] pub fn h(&self) {} }\n",
    )]);
    let out = analyze_dirs(dir.path(), dir.path());
    let info = |id: &str| &out.items[id].info;
    assert!(!info("t::hidden").public);
    assert!(!info("t::hidden::Inside").public);
    assert_eq!(info("t::hidden::Shown").public_paths, ["t::Shown"]);
    assert_eq!(info("t::private::Twice").public_paths, ["t::Twice"]);
    assert!(!info("t::secret").public);
    assert!(info("t::S::m").public);
    assert!(!info("t::S::h").public);
}
