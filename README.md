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
# Optional: a smaller page (about 0.35 MB less), at the cost of compiling
# Binaryen's wasm-opt (C++) from source once, about 5 more minutes.
cargo build --release --features wasm-opt

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

For `git`, the path may also be a subdirectory of the repository (then only that subdirectory is analyzed and listed) or a bare repository. For `dirs`, `target/` and `node_modules/` directories are skipped.

For `pr`, the repository is cloned without file contents (`--filter=blob:none`), and only the two revisions being compared are checked out. If the GitHub CLI (`gh`) is installed and logged in, `gh pr view` supplies the PR's title, description, base branch and its own branch; the page header then shows the repository, PR number and title, `base-branch @ commit ← pr-branch @ commit`, and the overview starts with the description (folded when it is longer than 20 lines). The description is rendered as Markdown like doc comments: raw HTML is shown as text, HTML comments (such as PR template instructions) are left out, and images show only their alt text, so the page loads nothing from elsewhere. Without `gh`, the title, description and PR branch are left out. Without `--base`, the base is the merge base of the PR head and the PR's base branch (without `gh`: the default branch). For a PR that was already merged with a merge commit, the base is the state just before that merge.

The demo in `fixtures/demo` is a small calculator crate. Its "PR" adds variables (a new `env` module, a changed `evaluate` signature, new error variants), moves a helper between modules, extracts a helper method from the parser's loop, re-exports `ast::Op` at the crate root and drops the root re-export of `parser::Parser`, extends one doc comment (shown as a word diff), and changes some import styles and formatting. That last part does not show up as a change.

## GitHub Action

The repository is also a GitHub Action that builds the page for every pull request and links it in a comment on the PR:

```yaml
# .github/workflows/pr-explorer.yml
name: PR Explorer
on:
  pull_request:
  # Optional, to fill the build cache that new pull requests start from.
  push:
    branches: [main]
permissions:
  contents: read
  pull-requests: write
jobs:
  explore:
    runs-on: ubuntu-latest
    steps:
      - uses: phil-opp/diff-viewer@main
```

The action builds the tool, runs `pr` on the pull request and uploads the page as a workflow artifact. The artifact is a single unzipped HTML file, so the link opens the page directly in the browser. The comment is posted once and updated on every push, with a one-line summary of the changed items. The link is also in the job summary.

Build time: the built tool is cached under a hash of its sources, so once a run has built it, later runs skip the build (in a repository that only uses the action, that is every run after the first). The build's dependencies, mainly rust-analyzer, are cached on the default branch, the only branch whose caches every pull request can read, so the `push` trigger above keeps new pull requests from building them from scratch (about 5 minutes). On a push, the action only builds and caches the tool. The tool is built with the `action` profile (see `Cargo.toml`): dependencies optimized, the tool's own crates not, which builds faster than `--release` and analyzes as fast.

To keep the action out of the repository's cache storage (10 GB per repository, shared with other workflows), set `cache: false`: every run then builds the tool from scratch, about 7 minutes. `save-cache: false` caches only the built tool (about 12 MB) and not the dependencies (about 200 MB).

Who can open the page: anyone logged in to GitHub with access to the repository, the same people who can see the workflow run. It expires with the run's artifacts (90 days unless the repository sets otherwise, or the `retention-days` input).

Inputs: `comment: false` leaves out the comment, `syntax-only: true` skips rust-analyzer, and `pr` and `repository` pick another pull request (by default the one that triggered the workflow). The `page-url` output is the link.

The action never builds or runs the PR's code, so it has no `--trust-project-code` input: the token is in the analysis step's environment. On a PR from a fork, the token GitHub gives the workflow is read-only, so the page is built and uploaded but the comment fails with a warning; the link is still in the job summary.

## What you see

### Overview

Lists only the public items that were added, changed or removed, nested by module, with their rustdoc-style signature and docs. A type whose methods changed appears as context with the changed members below it. Items are listed where rustdoc documents them: at their definition path when that is public, otherwise at a re-export, marked "re-export of …" (a `#[doc(inline)]` re-export first, a `#[doc(no_inline)]` one last). Items marked `#[doc(hidden)]`, or reachable only through hidden modules or hidden `pub use`s, are not part of the public API; an item that becomes hidden shows as "no longer public". Items of other crates that a crate re-exports (`pub use serde::Serialize;`) are part of its API too: they are listed as re-exports with the other crate's docs, so adding, removing or retargeting such a `pub use` is an API change. Internal changes are summarized per module, with a link to each module's internal call graph.

