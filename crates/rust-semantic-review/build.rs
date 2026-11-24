//! Compiles the viewer crate to WebAssembly and generates its JavaScript
//! bindings, so that `main.rs` can embed both in the review page.
//!
//! Requires the `wasm32-unknown-unknown` target, which rustup installs from
//! the workspace's `rust-toolchain.toml`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest_dir.parent().unwrap().parent().unwrap();
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for dep in ["crates/viewer", "crates/model", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", workspace.join(dep).display());
    }

    // For analyzing this repository with the tool itself in trusted
    // mode, which runs this build script once per version: the viewer's
    // wasm build is most of that time, and the analysis doesn't need it.
    // A tool built this way shows an empty page.
    // `RUST_PR_EXPLORER_STUB_VIEWER` is the name from before the rename.
    println!("cargo:rerun-if-env-changed=RUST_SEMANTIC_REVIEW_STUB_VIEWER");
    println!("cargo:rerun-if-env-changed=RUST_PR_EXPLORER_STUB_VIEWER");
    let stub = [
        "RUST_SEMANTIC_REVIEW_STUB_VIEWER",
        "RUST_PR_EXPLORER_STUB_VIEWER",
    ]
    .iter()
    .find_map(std::env::var_os);
    if stub.is_some_and(|v| v == "1") {
        std::fs::write(out_dir.join("viewer.js"), "").unwrap();
        std::fs::write(out_dir.join("viewer_bg.wasm.gz"), "").unwrap();
        return;
    }

    let target_dir = out_dir.join("viewer-target");
    build_viewer(workspace, &target_dir);
    let wasm = target_dir.join("wasm32-unknown-unknown/wasm-release/viewer.wasm");
    wasm_bindgen_cli_support::Bindgen::new()
        .input_path(&wasm)
        .web(true)
        .expect("wasm-bindgen: web target")
        .typescript(false)
        // Function names are only needed for debugging and were more than a
        // third of the module.
        .remove_name_section(true)
        .remove_producers_section(true)
        .generate(&out_dir)
        .expect("wasm-bindgen failed");
    #[cfg(feature = "wasm-opt")]
    {
        let wasm = out_dir.join("viewer_bg.wasm");
        wasm_opt::OptimizationOptions::new_optimize_for_size()
            .run(&wasm, &wasm)
            .expect("wasm-opt failed");
    }
    gzip(
        &out_dir.join("viewer_bg.wasm"),
        &out_dir.join("viewer_bg.wasm.gz"),
    );
}

/// The page embeds the wasm gzipped, which makes it about a third of its
/// size, and the browser unpacks it with a `DecompressionStream`: the
/// page is often served without compression, e.g. from a workflow
/// artifact or as a local file.
fn gzip(from: &Path, to: &Path) {
    use std::io::Write;
    let wasm = std::fs::read(from).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&wasm).unwrap();
    std::fs::write(to, gz.finish().unwrap()).unwrap();
}

fn build_viewer(workspace: &Path, target_dir: &Path) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(workspace).args([
        "build",
        "--package",
        "viewer",
        "--lib",
        "--profile",
        "wasm-release",
        "--target",
        "wasm32-unknown-unknown",
        "--target-dir",
    ]);
    cmd.arg(target_dir);
    // Settings of the outer build that must not leak into the wasm build.
    for var in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_TARGET_DIR",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTC_WRAPPER",
    ] {
        cmd.env_remove(var);
    }
    let status = cmd.status().expect("failed to run cargo for the viewer");
    if !status.success() {
        panic!(
            "building the viewer failed; is the wasm target installed? \
             Run `rustup target add wasm32-unknown-unknown`."
        );
    }
}
