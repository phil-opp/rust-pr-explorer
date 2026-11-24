# How it works

The analysis, its safety model and its limits. For the design and the alternatives that were considered, see [design.md](design.md).

## Analysis

The workspace has three crates:
- `crates/model`: the data passed from the analysis to the page.
- `crates/rust-semantic-review`: the command-line tool.
- `crates/viewer`: the page's front end.

1. **Sources** (`source.rs`): getting the two source trees.
   - PRs are cloned with `git` and the two revisions checked out.
   - Local revisions are exported with `git archive`; directories are copied.
   - In every case the analysis works on temporary copies, with tool configuration and symlinks removed (see Safety).
2. **Syntactic analysis** (`analyze/`, [`syn`](https://docs.rs/syn)): finding items and resolving names.
   - Builds the module tree, following `mod` declarations, `mod.rs`/`name.rs` and `#[path]`.
   - Gives every item, including methods, a stable id independent of file and `impl` block, e.g. `crate::ast::Expr::<impl Display>::fmt`.
   - Resolves every path in signatures and bodies: `crate`/`self`/`super`/`Self`, imports, renames, globs, re-exports and the prelude.
   - Renders signatures in rustdoc style with [`prettyplease`](https://docs.rs/prettyplease), and records calls, uses and intra-doc links.
3. **Call resolution** (`semantic.rs`): rust-analyzer, linked as a library, is asked for each function's outgoing calls.
   - It loads the workspace with all features enabled, without running build scripts and without expanding procedural macros.
   - Its type-checked answers decide the calls. Every call site the syntactic pass found is checked, including method calls and calls inside macros (with "go to definition" where the call hierarchy doesn't list them). Sites that can't be resolved are counted on the card, not guessed.
   - If loading fails, the syntactic results are used and the page says so.
4. **Public API** (`public_api.rs`): rust-analyzer's resolved module scopes, walked from each library's root through every name visible outside the crate, give each item's public paths. This covers glob imports, `cfg` and re-exports that macros generate, and it applies `#[doc(hidden)]`, `#[doc(inline)]` and `#[doc(no_inline)]` like rustdoc. Names that refer to other crates' items become re-export items. Without rust-analyzer, the syntactic pass computes the public paths, including `#[doc(hidden)]`.
5. **Comparison** (`merge.rs`): matching items by id across base and head.
   - Changes are detected on normalized forms: pretty-printed tokens plus the fully qualified target of every name. Reformatting and changes in import style don't count.
   - A `use` that now points at a different item does count.
6. **Page**:
   - The CLI's build script compiles `crates/viewer` to WebAssembly and generates its bindings with `wasm-bindgen`.
   - The output page embeds the wasm module, the bindings and the analysis as JSON.
   - The viewer renders the views with `web-sys`, and docs with [`pulldown-cmark`](https://docs.rs/pulldown-cmark).

`unsafe_code` is forbidden in all three crates.

## Safety

A pull request is untrusted code. By default the tool never compiles or runs it: no `cargo build`, no build scripts, no procedural macros. rust-analyzer only reads the sources, and it calls `cargo metadata`, which may download dependency sources but doesn't build them.

`cargo metadata` and rustup read configuration from the source tree, and that configuration could name a program to run. Before analysis, every `.cargo/config(.toml)` and `rust-toolchain(.toml)` is therefore deleted from the temporary copies. Symlinks are deleted too, so that no file outside the tree ends up in the page.

With `--trust-project-code`, rust-analyzer runs build scripts (via `cargo check`) and expands procedural macros with the toolchain's proc-macro server, and the project's `.cargo/config.toml` and `rust-toolchain.toml` are kept. This executes code from the pull request, so use it only in a sandbox (a container or VM without credentials). Symlinks are still removed.

`--semver-checks` executes code from the pull request too, for its own step only: cargo-semver-checks builds both versions' rustdoc JSON, which runs build scripts and procedural macros. It works on the same sanitized copies, so without `--trust-project-code` the project's `.cargo/config.toml` and `rust-toolchain.toml` are not used for that build either.

The page renders doc comments as Markdown with raw HTML shown as text, so the analyzed code cannot inject markup or script.

## Limitations

This is a prototype, built on one rule: **everything shown is either determined or left out**, never guessed. When something can't be determined, the page says so (e.g. "3 call sites not resolved") instead of filling the gap.

What that leaves out:

- **Calls.** With rust-analyzer, a call site stays unresolved when rust-analyzer can't determine its target, for example code behind a `cfg` that is inactive with all features enabled. Without it (`--syntax-only`, or if loading the workspace fails), only calls through paths (`foo()`, `Type::new()`) outside macros are shown; method calls and calls inside macros are counted as unresolved.
- **Names.** Paths are resolved syntactically by Rust's rules: name spaces, imports, glob imports (only what they make visible), `self`/`super`/`crate`, the prelude of the crate's edition. A name is left unlinked when it is ambiguous within an item, when it could be a local variable, a generic parameter or an item declared inside the body, when a glob import from another crate could provide it, or when it comes from a crate that isn't a declared dependency.
- **Generated code.** The syntactic pass never sees code generated by procedural macros (including derives) or build scripts. rust-analyzer sees it only with `--trust-project-code`.
- **Targets.** Besides each package's library, its binaries (`src/main.rs`, `src/bin/`, `[[bin]]`), build script, examples, integration tests and benches are analyzed, each as a crate of its own (`binary repl`, `test calc`, `build script`, …). They use the library by its name, and they are never part of the public API.
- **`cfg`.** In libraries and binaries, `#[test]` functions and items behind `#[cfg(test)]` (and everything inside such a module, impl or trait) are analyzed like other items, marked **test-only**, and never part of the public API; in integration tests and benches they are what the target is about, so they are not marked. Other `cfg` attributes are ignored by the syntactic pass, so code behind every `cfg` is shown. Two definitions at one path (`#[cfg(unix)]` and `#[cfg(windows)]` variants of a function, impls of a trait for `W<u8>` and `W<u16>`) are both kept, the later one with its `cfg` (or kind, or a number) in its id.
- **External crates.** Their items are described (declaration and docs) only with rust-analyzer, and only when rust-analyzer's definition has the same name and crate as the path the syntactic pass resolved. Their calls and uses are not analyzed. Intra-doc links in their docs that rust-analyzer can't turn into URLs show as plain text.
- **Re-exports.** With rust-analyzer, the public API is the one its name resolution determines (see [Analysis](#analysis)), with the features enabled that it loads (all). Items of other crates are listed per item, also when a glob re-export (`pub use dep::*`) brings in many; a re-export that a macro generates is listed only for items of the crate itself, since it has no place in the source. Enum variants re-exported on their own (`pub use Enum::*`) are part of their enum, not listed. Without rust-analyzer, public paths follow `pub use` (named, renamed and glob) through public modules, a glob re-export also forwards the target module's own re-exports, and `#[doc(hidden)]` is applied; re-exports of other crates' items are not listed and `#[doc(inline)]` is not taken into account.
- **Moves.** An item counts as moved when exactly one item of its kind and name (and parent type) disappears in one place and appears in another, and at least half of its code is the same (see [Items](features.md#items)); otherwise it is shown as removed plus added. Names of items that moved along with it (siblings, `Self`) may point into the new module. Moved lines inside bodies are found only when they are the same apart from whitespace: a moved block that was also edited shows as removed and added lines.
- **Removed configuration.** In the default (untrusted) mode, `.cargo/config(.toml)`, `rust-toolchain(.toml)` and symlinks are deleted from both checkouts before anything runs (see [Safety](#safety)). The file list is taken before that, so changes to them are listed (a symlink as the path it points to); they are just not used by the analysis.
- **Code outside items.** Code that is no item, such as macro invocations without a name (`thread_local!`, `bitflags!`), `extern` blocks, is split into parts by syntax only (no name resolution, no call graph): such a file is also listed under "Other changed files", with a link to its module's page, a box per changed part, and a diff of any remaining lines. Imports and module declarations (`mod name;`), including their attributes and when they span several lines, don't count: their effect shows in the items.
- **JavaScript** parts are found by their syntax, not by evaluating it: functions built by other calls (`const f = memo(() => …)`) are compared as variables, object literals as a whole, and TypeScript files (`.ts`) line by line. Statements end at `;` or at a line break where JavaScript would insert one, which is approximated.
- **CSS** is compared selector by selector within one file: rules for other selectors that match the same elements, specificity and the order between different selectors, shorthands against their longhands (`margin` and `margin-top`), and other stylesheets are not taken into account. Preprocessor files (`.scss`, `.less`) are compared line by line.
- **Python, JavaScript, YAML and Markdown** are compared structurally, not semantically: renamed or moved functions, keys or steps show as removed plus added (and so do YAML list items without a `name:`, `id:` or `uses:` when one is inserted before them), and Markdown sections are matched by their heading path.
- **SemVer labels.** They are read from the definitions alone. A new required trait item counts as breaking even if the trait is sealed, type changes count as breaking even where inference or coercion would hide them, and changes in auto traits (`Send`, `Sync`) or in trait impls that macros generate are not detected, unless `--semver-checks` runs cargo-semver-checks. Added and removed derives are detected, but only in the `#[derive]` itself: an impl that a derive now generates with different bounds is not. cargo-semver-checks' findings follow its own lints, with the features it picks (all but ones named like `unstable` or `nightly`); a finding whose place and name match no item is listed on its own instead of guessed onto one.
- **PR metadata.** The title, description and branches come from `gh` when it is available. GitHub-specific Markdown in the description (`#123` references, `@` mentions, task lists, images) is shown as plain text.
- **Build time.** A clean release build takes a few minutes, mostly for rust-analyzer. `--no-default-features` builds without it.

## Development

CI runs `cargo fmt`, clippy (for the host and for wasm) and the tests, builds the demo page, and opens it in headless Chromium in the light and the dark theme (`ci/smoke.mjs`). The smoke test fails on any JavaScript error, on an empty view, and on text below the WCAG AA contrast ratio (4.5:1, or 3:1 for large text) in either theme, as [axe-core](https://github.com/dequelabs/axe-core) measures it in every view, on a canvas card and in review mode. The page and screenshots of every view are uploaded as the `demo-page` artifact of each run.

To run the smoke test locally:

```sh
cargo run --release -- dirs fixtures/demo/base fixtures/demo/head -o demo.html
npm install --no-save --no-package-lock playwright@1.63.0 axe-core@4.10.2
npx playwright install chromium
node ci/smoke.mjs demo.html smoke
```