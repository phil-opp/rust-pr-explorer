//! Compiles the viewer crate to WebAssembly and generates its JavaScript
//! bindings, so that `main.rs` can embed both in the explorer page.
//!
//! Requires the `wasm32-unknown-unknown` target:
//! `rustup target add wasm32-unknown-unknown`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest_dir.parent().unwrap().parent().unwrap();
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for dep in ["crates/viewer", "crates/model", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", workspace.join(dep).display());
    }

    let target_dir = out_dir.join("viewer-target");
    build_viewer(workspace, &target_dir);
    let wasm = target_dir.join("wasm32-unknown-unknown/release/viewer.wasm");
    wasm_bindgen_cli_support::Bindgen::new()
        .input_path(&wasm)
        .web(true)
        .expect("wasm-bindgen: web target")
        .typescript(false)
        .generate(&out_dir)
        .expect("wasm-bindgen failed");
}

fn build_viewer(workspace: &Path, target_dir: &Path) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(workspace).args([
        "build",
        "--package",
        "viewer",
        "--lib",
        "--release",
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
