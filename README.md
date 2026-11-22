# Rust PR Explorer (prototype)

Review a Rust pull request as **changed items** (types, traits, functions, methods, impls, modules) instead of changed lines. The tool parses the base and head versions of the code and writes a single self-contained HTML page with:

- an **overview** of the public API changes in rustdoc form,
- **module pages** that keep what a module exposes apart from what it keeps private,
- a **canvas** where you arrange item cards freely and follow calls, uses and cross links,
- an optional **3D view** that stacks modules as planes by depth.

Everything is written in Rust, including the page's front end, which is compiled to WebAssembly. The analyzed code is never built or run. For the design and the alternatives that were considered, see [docs/design.md](docs/design.md).

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

All commands write `pr-explorer.html` (change it with `-o`). Open that file in a browser. `--json analysis.json` also writes the raw analysis. `--syntax-only` skips the rust-analyzer step, which makes the run faster but turns many method calls into guesses (see below).

For `pr`, the repository is cloned without file contents (`--filter=blob:none`), and only the two revisions being compared are checked out. Without `--base`, the base is the merge base of the PR head and the default branch. For a PR that was already merged with a merge commit, the base is the state just before that merge.

The demo in `fixtures/demo` is a small calculator crate. Its "PR" adds variables (a new `env` module, a changed `evaluate` signature, new error variants), moves a helper between modules, and changes some import styles and formatting. That last part does not show up as a change.

## What you see

### Overview

Lists only the public items that were added, changed or removed, nested by module, with their rustdoc-style signature and docs. A type whose methods changed appears as context with the changed members below it. Public items of private modules that are re-exported with `pub use` are listed as well. Internal changes are summarized per module, with a link to each module's internal call graph.

### Items

Every item shows what rustdoc would show: its definition or signature (private fields hidden when looking from outside the module), its docs, and its place in the module tree.

- **Change markers**: added, removed, or changed. A changed item also says whether its *signature*, *body* or *docs* changed. Items moved to another module are marked as *moved*. If only formatting, comments or import style changed, the item is marked *formatting only* and counts as unchanged.
- **Normalized names**: paths are shortened to what a reader needs (`crate::ast::Expr` → `Expr`). Every name links to its item, and hovering a name shows its fully qualified path.
- **Canvas awareness**: a name whose item is already on the canvas is highlighted, and hovering it outlines that card.
- **Source on demand**: each card has a collapsible source view, shown as a diff against the base version when the item changed.

### Canvas

- Click any name to place its item next to the current card, with a connector between the two.
- **calls ▸** shows callees and **◂ callers** shows callers. **◂ used by** shows where a type or trait is used, broken down into signatures, fields, impls and bodies.
- Edges: solid for calls, dotted for uses, dashed for method calls that were guessed from the method name alone.
- Drag a card by its header onto another card's header to combine them into **tabs**. *members as tabs* opens all methods of a type in one tabbed card.
- **Viewing from** sets the module the canvas is looking from. Only items that are visible from that module, under Rust's privacy rules, are shown. Hidden callers and callees are summarized ("2 callers are private to `mod lexer`") with a link to drill into that module.
- **Arrange by module** lays the cards out inside nested module frames, with each module's private items in a dashed zone.

### 3D view

Each module is a plane. Outer modules sit on top and nested modules below. Filled dots are items the module exposes, hollow dots are private items. Calls between planes cross module boundaries. *Side view* shows the layers from the side, *From above* together with *focus* shows one plane's internal graph. Drag to rotate and click a node to open it on the canvas.

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
   - Its type-checked answers replace the method calls the syntactic pass could only guess.
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

A pull request is untrusted code. The tool never compiles or runs it: no `cargo build`, no build scripts, no procedural macros. rust-analyzer only reads the sources, and it calls `cargo metadata`, which may download dependency sources but doesn't build them.

`cargo metadata` and rustup read configuration from the source tree, and that configuration could name a program to run. Before analysis, every `.cargo/config(.toml)` and `rust-toolchain(.toml)` is therefore deleted from the temporary copies. Symlinks are deleted too, so that no file outside the tree ends up in the page.

The page renders doc comments as Markdown with raw HTML shown as text, so the analyzed code cannot inject markup or script.

## Limitations

This is a prototype.

- Names in paths are resolved syntactically.
- Method calls are type-checked by rust-analyzer, except calls inside `format_args!`-style macro arguments, and except when rust-analyzer can't load the workspace. Those cases, and `--syntax-only` runs, fall back to guessing the target from a method name that is unique in the crate. Guesses are marked as *inferred* and drawn dashed.
- Code generated by procedural macros (including derives) and by build scripts is not seen. Items behind `#[cfg(test)]` and `#[test]` functions are skipped. Other `cfg` attributes are ignored, so code behind every `cfg` is analyzed.
- Items of external crates are not analyzed. Their names show their full path on hover but don't link anywhere.
- Re-exports mark the target item as public, but the item is still shown at its definition path.
- Moves are detected only when exactly one item of that kind and name disappears in one place and appears in another.
- PR metadata (title, description) is not fetched. Only `git` access to the repository is used.
- A clean release build takes a few minutes, mostly for rust-analyzer. `--no-default-features` builds without it.
