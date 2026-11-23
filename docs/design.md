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
  src/source.rs          git worktrees or directory copies; finds crates and targets
  src/analyze/           syntactic analysis with syn (items, names, edges)
    load.rs              module files, items and impls (passes 1 and 2)
    resolve.rs           name resolution: imports, globs, prelude, visibility
    items.rs             signatures, docs and edges of each item (pass 3)
    public.rs            re-exports, reachability, public paths
    collector.rs         resolving the paths inside an item
    syntax.rs            rendering and normalizing syntax
  src/semantic.rs        call resolution with rust-analyzer (feature "semantic")
  src/public_api.rs      exact public API from rust-analyzer's name resolution
  src/merge.rs           base/head comparison, moves
  src/semver.rs          breaking / non-breaking labels for API changes
  src/sigdiff.rs         structured signature diffs
  src/files.rs           every changed file: line diffs, Cargo.toml entries
  src/lockfile.rs        lock files (Cargo.lock, uv.lock, poetry.lock) by package
  src/parts.rs           files split into parts compared one by one: YAML keys,
                         Rust code outside items (tests, macro calls, extern)
  src/python.rs          Python files compared by function and class
  src/javascript.rs      JavaScript files compared by function, class, method, test
  src/css.rs             CSS files compared by selector, rules merged by the cascade
  build.rs               compiles crates/viewer to wasm, runs wasm-bindgen
crates/viewer            the page's front end (wasm, web-sys, pulldown-cmark)
  src/pages.rs           overview and module pages
  src/code/              highlighting, signature diffs, Markdown, doc diffs
  src/canvas/            canvas of cards: cards, edges, layout, events
  src/map.rs, route.rs   structure view (module map, matrix) and arrow routing
  src/files.rs           other changed files (Markdown sections, Python, YAML,
                         manifests, Rust code outside items)
  src/css.rs             CSS highlighting and selector boxes
```

The CLI's build script runs a nested `cargo build` of the viewer for `wasm32-unknown-unknown`, in its own target directory. It then generates the JavaScript bindings with `wasm-bindgen-cli-support`, used as a library. `main.rs` embeds both outputs with `include_bytes!`. A plain `cargo build` therefore produces a complete tool; the only extra requirement is the wasm target (`rustup target add wasm32-unknown-unknown`).

### Size of the wasm module

The module is embedded in every page as base64, so its size is paid by every page. The viewer is built with the `wasm-release` profile (`opt-level = "s"`, one codegen unit, link-time optimization), and `wasm-bindgen` drops the name and producers sections. Measured on the viewer at the time of the change:

| Step | Module | Page (demo) |
|---|---|---|
| before: `release` profile, names kept | 1.99 MB | about 2.86 MB |
| name section removed | 1.28 MB | |
| plus LTO (`wasm-release`) | 1.22 MB | 1.83 MB |
| plus `wasm-opt -Os` (feature `wasm-opt`) | 0.96 MB | 1.49 MB |

The function names were 37% of the module; they only help when debugging a panic in the browser. `wasm-opt` is opt-in because the `wasm-opt` crate compiles Binaryen (C++) from source, which takes about 5 minutes and needs a C++ compiler; `opt-level = "z"` saved only 1% more than `"s"`, so the viewer stays at `"s"`.

Replacing `serde_json` with a more compact format was considered and not done. The JSON deserializer and the derived visitors are roughly 130 kB of the module before `wasm-opt` (by `twiggy`), and a binary format such as `postcard` would still need the derived visitors. The model also relies on `skip_serializing_if`, `flatten` and internally tagged enums, which only work with self-describing formats like JSON; and binary data would have to be embedded as base64. The `similar` crate is larger (about 240 kB before `wasm-opt`, mostly copies of its diff algorithms for each element type), so it would be the next place to look.

## Two analysis layers

### 1. Syntactic analysis (always on)

[`syn`](https://docs.rs/syn) parses every module of every crate in both checkouts. The analyzer builds the module tree, collects definitions and imports, and resolves paths:
- `crate`, `self`, `super` and `Self`
- imports, renames, glob imports and re-exports
- the prelude

From that it renders rustdoc-style signatures with `prettyplease`, and records calls, uses and intra-doc links. Changes are detected on normalized text plus the fully qualified target of every name, so formatting and import style don't count.

This layer needs nothing but the source text. It works on any checkout, broken or not, and takes milliseconds. Its gap is types: in `parser.parse()?.next()` it cannot know which `next` is meant, and it cannot know what a macro does with its arguments. It records those call sites, with their positions, and leaves them unresolved.

Since this is a review tool, it never guesses. Name resolution follows Rust's rules where it can be sure, and stays silent otherwise:
- **Name spaces.** `helper()` names the function, even if a module `helper` exists too.
- **Shadowing.** Names bound by a pattern, generic parameters, and items or imports declared inside a body are never resolved to module-level items of the same name.
- **Glob imports** bring in only what they make visible. A glob import from another crate (or an unresolvable path) could provide any name, so in that module prelude names are not claimed.
- **External paths** are claimed only for declared dependencies and `std`/`core`/`alloc`, using the prelude of the crate's edition.
- **Ambiguity.** A name that resolves to different items in two places of one item is not linked at all.

### Public paths

After the items are built, every item gets the paths under which code outside the crate can name it. Crate roots start with their own name. Then, until nothing changes: a `pub` item of a module with a public path gets that path plus its name, and a `pub use` in such a module gives its target the module's path plus the imported (or renamed) name; glob re-exports do this for every `pub` item of the module. Paths are capped at 12 segments, which only re-export cycles reach. An item is public if it has a path. `#[doc(hidden)]` items and `pub use`s give no paths. Base and head paths are compared: a path gained or lost is a change of the item, even if its code is the same. The first path is where rustdoc documents the item (its definition path if public, else the shortest re-export), and the overview lists it there.

