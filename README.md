# Rust Semantic Review (prototype)

> [!WARNING]
> This is a vibe-coded toy project, written mostly by an AI coding assistant. Expect bugs and rough edges, and use it with care. In particular, don't rely on its output as the only basis for a review.

Review a Rust pull request as **changed items** (types, traits, functions, impls, modules) instead of changed lines. The tool compares the base and head versions and writes one self-contained HTML page with an overview of the public API changes and their SemVer impact, module pages, a canvas to follow calls, and a review mode that posts to GitHub.

![The overview of the demo PR](docs/screenshot.png)

By default the analyzed code is never built or run. See [docs/features.md](docs/features.md) for what the page shows, [docs/usage.md](docs/usage.md) for details on the options, [docs/how-it-works.md](docs/how-it-works.md) for the analysis, its safety model and its limits, and [docs/design.md](docs/design.md) for the design.

## GitHub Action

Builds the page for every pull request and links it in a comment:

```yaml
# .github/workflows/rust-semantic-review.yml
name: Rust Semantic Review
on:
  pull_request:
  push:
    branches: [main] # optional: fills the build cache that PRs start from
permissions:
  contents: read
  pull-requests: write
jobs:
  explore:
    runs-on: ubuntu-latest
    steps:
      - uses: phil-opp/rust-semantic-review@main
```

| Input | Default | |
|---|---|---|
| `comment` | `true` | Post a comment that links the page (and a page of the changes since the previous comment). `workflow-run` leaves it to a second workflow, see below. |
| `syntax-only` | `false` | Skip rust-analyzer: faster, but method calls stay unresolved. |
| `trust-project-code` | `auto` | Run the PR's build scripts and proc macros, so calls into generated code resolve. `auto`: on under `pull_request`, off when another event analyzes a PR. Refused under `pull_request_target`. |
| `semver-checks` | `false` | Also run cargo-semver-checks. Builds the PR's code; refused under `pull_request_target`. |
| `cache` / `save-cache` | `true` / on the default branch | Cache the built tool / also its dependencies. |
| `cache-target` | `false` | In trusted mode, also cache the analysis' target directory: much faster, but can take several GB. |
| `retention-days` | repository setting | How long the page is kept. |
| `pr`, `repository` | the triggering PR | Analyze another pull request. |

### Pull requests from forks

On a PR from a fork, GitHub gives the workflow a read-only token, so the comment can't be posted and the link is only in the job summary. To comment there too, let the workflow above only upload the data for the comment, and post it from a second workflow, triggered by `workflow_run`, whose token can write:

```yaml
# In the workflow above:
permissions:
  contents: read
  pull-requests: read
# …
      - uses: phil-opp/rust-semantic-review@main
        with:
          comment: workflow-run
```

```yaml
# .github/workflows/rust-semantic-review-comment.yml
name: Rust Semantic Review comment
on:
  workflow_run:
    workflows: [Rust Semantic Review] # the name of the workflow above
    types: [completed]
permissions:
  actions: read
  pull-requests: write
jobs:
  comment:
    if: github.event.workflow_run.event == 'pull_request' && github.event.workflow_run.conclusion == 'success'
    runs-on: ubuntu-latest
    steps:
      - uses: phil-opp/rust-semantic-review/comment@main
```

The second workflow runs as it is on the default branch and never checks out or runs the PR's code. It treats the data from the first run as untrusted, since a fork's PR can change that workflow; see [docs/usage.md](docs/usage.md#pull-requests-from-forks).

The pages are stored as artifacts of the workflow run: opening one needs a GitHub login with access to the repository, and they expire with the run's artifacts (90 days unless the repository or `retention-days` sets otherwise).

## Command line

```sh
cargo build --release   # rustup installs the toolchain from rust-toolchain.toml

./target/release/rust-semantic-review pr rust-lang/log#613   # needs git; gh adds the title and review threads
./target/release/rust-semantic-review git path/to/repo main my-branch
./target/release/rust-semantic-review dirs fixtures/demo/base fixtures/demo/head
```

| Option | |
|---|---|
| `-o FILE` | Output page (default `rust-semantic-review.html`). |
| `--json FILE` | Also write the analysis as JSON. |
| `--syntax-only` | Skip rust-analyzer. |
| `--trust-project-code` | Run build scripts and proc macros. Only in a sandbox. |
| `--target-dir DIR` | With trust: keep the build between runs. |
| `--semver-checks` | Also run cargo-semver-checks (`cargo install cargo-semver-checks`). Only in a sandbox. |
| `--base REV` | `pr`: compare against this revision instead of the merge base. |
| `--since COMMIT` | `pr`: also write a page of what changed since an earlier commit of the PR. |

`cargo build --release --features wasm-opt` makes the page smaller, at the cost of about 5 more minutes of build time.

Crates that don't follow SemVer on purpose can mark their breaking changes as allowed:

```toml
[package.metadata.rust-semantic-review]
allow-breaking-changes = true
```

## Development

CI runs `cargo fmt`, clippy, the tests, and a headless browser smoke test of the demo page (`ci/smoke.mjs`); see [docs/how-it-works.md](docs/how-it-works.md#development) to run it locally.
