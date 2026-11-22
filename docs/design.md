# Design notes

This document records how the Rust PR Explorer prototype gets its information and why. It also covers the alternatives that were considered but not implemented. For what the tool shows, see the [README](../README.md).

## Requirements that shape the design

- **Everything in Rust.** The analysis and the viewer are Rust crates. The only JavaScript in the generated page is the glue that `wasm-bindgen` generates, plus three handwritten lines that load the WebAssembly module. A browser offers no other way to start WebAssembly.
- **No `unsafe`.** `unsafe_code = "forbid"` is set for the whole workspace. Dependencies such as `wasm-bindgen` and rust-analyzer use `unsafe` internally; our code does not.
- **Never run the analyzed project's code.** A pull request is untrusted input. Build scripts (`build.rs`) and procedural macros are arbitrary programs, so the tool must not compile or run them. See [Safety](#safety).
- **One self-contained output file.** The explorer is a single HTML file that can be opened locally or shared.

## Architecture

```text
crates/model             data types shared by analysis and viewer (serde)
crates/rust-pr-explorer  CLI: get sources, analyze, compare, write the page
  src/source.rs          git clone/archive or directory copy; finds crates
  src/analyze.rs         syntactic analysis with syn (items, names, edges)
  src/semantic.rs        call resolution with rust-analyzer (feature "semantic")
  src/merge.rs           base/head comparison
  build.rs               compiles crates/viewer to wasm, runs wasm-bindgen
crates/viewer            the page's front end (wasm, web-sys, pulldown-cmark)
```

The CLI's build script runs a nested `cargo build` of the viewer for `wasm32-unknown-unknown`, in its own target directory. It then generates the JavaScript bindings with `wasm-bindgen-cli-support`, used as a library. `main.rs` embeds both outputs with `include_bytes!`. A plain `cargo build` therefore produces a complete tool; the only extra requirement is the wasm target (`rustup target add wasm32-unknown-unknown`).

## Two analysis layers

### 1. Syntactic analysis (always on)

