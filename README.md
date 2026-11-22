# Rust PR Explorer (prototype)

Review a Rust pull request as **changed items** (types, traits, functions, methods, impls, modules) instead of changed lines. The tool parses the base and head versions of the code and writes a single self-contained HTML page with:

- an **overview** of the public API changes in rustdoc form,
- **module pages** that keep what a module exposes apart from what it keeps private,
- a **canvas** where you arrange item cards freely and follow calls, uses and cross links,
- a **structure view** that shows how the change spreads over the modules, as a module map or a matrix.

Everything is written in Rust, including the page's front end, which is compiled to WebAssembly. By default the analyzed code is never built or run. For the design and the alternatives that were considered, see [docs/design.md](docs/design.md).

## Usage

```sh
# Once: the viewer is compiled to WebAssembly, and rust-analyzer needs the
# standard library's source for type information.
rustup target add wasm32-unknown-unknown
rustup component add rust-src

cargo build --release

# A GitHub pull request (public repositories; needs `git`).
./target/release/rust-pr-explorer pr https://github.com/rust-lang/log/pull/613
./target/release/rust-pr-explorer pr rust-lang/log#613 --base main

# Two revisions of a local repository.
./target/release/rust-pr-explorer git path/to/repo main my-branch

# Two directories, e.g. the bundled demo.
./target/release/rust-pr-explorer dirs fixtures/demo/base fixtures/demo/head
```

All commands write `pr-explorer.html` (change it with `-o`). Open that file in a browser. `--json analysis.json` also writes the raw analysis. `--syntax-only` skips the rust-analyzer step, which makes the run faster but leaves method calls and calls inside macros unresolved (see below).

In a sandboxed environment, `--trust-project-code` (or `RUST_PR_EXPLORER_TRUST_PROJECT_CODE=1`) lets rust-analyzer run the project's build scripts and procedural macros. It then also resolves calls involving generated code, such as types from `include!(concat!(env!("OUT_DIR"), …))` or items produced by proc macros. See [Safety](#safety).

For `pr`, the repository is cloned without file contents (`--filter=blob:none`), and only the two revisions being compared are checked out. If the GitHub CLI (`gh`) is installed and logged in, `gh pr view` supplies the PR's title, its base branch and its own branch; the page header then shows the repository, PR number and title, `base-branch @ commit ← pr-branch @ commit`. Without `gh`, the title and PR branch are left out. Without `--base`, the base is the merge base of the PR head and the PR's base branch (without `gh`: the default branch). For a PR that was already merged with a merge commit, the base is the state just before that merge.

The demo in `fixtures/demo` is a small calculator crate. Its "PR" adds variables (a new `env` module, a changed `evaluate` signature, new error variants), moves a helper between modules, re-exports `ast::Op` at the crate root and drops the root re-export of `parser::Parser`, extends one doc comment (shown as a word diff), and changes some import styles and formatting. That last part does not show up as a change.

## What you see

### Overview

Lists only the public items that were added, changed or removed, nested by module, with their rustdoc-style signature and docs. A type whose methods changed appears as context with the changed members below it. Items are listed where rustdoc lists them: under their shortest public path, so an item of a private module that is re-exported with `pub use` appears at the re-export, marked "re-export of …". Internal changes are summarized per module, with a link to each module's internal call graph.

### Files outside the Rust analysis

Every file the PR changes is accounted for:
- **Dependencies and package settings**, at the top: changes in each `Cargo.toml` as entries (dependencies in every section, including target-specific ones, features, workspace members, package version, edition and minimum Rust version), not as text.
- **Other changed files**, at the end: each changed file the Rust analysis doesn't cover, with its +/− counts.
  - **Python** files are compared by function and class (nested ones under their `__qualname__`, e.g. `Corpus.build` or `f.<locals>.g`). For each one: added, removed, or what changed: the header (diffed), the docstring, the body; changes in only whitespace or comments are marked as such. Changes to top-level statements are noted. A small tokenizer tracks strings, comments, brackets and line continuations, so that `def` inside a string or a bracket is never taken for a definition.
  - **Markdown** files are compared section by section (headings outside code fences): changed paragraphs are marked as in doc comments, with word-level marks where safe, added sections get a bar, removed headings are struck through, and unchanged sections are counted.
  - **Other text files** show a line diff (shown directly when small).
  - **Binary** and very large files (over 1 MB) are listed without a diff.
  - **Generated and vendored** files (lock files, `vendor/`, `third_party/`, `node_modules/`, minified files, files marked `@generated` or "do not edit") are collapsed into one group.
