# Rust PR Explorer (prototype)

Review a Rust pull request as **changed items** (types, traits, functions, methods, impls, modules) instead of changed lines. The tool parses the base and head versions of the code and writes a single self-contained HTML page with:

- an **overview** of the public API changes in rustdoc form,
- **module pages** that keep what a module exposes apart from what it keeps private,
- a **canvas** where you arrange item cards freely and follow calls, uses and cross links,
- an optional **3D view** that stacks modules as planes by depth.

## Usage

```sh
cargo build --release

# A GitHub pull request (public repositories; needs `git`).
./target/release/rust-pr-explorer pr https://github.com/rust-lang/log/pull/613
./target/release/rust-pr-explorer pr rust-lang/log#613 --base main

# Two revisions of a local repository.
./target/release/rust-pr-explorer git path/to/repo main my-branch

# Two directories, e.g. the bundled demo.
./target/release/rust-pr-explorer dirs fixtures/demo/base fixtures/demo/head
```

All commands write `pr-explorer.html` (change it with `-o`). Open that file in a browser. `--json analysis.json` also writes the raw analysis.

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

`src/source.rs` gets two source trees: a git clone and checkout, `git archive`, or plain directories. It then finds the crates in them (library first, otherwise binary; workspace members included).

`src/analyze.rs` processes each tree with [`syn`](https://docs.rs/syn) in three passes:

1. Build the module tree (following `mod` declarations, `mod.rs`/`name.rs` and `#[path]`) and collect definitions and `use` imports per module.
2. Resolve `impl` blocks to their self type and trait. Methods get stable ids such as `crate::parser::Parser::parse` or `crate::ast::Expr::<impl Display>::fmt`, independent of which file or impl block they are in.
3. For each item, resolve every path in its signature and body (`crate::`, `self::`, `super::`, `Self`, imports, glob imports, renames, re-exports and the std prelude). Then pretty-print the signature with [`prettyplease`](https://docs.rs/prettyplease), record calls and uses, and extract docs and intra-doc links.

`src/merge.rs` matches items by id across base and head. Changes are detected on normalized forms: pretty-printed tokens plus the fully qualified targets of every name. This is why reformatting and import changes are not reported, while a `use` that now points at a different item is.

The page (`assets/`) is plain HTML, CSS and JavaScript with no dependencies. The analysis is embedded in it as JSON.

## Limitations

This is a prototype. Name resolution is syntactic: there is no type checking or macro expansion.

- `x.method()` is resolved only when the receiver is `self` or a function parameter with a local type. Otherwise a method that is unique within the crate is used as a guess, marked as *inferred*. Calls on other receivers can be missed.
- Code generated by macros, including derives, is not seen. Items behind `#[cfg(test)]` and `#[test]` functions are skipped. Other `cfg` attributes are ignored, so both sides of a `cfg` are analyzed.
- Items of external crates are not analyzed. Their names show their path on hover but do not link anywhere.
- Re-exports mark the target item as public, but the item is still shown at its definition path.
- Moves are detected only when exactly one item of that kind and name disappears in one place and appears in another.
- PR metadata (title, description) is not fetched. Only `git` access to the repository is used.