[`syn`](https://docs.rs/syn) parses every module of every crate in both checkouts. The analyzer builds the module tree, collects definitions and imports, and resolves paths:
- `crate`, `self`, `super` and `Self`
- imports, renames, glob imports and re-exports
- the prelude

From that it renders rustdoc-style signatures with `prettyplease`, and records calls, uses and intra-doc links. Changes are detected on normalized text plus the fully qualified target of every name, so formatting and import style don't count.

This layer needs nothing but the source text. It works on any checkout, broken or not, and takes milliseconds. Its gap is types: in `parser.parse()?.next()` it cannot know which `next` is meant. Method calls are therefore resolved only when the receiver is `self` or a typed parameter. Otherwise the target is guessed from a unique method name and marked as *inferred*.

### 2. rust-analyzer as a library (on by default)

The `semantic` feature links rust-analyzer's crates (`ra_ap_ide`, `ra_ap_load-cargo`, …). For every function found by the syntactic pass, the tool asks rust-analyzer for the function's *outgoing calls*. rust-analyzer resolves each call site with full type inference. Its answers replace the guessed calls of that function, matched by the position of each call site. Results are mapped back to item ids through the position of the target's name.

Configuration:

| Setting | Value | Why |
|---|---|---|
| build scripts (`load_out_dirs_from_check`) | off | never run project code |
| proc-macro server | none | never run project code |
| features | all | the syntactic pass ignores `cfg` too; with default features, code behind `cfg(feature = "std")` was invisible |
| dependencies (`no_deps`) | loaded | with `no_deps`, the standard library was not loaded either, and inference failed even on `?` |
| sysroot | discovered | type information for `core`/`alloc`/`std` (needs the `rust-src` component) |

Measured on `rust-lang/log#613`:
- With default features, 70 of 205 calls stayed guesses.
- With all features enabled, none did.
- Both sides took about 13 seconds in total, including the clone.

On the demo fixture, rust-analyzer resolves calls such as `Parser::new(input)?.parse_expr()` and `op.precedence()`, where `op` comes from a pattern.

If loading fails (no `Cargo.toml`, `cargo metadata` errors, missing network for dependencies), the tool falls back to the syntactic results, notes this in the page, and continues. `--syntax-only` skips this layer.

Costs and risks:

- **Build time.** The rust-analyzer crates add about 3 minutes to a clean release build. Building with `--no-default-features` removes them.
- **Version churn.** The `ra_ap_*` crates are published weekly with breaking changes, so they are pinned to an exact version. They also broke with a newer semver-compatible `salsa` release; `salsa` is pinned as a direct dependency for that reason.
- **Limits.** rust-analyzer reports no calls made inside `format_args!`-style macro arguments (`write!(f, "{}", x.foo())`). Those keep the syntactic guess, still marked as inferred.

## Safety

The analyzed code is never compiled or executed. Two tools do run on the checkout: `cargo metadata`, which rust-analyzer uses to load the workspace, and `rustc --print …` for the sysroot. Both read configuration from the source tree, which a hostile PR could use to run its own programs:
- `.cargo/config.toml` can set `build.rustc` or `build.rustc-wrapper`.
- `rust-toolchain.toml` can point at a custom toolchain directory.

The tool therefore analyzes only temporary copies, and before analysis it deletes from them:
- every `.cargo/config(.toml)`
- every `rust-toolchain(.toml)`
- every symlink, so that no file outside the tree is read and embedded in the page

`cargo metadata` may download dependency sources from the registry, but it does not build them.

In the page, doc comments are rendered with `pulldown-cmark`, with all raw HTML turned into text. Links only point to analyzed items or to `http(s)` URLs. The embedded JSON has `</` escaped. The analyzed code cannot inject markup or script into the page.

## Alternatives considered

### rustdoc JSON (`--output-format json`) — not implemented

Requiring both sides to build with rustdoc would give the compiler's own view of every item:
- exact paths and visibility
- `pub use` re-exports and `#[doc(inline)]`
- `#[doc(hidden)]`
- derive output, and blanket and auto-trait impls
- macro-generated items
- fully resolved types in signatures, including links into dependencies

This is how tools like `cargo-public-api` compute API diffs, and it would make the overview's "public API" exact.

Not done, because:

- **It runs the project's code.** Building documentation compiles dependencies and runs build scripts and proc macros. For PRs from strangers this needs a sandbox (container or VM without network or credentials). The current design avoids that requirement entirely.
- **Nightly only, unstable format.** The JSON format changes often (`FORMAT_VERSION`). Parsing it needs a `rustdoc-types` version that matches the exact nightly.
- **No bodies.** rustdoc describes interfaces only, so call graphs and uses in bodies would still need a second source (syn or rust-analyzer).
- **One `cfg` configuration per build.**
- **Slow.** Both sides must build with their dependencies.

rust-analyzer already provides most of the semantic precision the views need, without executing anything. A later version could add rustdoc JSON as an opt-in mode (`--rustdoc`, ideally inside a sandbox) to make the public-API overview and signatures exact. The syntactic item model would stay for bodies and for code that doesn't build.

### rustdoc HTML — not implemented

Embedding or scraping rustdoc's HTML pages would reuse rustdoc's rendering. But the HTML is not a stable interface. It comes with rustdoc's own JavaScript and CSS, which are large, and it needs the same build as rustdoc JSON. The explorer already renders signatures in rustdoc style. A cheaper use of rustdoc HTML would be linking names in external crates to their pages on docs.rs: the fully qualified path is already known for every name.

### rust-analyzer over LSP — not implemented

Running the `rust-analyzer` binary and talking LSP (`callHierarchy/outgoingCalls`) would avoid the heavy compile-time dependency and the version pinning. Against it:
- It needs the binary installed.
- It needs an LSP client, with asynchronous indexing and progress handling.
- By default the binary *does* run build scripts and proc macros, so safety depends on passing the right settings (`cargo.buildScripts.enable = false`, `procMacro.enable = false`).

The library gives direct, synchronous access and makes the safe configuration explicit in code.

### rust-analyzer with build scripts and proc macros — not implemented

Running them would let rust-analyzer see:
- code included from `OUT_DIR` (generated bindings)
- items produced by proc-macro attributes and derives

That is a real gap for crates that rely on them. It means executing project code, so it should only exist as an explicit opt-in such as `--trust-build-scripts`, or inside a sandbox.

### rust-analyzer as the only item model — not implemented

rust-analyzer's `hir` API could replace the syntactic item model entirely: modules, items, visibility and signatures, with types resolved. That would remove the hand-written name resolution, but:
- It couples the whole tool to a weekly-changing API.
- It doesn't work for code that fails to load as a cargo workspace.
- The syntactic model is fast and already accurate for paths.

The current split uses rust-analyzer where only type inference helps: method calls. Moving the public-API computation (re-exports, visibility) to `hir` would be the natural next step.

### Front end: framework or native GUI — not chosen

- **Leptos, Yew or Dioxus** would make the views declarative. They add compile time and API churn, and the views are re-rendered wholesale anyway: they are small, and the canvas positions cards absolutely.
- **A native GUI (egui)** removes the browser and the generated JavaScript glue. It would lose text selection, rustdoc-like typography, and the ability to share one HTML file. It would also need a display to test, unlike the headless browser used now.

The chosen design is plain `web-sys`: views render HTML strings, and one listener per event type dispatches on `data-*` attributes. It keeps the dependency set small (`wasm-bindgen`, `web-sys`, `js-sys`, `serde_json`, `pulldown-cmark`) and produces about 1.2 MB of wasm.

### Git access with `gix` — not implemented

The tool calls the `git` binary to clone, fetch pull request refs, compute merge bases, and create worktrees or archives. [`gix`](https://docs.rs/gix) could do this in-process, in Rust. It was not worth the extra dependency for a prototype; `git` is available wherever pull requests are reviewed.

## Next steps

- Opt-in rustdoc JSON or proc-macro expansion, inside a sandbox.
- Public-API computation via rust-analyzer's `hir` (exact re-exports, `doc(hidden)`).
- docs.rs links for external names.
- Fetch PR metadata (title, description) through the GitHub API when a token is available.
- Smaller wasm, for example by replacing `serde_json` with a more compact format, or by running `wasm-opt`.