- A PR without any Rust change says so instead of showing empty sections.

### Items

Every item shows what rustdoc would show: its definition or signature (private fields hidden when looking from outside the module), its docs, and its place in the module tree.

- **Change markers**: inside a changed item, a bar on the left marks each part that changed, so the eye finds changes without reading labels. Each kind differs in line style as well as color, so it doesn't depend on telling red from green: changed, the most common, is a plain solid amber line; added is a dotted green line and removed a dashed red line. Removed text is also struck through, and status dots differ in shape (disc, diamond, hollow square). Neighboring parts that changed the same way share one bar.
- **New and removed items** are marked as a whole instead: a NEW or REMOVED badge, and a tinted background with a border around the item (solid for new, dashed for removed). A new type and its new methods form one group, and the badge appears once, on the type; inside a new module only the module has it.
  - The signature block gets a bar when the signature changed. An unchanged signature, shown only for reference, is drawn muted and outlined instead.
  - Docs are compared paragraph by paragraph (lists and code blocks count as one paragraph). Changed and added paragraphs get a bar; removed paragraphs are shown struck through. When a paragraph changed only a few words, the words are marked inside it (inserted underlined, removed struck through). This is done only when it can't change how the Markdown renders: not for changes to link text or targets, Markdown syntax, the start of a line, or code blocks. Otherwise the previous text of the paragraph can be expanded. **Compare docs side by side** opens both versions next to each other, with changed paragraphs aligned.
  - After the docs, a line such as **Body changed +9 −6** expands to the source diff. A small change (at most 4 changed lines) is shown right away: the changed lines with two lines of context, and the whole source behind a toggle.
  - **Public paths**: an item that becomes public under a new path, for example through a new `pub use`, or loses a public path, is a change: "+ now public as `crate::Op`", "− no longer public as `crate::Parser`".
- **Settings** (⚙ in the header, remembered per browser): source diffs inline or side by side, hiding whitespace-only changes (like GitHub), and the code font and size. The default font is Iosevka, embedded in the page; ligatures are off so that `->` and `=>` show as written.
  - Items moved to another module are marked as *moved*. If only formatting, comments or import style changed, the item is marked *formatting only* and counts as unchanged.
- **Private fields**: seen from outside its module, a type is shown as rustdoc shows it, with private fields hidden. An expandable "Full definition" section below it shows every field, with a diff if the definition changed.
- **Signature layout**: long function signatures are broken into lines the way rustfmt does (one parameter per line, `where` predicates on their own lines), never soft-wrapped.
- **Signature diffs**: a changed signature is diffed by structure, not text.
  - Parameters, generic parameters, where-clauses, fields and variants are matched by name, so reordering or renaming one doesn't scramble the rest.
  - Types are compared as trees: `f64` → `Result<f64, Error>` shows one green box around the new wrapper, with the kept `f64` inside; unwrapping gets a dashed red box.
  - Under the signature, one note per change says what changed, e.g. "parameter `env: &Env` added" or "return type wrapped in `Result<_, Error>`". Hovering a part of a change, or its note, highlights everything that belongs to it.
  - Seen from outside the defining module, the public form of the definition is diffed, and a change to private fields only is noted as such. Signatures the analysis can't parse this way (traits, type aliases, …) fall back to a token-level diff.
- **Normalized names**: paths are shortened to what a reader needs (`crate::ast::Expr` → `Expr`). Every name links to its item, and hovering a name shows its fully qualified path.
- **Canvas awareness**: a name whose item is already on the canvas is highlighted, and hovering it outlines that card.
- **Source on demand**: each card has a collapsible source view, shown as a diff against the base version when the item changed.
- **Standard library and dependencies**: names from outside the analyzed crates link to a read-only card with their declaration and docs, as rust-analyzer describes them, plus a link to their documentation online. Only the items the analyzed code names are looked up, not whole crates, so this adds little time.