With rust-analyzer, `public_api.rs` replaces these paths: it walks rust-analyzer's resolved module scopes (`hir_def`'s `ItemScope`) from each library root through every `pub` name, which also covers globs, `cfg` and macro-generated `pub use`s. Paths through a `#[doc(hidden)]` item, module or `pub use` are dropped, and re-exports are ordered by `#[doc(inline)]`/`#[doc(no_inline)]` like rustdoc. Definitions are mapped back to items by the position of their name (modules by path). Names that refer to other crates' items become items of their own (`reexport_of`), and items rust-analyzer doesn't see (an inactive `cfg`) keep the syntactic paths.

### 2. rust-analyzer as a library (on by default)

The `semantic` feature links rust-analyzer's crates (`ra_ap_ide`, `ra_ap_load-cargo`, …). For every function found by the syntactic pass, the tool asks rust-analyzer for the function's *outgoing calls*, which it resolves with full type inference. Then every call site the syntactic pass recorded is checked against those answers by position. Sites the call hierarchy doesn't list, notably calls inside macro arguments, are checked with *go to definition* at the site. What still can't be resolved is counted per function and shown on its card. Results are mapped back to item ids through the position of the target's name. Method calls also keep their target by position, so the source view links each call site exactly.

Configuration:

| Setting | Value | Why |
|---|---|---|
| build scripts (`load_out_dirs_from_check`) | off | never run project code |
| proc-macro server | none | never run project code |
| features | all | the syntactic pass ignores `cfg` too; with default features, code behind `cfg(feature = "std")` was invisible |
| dependencies (`no_deps`) | loaded | with `no_deps`, the standard library was not loaded either, and inference failed even on `?` |
| sysroot | discovered | type information for `core`/`alloc`/`std` (needs the `rust-src` component) |

Measured on `rust-lang/log#613`:
- With default features, 70 of 205 calls could not be resolved.
- With all features enabled, none did.
- Both sides took about 13 seconds in total, including the clone.

On the demo fixture, rust-analyzer resolves calls such as `Parser::new(input)?.parse_expr()` and `op.precedence()`, where `op` comes from a pattern.

The same session also describes items outside the analyzed crates. For each path into the standard library or a dependency, the tool takes one place in the code that names it and asks rust-analyzer for *hover* (declaration and docs, with intra-doc links turned into URLs), *go to definition* (the kind of item) and *external docs* (the docs.rs or doc.rust-lang.org URL). The description is kept only if the definition has the same name and crate as the path the syntactic pass resolved (`std`, `core` and `alloc` count as one crate). This is the "on demand" form of what rustdoc does for the whole dependency tree: only what the analyzed code names is looked up. On `rust-lang/log#613` it describes 74 items on the base side and 59 on the head side; the whole run, including the clone, took about 14 seconds (about 13 before this step existed).

If loading fails (no `Cargo.toml`, `cargo metadata` errors, missing network for dependencies), the tool falls back to the syntactic results, notes this in the page, and continues. `--syntax-only` skips this layer.

Costs and risks:

- **Build time.** The rust-analyzer crates add about 3 minutes to a clean release build. Building with `--no-default-features` removes them.
- **Version churn.** The `ra_ap_*` crates are published weekly with breaking changes, so they are pinned to an exact version. They also broke with a newer semver-compatible `salsa` release; `salsa` is pinned as a direct dependency for that reason.
- **Limits.** rust-analyzer's call hierarchy doesn't list calls inside `format_args!`-style macro arguments (`write!(f, "{}", x.foo())`); *go to definition* resolves them. Code that is inactive with all features enabled (e.g. behind `cfg(not(feature = "x"))`) can't be typed, and its call sites stay unresolved.

## Safety

By default the analyzed code is never compiled or executed. Two tools do run on the checkout: `cargo metadata`, which rust-analyzer uses to load the workspace, and `rustc --print …` for the sysroot. Both read configuration from the source tree, which a hostile PR could use to run its own programs:
- `.cargo/config.toml` can set `build.rustc` or `build.rustc-wrapper`.
- `rust-toolchain.toml` can point at a custom toolchain directory.

The tool therefore analyzes only temporary copies, and before analysis it deletes from them:
- every `.cargo/config(.toml)`
- every `rust-toolchain(.toml)`
- every symlink, so that no file outside the tree is read and embedded in the page