Sections without changes are left out, on the overview and on the module pages; when only internal items changed, the "Internal changes" heading notes that the public API is unchanged. What a section lists is explained on hover over the **?** next to its heading, not in an intro paragraph.

Each public API change is labeled **breaking**, **non-breaking** or **check by hand**, following [Cargo's SemVer rules](https://doc.rust-lang.org/cargo/reference/semver.html), with the reasons listed under the label (and on hover). The top of the section counts them and names the version bump they need. Breaking, for example: removing an item or one of its public paths, adding a variant to an enum that isn't `#[non_exhaustive]`, adding any field to a struct whose fields are all public, a changed function signature, a function that is no longer `const` or is now `unsafe`, a new trait item without a default, a new supertrait. Non-breaking: new items and paths, new fields next to private ones, a type parameter with a default, `const` added, `unsafe` removed, a changed constant value. "Check by hand" is for what the definitions can't settle: a changed macro, `#[repr]` or enum discriminant. Body and doc changes are no API change. Members of trait impls are labeled with their impl.

The sidebar lists the modules that changed or contain changes, with a status symbol and the number of changed items in each. Unchanged modules are left out (except the open module and its parents); **show unchanged modules** lists them too. When the PR changes no Rust item, the module tree is left out, except on the module pages, where it is the way to browse the modules.

Below the modules, the sidebar lists every changed file with its status and its added and removed lines (the total is in the heading). A Rust file whose changes are all inside items opens its module's page; any other file scrolls to its entry in the overview.

### Files outside the Rust analysis

Every file the PR changes is accounted for:
- **Dependencies and package settings**, at the top: changes in each `Cargo.toml` as entries (dependencies in every section, including target-specific ones, features, workspace members, package version, edition and minimum Rust version), not as text. Lock files (`Cargo.lock`, `uv.lock`, `poetry.lock`) are listed there too, as the packages whose locked version changed: updated (labeled breaking or compatible by Cargo's semver rules, or major, minor or patch for Python, and downgrades), added and removed. A package that keeps its version but now comes from another source (a git repository instead of the registry) or has another checksum is flagged, since the code it stands for changed.
- **Other changed files**, at the end: each changed file the Rust analysis doesn't cover, with its +/− counts.
  - **Python** files are compared by function and class (nested ones under their `__qualname__`, e.g. `Corpus.build` or `f.<locals>.g`). For each one: added, removed, or what changed: the header (diffed), the docstring, the body; changes in only whitespace or comments are marked as such. Changes to top-level statements are noted. A small tokenizer tracks strings, comments, brackets and line continuations, so that `def` inside a string or a bracket is never taken for a definition.
  - **Markdown** files are compared section by section (headings outside code fences): changed paragraphs are marked as in doc comments, with word-level marks where safe, added sections get a bar, removed headings are struck through, and unchanged sections are counted.
  - **JavaScript** files (`.js`, `.mjs`, `.cjs`, `.jsx`) are compared by function, class and method: function declarations and `const f = (…) => …`, classes with their methods, getters, setters, fields and static blocks (`Cart.add`), other top-level `const`/`let`/`var` declarations, and test blocks (`describe("Cart") › it("counts items")`, any top-level call whose first argument is a string, with such calls in its callback nested under it). For each one: added, removed, or what changed: the header (diffed), the JSDoc comment, the body; changes in only whitespace or comments are marked as such. Changes to other top-level code (imports, statements) are noted. A tokenizer follows strings, template literals with their `${…}` expressions, regular expression literals and comments, so braces inside them don't count. Diffs have syntax colors.
  - **YAML** files (`.yml`, `.yaml`), such as GitHub Actions workflows, are compared by key: each top-level key is a box, and a long block (over 12 lines) is split into its child keys or list items, down to four levels. List items are named by their `name:`, `id:` or `uses:` (a workflow's `jobs.test.steps[Build]`), or else by their position. Changes in only whitespace or comments are marked as such. Their diffs have syntax colors: keys, strings, numbers, booleans, comments, anchors and tags, and `${{ … }}` expressions wherever they appear; block scalars (`run: |`) are followed across lines, so a script's lines aren't taken for keys.
  - **CSS** files are compared by selector. The stylesheet is parsed into rules (following comments, strings, brackets, at-rules and native `&` nesting), and the declarations of every rule for the same selector in the same `@media`/`@supports`/… context are merged by the cascade: a later declaration wins unless an earlier one is `!important`. Each changed selector is a box listing its added, changed and removed properties, with the other properties it keeps for context, and the lines of the rules it was merged from. A declaration that changed without changing what applies (one a later rule overrides) is shown with why it has no effect. Selectors of one rule that changed the same way share a box (`h1, h2`); `@import` and other statements are boxes of their own. Changes in only comments or formatting are noted as such. Their diffs have syntax colors: selectors, properties, numbers and colors, strings, at-rules and comments.
  - **Rust code outside items**: in a Rust file whose items are analyzed, the code that belongs to no item (macro calls such as `thread_local!`, `extern` blocks) gets a box per macro call or block. Each box shows the header with its attributes and the diff of its lines; doc comment and formatting-only changes are marked. The file's card links its module's page for the items themselves.
  - **Other text files** show a line diff (shown directly when small).
  - **Binary** and very large files (over 1 MB) are listed without a diff.
  - **Generated and vendored** files (lock files, `vendor/`, `third_party/`, `node_modules/`, minified files, files marked `@generated` or "do not edit") are collapsed into one group.
- A PR without any Rust change says so instead of showing empty sections.

### Items

Every item shows what rustdoc would show: its definition or signature (private fields hidden when looking from outside the module), its docs, and its place in the module tree.

- **Change markers**: inside a changed item, a bar on the left marks each part that changed, so the eye finds changes without reading labels. Each kind differs in line style as well as color, so it doesn't depend on telling red from green: changed, the most common, is a plain solid amber line; added is a dotted green line and removed a dashed red line. Removed text is also struck through, and status symbols differ in shape: a small square with a plus (added), a dot (changed) or a minus (removed); unchanged items have no symbol. Neighboring parts that changed the same way share one bar.
- **New and removed items** are marked as a whole instead: a NEW or REMOVED badge, and a tinted background with a border around the item (solid for new, dashed for removed). A new type and its new methods form one group, and the badge appears once, on the type; inside a new module only the module has it.
  - The signature block gets a bar when the signature changed. An unchanged signature, shown only for reference, is drawn muted and outlined instead.
  - Docs are compared paragraph by paragraph (lists and code blocks count as one paragraph). Changed and added paragraphs get a bar; removed paragraphs are shown struck through. When a paragraph changed only a few words, the words are marked inside it (inserted underlined, removed struck through). This is done only when it can't change how the Markdown renders: not for changes to link text or targets, Markdown syntax, the start of a line, or code blocks. Otherwise the previous text of the paragraph can be expanded. **Compare docs side by side** opens both versions next to each other, with changed paragraphs aligned.
  - After the docs, a line such as **Body changed +9 −6** expands to the source diff. A small change (at most 4 changed lines) is shown right away: the changed lines with two lines of context, and the whole source behind a toggle.
  - In source diffs, every resolved name links to its item, on added, removed and unchanged lines. A name of an item the PR adds is tagged **new**, and a call of a function or method the PR changes is tagged **changed** (not a recursive call, and not names of changed types, which would be everywhere), so the callees that need a review stand out. In review mode, the tag gets a ✓ once that item is reviewed.
  - **Public paths**: an item that becomes public under a new path, for example through a new `pub use`, or loses a public path, is a change: "+ now public as `crate::Op`", "− no longer public as `crate::Parser`".
  - Items moved to another module are marked as *moved*, also when they were edited while moving: an item is paired with the one of the same kind and name (and parent type) that disappeared elsewhere, if that is the only candidate on both sides and at least half of its code is the same. Its changes are then shown as for any changed item. If only formatting, comments or import style changed, the item is marked *formatting only* and counts as unchanged.
  - **Moved lines**: code moved between items, for example into an extracted helper, is marked as moved instead of as removed and added (like `git diff --color-moved`). A block of removed lines that reappears among the added lines of another item, or elsewhere in the same item, the same apart from whitespace and with at least 20 letters and digits, keeps its − or + but gets its own color and a note on its first line that links to the other end ("moved to `Parser::next_op`, line 47"). The diff's summary counts the moved lines. Lines that were only indented differently are not counted as moved.
- **Settings** (⚙ in the header, remembered per browser): source diffs inline or side by side, hiding whitespace-only changes (like GitHub), and the code font and size. The default font is Iosevka, embedded in the page; ligatures are off so that `->` and `=>` show as written.
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

### Review mode

**Start review** in the header turns on review mode, for going through a PR one item at a time (**Exit review**, at the end of the review bar, hides the controls again and keeps your marks):

- Every added, changed or removed item (functions, methods, types, trait impls, …) and every file listed under "Other changed files" or "Dependencies and package settings" gets the three choices of a GitHub review, on the overview, the module pages and canvas cards: **✓ Approve**, **✗ Request changes** and **✎ Comment**. Approve marks the item and moves to the next one without a verdict, in call order: first a changed item that no other changed item calls, then the changed items it calls (also through unchanged functions in between), depth-first in the order its body calls them, then the next such item. Items are taken in reading order where calls don't decide, and files come last. The next item is shown where it is listed: on the same page if it is there, else on its module's page (or, on the canvas, as a new card next to its caller). The other two first ask for a comment (Markdown; Ctrl+Enter saves). **Next ▸** in the header, or the `n` key, moves on without a verdict.
- A changed item whose source diff has several separate changes (hunks, between folded unchanged lines) can also be reviewed hunk by hunk: each hunk has its own **Approve**, **Request changes** and **Comment**, and moves on to the item's next hunk. When every hunk has a verdict, the item gets one too (changes requested if any hunk needs them, else commented if any has a comment, else approved). A hunk's mark depends only on its changed lines, so after new commits only the hunks that changed need a review again. A comment on a hunk goes on that hunk's first changed line when posted.
- A reviewed item shows its verdict and the start of its comment everywhere it appears, also with review mode off, and its row is folded to one dimmed line. **▾ Expand** shows it whole again, **Edit** changes the verdict or the comment, and clicking the verdict removes it.
- The header shows how many of the PR's items and files have a verdict; in review mode the sidebar counts what is left per module (✓ when nothing is). **Clear marks** removes every mark.
- Marks are saved in the browser's local storage, per pull request (for `git`, per repository or subdirectory; for `dirs`, per pair of directories; the page holds only a hash of their paths), so they carry over to a page generated again after new commits. A mark remembers the item's diff: if the diff is different in the new page, the item shows **⚠ Changed since review** and counts as not reviewed.
- A mark also keeps the version that was reviewed. **Show changes** next to a stale mark compares it with the current one: a line diff of the PR's version since the review, of the base version if that changed too (after a rebase), and a list of anything else that differs (status, public paths, what the signature's names refer to). For a file outside the Rust analysis, only the parts its diff shows are kept, except for Markdown files, which are kept whole. Marks made by pages from before this was added have no saved version; the dialog says so.
- **Finish review…** sums up the verdicts and comments, suggests an overall verdict (request changes if any item needs them, approve if every item is approved, else comment) and takes a summary. For a pull request, it offers one command to copy and run, which posts the review (verdict, summary and inline comments) with the GitHub CLI: `printf '%s' '{…}' | gh api --method POST repos/OWNER/REPO/pulls/NUMBER/reviews --input -`. The JSON is in the form [GitHub's API for creating a review](https://docs.github.com/en/rest/pulls/reviews#create-a-review-for-a-pull-request) takes, and is quoted so that the command works the same in bash, zsh and fish. The page itself never talks to GitHub and holds no token, since anyone who can open it could read one; `gh` uses your own login. Each comment goes on a line of its item that GitHub's diff shows (its first added line, else its first removed line, else a line of context), on the commit the page was generated for. Comments that fit on no line (a binary file, an item without a diff line nearby) go into the review's text, and comments on items that changed since you reviewed them are left out. The review can also be copied as Markdown, for an ordinary comment.
- **Keyboard**, in review mode (unless typing in a field): `n` goes to the next unreviewed item; `a`, `r` and `c` approve, request changes on or comment on the current item (or the current hunk); `j` and `k` move to the item's next and previous hunk (`j` after the last one goes on to the next item); `Enter` follows the current item's first reference to a new or changed item that isn't reviewed yet (the ones its source diff tags), and shows that item; `?` (or the **?** button in the review bar) lists the shortcuts. The current item is the one that **Next ▸**, a verdict or a key last went to, as long as the open view shows it, else the first unreviewed one in view.
- Nothing leaves the browser: marks are not shared between browsers or people.

### Canvas

- Click any name to place its item next to the current card, with a connector between the two.
- **◂ callers** shows callers, to the left of the card, and **calls ▸** shows callees, to the right. **◂ used by** shows where a type or trait is used, broken down into signatures, fields, impls and bodies.
- Edges: solid for calls, dotted for uses. Every edge is resolved, never guessed. A badge in a card's footer ("3 unresolved") counts the call sites in its body that could not be resolved, so a missing edge is visible; its tooltip says why.
- Drag a card by its header onto another card's header to combine them into **tabs**. *members as tabs* opens all methods of a type in one tabbed card.
- **Viewing from** sets the module the canvas is looking from. Only items that are visible from that module, under Rust's privacy rules, are shown. Hidden callers and callees are counted in a badge ("2 private callers"); its tooltip names the modules they are private to, and a click views the canvas from that module.
- Cards of unchanged items (an unchanged callee, say) show only their signature. **docs & source ▾** expands one, **signature only ▴** folds it again.
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
- **Re-exports.** With rust-analyzer, the public API is the one its name resolution determines (see [How it works](#how-it-works)), with the features enabled that it loads (all). Items of other crates are listed per item, also when a glob re-export (`pub use dep::*`) brings in many; a re-export that a macro generates is listed only for items of the crate itself, since it has no place in the source. Enum variants re-exported on their own (`pub use Enum::*`) are part of their enum, not listed. Without rust-analyzer, public paths follow `pub use` (named, renamed and glob) through public modules, a glob re-export also forwards the target module's own re-exports, and `#[doc(hidden)]` is applied; re-exports of other crates' items are not listed and `#[doc(inline)]` is not taken into account.
- **Moves.** An item counts as moved when exactly one item of its kind and name (and parent type) disappears in one place and appears in another, and at least half of its code is the same (see [Items](#items)); otherwise it is shown as removed plus added. Names of items that moved along with it (siblings, `Self`) may point into the new module. Moved lines inside bodies are found only when they are the same apart from whitespace: a moved block that was also edited shows as removed and added lines.
- **Removed configuration.** In the default (untrusted) mode, `.cargo/config(.toml)`, `rust-toolchain(.toml)` and symlinks are deleted from both checkouts before anything runs (see [Safety](#safety)). The file list is taken before that, so changes to them are listed (a symlink as the path it points to); they are just not used by the analysis.
- **Code outside items.** Code that is no item, such as macro invocations without a name (`thread_local!`, `bitflags!`), `extern` blocks, is split into parts by syntax only (no name resolution, no call graph): such a file is also listed under "Other changed files", with a link to its module's page, a box per changed part, and a diff of any remaining lines. Imports and module declarations (`mod name;`), including their attributes and when they span several lines, don't count: their effect shows in the items.
- **JavaScript** parts are found by their syntax, not by evaluating it: functions built by other calls (`const f = memo(() => …)`) are compared as variables, object literals as a whole, and TypeScript files (`.ts`) line by line. Statements end at `;` or at a line break where JavaScript would insert one, which is approximated.
- **CSS** is compared selector by selector within one file: rules for other selectors that match the same elements, specificity and the order between different selectors, shorthands against their longhands (`margin` and `margin-top`), and other stylesheets are not taken into account. Preprocessor files (`.scss`, `.less`) are compared line by line.
- **Python, JavaScript, YAML and Markdown** are compared structurally, not semantically: renamed or moved functions, keys or steps show as removed plus added (and so do YAML list items without a `name:`, `id:` or `uses:` when one is inserted before them), and Markdown sections are matched by their heading path.
- **SemVer labels.** They are read from the definitions alone. A new required trait item counts as breaking even if the trait is sealed, type changes count as breaking even where inference or coercion would hide them, and changes in auto traits (`Send`, `Sync`) or in trait impls generated by derives the syntactic pass doesn't see are not detected.
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