### Canvas

- Click any name to place its item next to the current card, with a connector between the two.
- **calls ▸** shows callees and **◂ callers** shows callers. **◂ used by** shows where a type or trait is used, broken down into signatures, fields, impls and bodies.
- Edges: solid for calls, dotted for uses. Every edge is resolved, never guessed. A card lists how many call sites in its body could not be resolved, so a missing edge is visible.
- Drag a card by its header onto another card's header to combine them into **tabs**. *members as tabs* opens all methods of a type in one tabbed card.
- **Viewing from** sets the module the canvas is looking from. Only items that are visible from that module, under Rust's privacy rules, are shown. Hidden callers and callees are summarized ("2 callers are private to `mod lexer`") with a link to drill into that module.
- **Arrange by module** lays the cards out inside nested module frames, with each module's private items in a dashed zone.

### Structure view

How the change spreads over the modules, in two modes:
- **Module map** (default): the module tree as boxes, a parent above its children. Each box lists its changed items, exposed ones first and private ones below a dashed line; private modules have a dashed border. One arrow per pair of modules whose items call each other, labeled with the number of calls. Arrows run horizontally and vertically around the boxes, never through them, and leave and enter boxes at right angles; hovering a module highlights its arrows. With many arrows they stay faint until hovered. Long lists are cut after 8 items, with a link to the module page.
- **Matrix**: modules against modules; each cell counts the calls from the row's module into the column's that involve a change. Hovering a cell lists the calls, clicking it opens them on the canvas.

"uses" adds type uses to the calls, "all items" includes unchanged items.

## How it works

The workspace has three crates:
- `crates/model`: the data passed from the analysis to the page.
- `crates/rust-pr-explorer`: the command-line tool.
- `crates/viewer`: the page's front end.

1. **Sources** (`source.rs`): getting the two source trees.
   - PRs are cloned with `git` and the two revisions checked out.
   - Local revisions are exported with `git archive`; directories are copied.
   - In every case the analysis works on temporary copies, with tool configuration and symlinks removed (see Safety).