`cargo metadata` may download dependency sources from the registry, but it does not build them.

The list of changed files is taken before these deletions, so they are still shown. In `git` mode the revisions are checked out as worktrees of a temporary `--shared` clone, not with `git archive`, which would apply the commit's own `export-ignore` attributes and let a PR hide files.

Paths the project chooses are confined to the checkout: `#[path]` attributes and the target paths in `Cargo.toml` (`[lib] path`, `[[bin]] path`, `package.build`) are canonicalized, and files outside the checkout are not read (a warning on the page says so); a file is not loaded again inside a module that was loaded from it, so `#[path]` cycles end (one file included as two unrelated modules is fine, as with rustc, up to 1000 such repeats per crate, so that files including the next one twice cannot double the work with every file). Items outside the analyzed crates are described only if rust-analyzer found them in the checkout, the toolchain's sources or Cargo's registry and git directories, so a path dependency pointing elsewhere on the machine is not described either.

### Trusted mode

In a sandboxed environment (a throwaway container or VM without credentials), running the project's code is acceptable. `--trust-project-code`, or `RUST_PR_EXPLORER_TRUST_PROJECT_CODE=1` for CI and container setups, then:
- runs build scripts (rust-analyzer's `cargo check` step), so code included from `OUT_DIR` is analyzed;
- expands procedural macros with the toolchain's `rust-analyzer-proc-macro-srv`;
- keeps `.cargo/config.toml` and `rust-toolchain.toml`, which builds may need.

Symlinks are still removed, because removing them protects the page's contents, not the machine. Example: with a type defined only in build-script output, `make().hello()` stays unresolved in the default mode but resolves to the right method in trusted mode. The page states which mode was used.

In the page, doc comments are rendered with `pulldown-cmark`, with all raw HTML turned into text. Links only point to analyzed items or to `http(s)` URLs, including documentation links of dependencies, which come from their `html_root_url`. The embedded JSON has every `<` escaped, so nothing in it can end or change its `<script>` element. The analyzed code cannot inject markup or script into the page.

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

Embedding or scraping rustdoc's HTML pages would reuse rustdoc's rendering. But the HTML is not a stable interface. It comes with rustdoc's own JavaScript and CSS, which are large, and it needs the same build as rustdoc JSON. The explorer already renders signatures in rustdoc style. Names in external crates link to their pages on docs.rs through rust-analyzer's *external docs* instead.

### rust-analyzer over LSP — not implemented

Running the `rust-analyzer` binary and talking LSP (`callHierarchy/outgoingCalls`) would avoid the heavy compile-time dependency and the version pinning. Against it:
- It needs the binary installed.
- It needs an LSP client, with asynchronous indexing and progress handling.
- By default the binary *does* run build scripts and proc macros, so safety depends on passing the right settings (`cargo.buildScripts.enable = false`, `procMacro.enable = false`).

The library gives direct, synchronous access and makes the safe configuration explicit in code.

### rust-analyzer with build scripts and proc macros — implemented as an opt-in

Running them lets rust-analyzer see code included from `OUT_DIR` and items produced by proc macros. Because it executes project code, it is off by default and enabled with `--trust-project-code`; see [Trusted mode](#trusted-mode). The syntactic item model still doesn't include generated items, so they show up as call targets only if they are defined in the analyzed source.

### rust-analyzer as the only item model — not implemented

rust-analyzer's `hir` API could replace the syntactic item model entirely: modules, items, visibility and signatures, with types resolved. That would remove the hand-written name resolution, but:
- It couples the whole tool to a weekly-changing API.
- It doesn't work for code that fails to load as a cargo workspace.
- The syntactic model is fast and already accurate for paths.

The current split uses rust-analyzer where the syntax isn't enough: method calls, and the public API (re-exports, `#[doc(hidden)]`, macro-generated `pub use`s).

### Front end: framework or native GUI — not chosen

- **Leptos, Yew or Dioxus** would make the views declarative. They add compile time and API churn, and the views are re-rendered wholesale anyway: they are small, and the canvas positions cards absolutely.
- **A native GUI (egui)** removes the browser and the generated JavaScript glue. It would lose text selection, rustdoc-like typography, and the ability to share one HTML file. It would also need a display to test, unlike the headless browser used now.

The chosen design is plain `web-sys`: views render HTML strings, and one listener per event type dispatches on `data-*` attributes. It keeps the dependency set small (`wasm-bindgen`, `web-sys`, `js-sys`, `serde_json`, `pulldown-cmark`, `similar`) and produces about 1.2 MB of wasm (1 MB with `wasm-opt`; see [Size of the wasm module](#size-of-the-wasm-module)).

### Git access with `gix` — not implemented

The tool calls the `git` binary to clone, fetch pull request refs, compute merge bases, and create worktrees or archives. [`gix`](https://docs.rs/gix) could do this in-process, in Rust. It was not worth the extra dependency for a prototype; `git` is available wherever pull requests are reviewed.

## Next steps

- Opt-in rustdoc JSON in trusted mode, for an exact public API.