2. **Syntactic analysis** (`analyze.rs`, [`syn`](https://docs.rs/syn)): finding items and resolving names.
   - Builds the module tree, following `mod` declarations, `mod.rs`/`name.rs` and `#[path]`.
   - Gives every item, including methods, a stable id independent of file and `impl` block, e.g. `crate::ast::Expr::<impl Display>::fmt`.
   - Resolves every path in signatures and bodies: `crate`/`self`/`super`/`Self`, imports, renames, globs, re-exports and the prelude.
   - Renders signatures in rustdoc style with [`prettyplease`](https://docs.rs/prettyplease), and records calls, uses and intra-doc links.
3. **Call resolution** (`semantic.rs`): rust-analyzer, linked as a library, is asked for each function's outgoing calls.
   - It loads the workspace with all features enabled, without running build scripts and without expanding procedural macros.
   - Its type-checked answers decide the calls. Every call site the syntactic pass found is checked, including method calls and calls inside macros (with "go to definition" where the call hierarchy doesn't list them). Sites that can't be resolved are counted on the card, not guessed.
   - If loading fails, the syntactic results are used and the page says so.
4. **Comparison** (`merge.rs`): matching items by id across base and head.
   - Changes are detected on normalized forms: pretty-printed tokens plus the fully qualified target of every name. Reformatting and changes in import style don't count.
   - A `use` that now points at a different item does count.
5. **Page**:
   - The CLI's build script compiles `crates/viewer` to WebAssembly and generates its bindings with `wasm-bindgen`.
   - The output page embeds the wasm module, the bindings and the analysis as JSON.
   - The viewer renders the views with `web-sys`, and docs with [`pulldown-cmark`](https://docs.rs/pulldown-cmark).

`unsafe_code` is forbidden in all three crates.

## Safety

A pull request is untrusted code. By default the tool never compiles or runs it: no `cargo build`, no build scripts, no procedural macros. rust-analyzer only reads the sources, and it calls `cargo metadata`, which may download dependency sources but doesn't build them.

`cargo metadata` and rustup read configuration from the source tree, and that configuration could name a program to run. Before analysis, every `.cargo/config(.toml)` and `rust-toolchain(.toml)` is therefore deleted from the temporary copies. Symlinks are deleted too, so that no file outside the tree ends up in the page.

With `--trust-project-code`, rust-analyzer runs build scripts (via `cargo check`) and expands procedural macros with the toolchain's proc-macro server, and the project's `.cargo/config.toml` and `rust-toolchain.toml` are kept. This executes code from the pull request, so use it only in a sandbox (a container or VM without credentials). Symlinks are still removed.

The page renders doc comments as Markdown with raw HTML shown as text, so the analyzed code cannot inject markup or script.

## Limitations

This is a prototype, built on one rule: **everything shown is either determined or left out**, never guessed. When something can't be determined, the page says so (e.g. "3 call sites not resolved") instead of filling the gap.

What that leaves out:

- **Calls.** With rust-analyzer, a call site stays unresolved when rust-analyzer can't determine its target, for example code behind a `cfg` that is inactive with all features enabled. Without it (`--syntax-only`, or if loading the workspace fails), only calls through paths (`foo()`, `Type::new()`) outside macros are shown; method calls and calls inside macros are counted as unresolved.
- **Names.** Paths are resolved syntactically by Rust's rules: name spaces, imports, glob imports (only what they make visible), `self`/`super`/`crate`, the prelude of the crate's edition. A name is left unlinked when it is ambiguous within an item, when it could be a local variable, a generic parameter or an item declared inside the body, when a glob import from another crate could provide it, or when it comes from a crate that isn't a declared dependency.
- **Generated code.** The syntactic pass never sees code generated by procedural macros (including derives) or build scripts. rust-analyzer sees it only with `--trust-project-code`.
- **Targets.** Besides each package's library, its binaries (`src/main.rs`, `src/bin/`, `[[bin]]`), build script, examples, integration tests and benches are analyzed, each as a crate of its own (`binary repl`, `test calc`, `build script`, …). They use the library by its name, and they are never part of the public API.
- **`cfg`.** In libraries and binaries, items behind `#[cfg(test)]` and `#[test]` functions are skipped; in integration tests and benches they are what the target is about, so they are kept. Other `cfg` attributes are ignored by the syntactic pass, so code behind every `cfg` is shown. Two definitions at one path (`#[cfg(unix)]` and `#[cfg(windows)]` variants of a function, impls of a trait for `W<u8>` and `W<u16>`) are both kept, the later one with its `cfg` (or kind, or a number) in its id.
- **External crates.** Their items are described (declaration and docs) only with rust-analyzer, and only when rust-analyzer's definition has the same name and crate as the path the syntactic pass resolved. Their calls and uses are not analyzed. Intra-doc links in their docs that rust-analyzer can't turn into URLs show as plain text.
- **Re-exports.** Public paths follow `pub use` (named, renamed and glob) through public modules; a glob re-export also forwards the target module's own re-exports. Re-exports of items from other crates are not listed, and `#[doc(hidden)]` and `#[doc(inline)]` are not taken into account.
- **Moves.** An item counts as moved only when its code is identical: the same body (for functions, constants, macros) or the same definition (for types), and exactly one item of that kind and name disappears in one place and appears in another. Names of items that moved along with it (siblings, `Self`) may point into the new module. Anything else is shown as removed plus added.
- **Removed configuration.** In the default (untrusted) mode, `.cargo/config(.toml)`, `rust-toolchain(.toml)` and symlinks are deleted from both checkouts before anything runs (see [Safety](#safety)). The file list is taken before that, so changes to them are listed (a symlink as the path it points to); they are just not used by the analysis.
- **Code outside items.** Changes in code that is no item, such as macro invocations without a name (`thread_local!`, `bitflags!`), `extern` blocks and `#[cfg(test)]` code in libraries, are counted per file: such a file is also listed under "Other changed files", with a note and its line diff.
- **Python and Markdown** are compared structurally, not semantically: renamed or moved functions show as removed plus added, and Markdown sections are matched by their heading path.
- **PR metadata.** The title and branches come from `gh` when it is available; the PR description is not shown.
- **Build time.** A clean release build takes a few minutes, mostly for rust-analyzer. `--no-default-features` builds without it.
