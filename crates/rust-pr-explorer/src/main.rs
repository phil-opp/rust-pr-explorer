//! Rust PR Explorer: shows a pull request as changed Rust items.

mod analyze;
mod merge;
#[cfg(feature = "semantic")]
mod semantic;
mod sigdiff;
mod source;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    version,
    about = "Explore a Rust pull request as changed items instead of changed lines"
)]
struct Cli {
    #[command(subcommand)]
    input: Input,
    /// Where to write the explorer page.
    #[arg(short, long, global = true, default_value = "pr-explorer.html")]
    out: PathBuf,
    /// Also write the analysis as JSON.
    #[arg(long, global = true)]
    json: Option<PathBuf>,
    /// Skip rust-analyzer and resolve method calls by name only.
    #[arg(long, global = true)]
    syntax_only: bool,
    /// Allow running the analyzed project's build scripts and procedural
    /// macros, and keep its `.cargo/config.toml` and `rust-toolchain.toml`.
    /// Only use this in a sandboxed environment: the PR's code gets executed.
    #[arg(long, global = true, env = "RUST_PR_EXPLORER_TRUST_PROJECT_CODE")]
    trust_project_code: bool,
}

#[derive(Subcommand)]
enum Input {
    /// A GitHub pull request: `https://github.com/OWNER/REPO/pull/N` or `OWNER/REPO#N`.
    Pr {
        pr: String,
        /// Base revision (defaults to the merge base with the default branch).
        #[arg(long)]
        base: Option<String>,
    },
    /// Two revisions of a local git repository.
    Git {
        repo: PathBuf,
        base: String,
        head: String,
    },
    /// Two directories with the old and new source tree.
    Dirs { base: PathBuf, head: PathBuf },
}

const TEMPLATE: &str = include_str!("../../viewer/assets/explorer.html");
const STYLE: &str = include_str!("../../viewer/assets/explorer.css");
// Built by build.rs from the `viewer` crate.
const GLUE: &str = include_str!(concat!(env!("OUT_DIR"), "/viewer.js"));
const WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/viewer_bg.wasm"));

fn main() -> Result<()> {
    let cli = Cli::parse();
    let trust = if cli.trust_project_code {
        eprintln!("trusting project code: build scripts and proc macros may run");
        source::Trust::Trusted
    } else {
        source::Trust::Untrusted
    };
    let checkout = match &cli.input {
        Input::Pr { pr, base } => source::Checkout::from_github_pr(pr, base.as_deref(), trust)?,
        Input::Git { repo, base, head } => source::Checkout::from_git(repo, base, head, trust)?,
        Input::Dirs { base, head } => source::Checkout::from_dirs(base, head, trust)?,
    };
    let output = analyze_checkout(&checkout, !cli.syntax_only, trust);
    for w in &output.warnings {
        eprintln!("warning: {w}");
    }
    let json = serde_json::to_string(&output)?;
    if let Some(path) = &cli.json {
        std::fs::write(path, serde_json::to_string_pretty(&output)?)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    std::fs::write(&cli.out, render_page(&output.meta.title, &json))
        .with_context(|| format!("writing {}", cli.out.display()))?;
    let changed = output
        .items
        .values()
        .filter(|i| i.status != model::Status::Unchanged && i.info.kind != model::Kind::Mod)
        .count();
    eprintln!(
        "{} items analyzed, {changed} added/changed/removed → {}",
        output.items.len(),
        cli.out.display()
    );
    Ok(())
}

fn analyze_checkout(
    checkout: &source::Checkout,
    semantic: bool,
    trust: source::Trust,
) -> model::Output {
    let (base, base_note) = analyze_side(&checkout.base_dir, semantic, trust, "base");
    let (head, head_note) = analyze_side(&checkout.head_dir, semantic, trust, "head");
    let resolution = if base_note == head_note {
        head_note
    } else {
        format!("base: {base_note}; head: {head_note}")
    };
    merge::merge(
        base,
        head,
        model::Meta {
            title: checkout.title.clone(),
            base: checkout.base_label.clone(),
            head: checkout.head_label.clone(),
            url: checkout.url.clone(),
            resolution,
        },
    )
}

/// Analyzes one source tree. Returns the analysis and a note on how calls
/// were resolved.
fn analyze_side(
    root: &Path,
    semantic: bool,
    trust: source::Trust,
    label: &str,
) -> (analyze::Analysis, String) {
    let crates = source::find_crates(root);
    #[allow(unused_mut)]
    let mut analysis = analyze::Analyzer::run(root, &crates);
    if !semantic {
        return (analysis, "syntax only".into());
    }
    #[cfg(feature = "semantic")]
    {
        eprintln!("{label}: resolving calls with rust-analyzer…");
        let mut dirs: Vec<PathBuf> = if root.join("Cargo.toml").is_file() {
            vec![root.to_path_buf()]
        } else {
            crates.iter().map(|c| c.manifest_dir.clone()).collect()
        };
        dirs.dedup();
        match semantic::refine(root, &dirs, trust, &mut analysis) {
            Ok(stats) => {
                eprintln!(
                    "{label}: {} functions, {} call sites resolved, {} unresolved, {} external items described",
                    stats.functions, stats.resolved_sites, stats.unresolved_sites, stats.externals
                );
                let note = match trust {
                    source::Trust::Untrusted => "rust-analyzer",
                    source::Trust::Trusted => "rust-analyzer, with build scripts and proc macros",
                };
                (analysis, note.into())
            }
            Err(e) => {
                let note = format!("syntax only (rust-analyzer failed: {e:#})");
                eprintln!("{label}: {note}");
                analysis.warnings.push(note.clone());
                (analysis, note)
            }
        }
    }
    #[cfg(not(feature = "semantic"))]
    {
        let _ = (label, trust);
        (analysis, "syntax only (built without rust-analyzer)".into())
    }
}

fn render_page(title: &str, json: &str) -> String {
    use base64::Engine;
    // Keep the embedded JSON from closing the script element.
    let json = json.replace("</", "<\\/");
    let wasm = base64::engine::general_purpose::STANDARD.encode(WASM);
    TEMPLATE
        .replace("{{TITLE}}", &html_escape(title))
        .replace("/*{{STYLE}}*/", STYLE)
        .replace("/*{{GLUE}}*/", GLUE)
        .replace("{{WASM}}", &wasm)
        // Last, so that nothing in the analyzed code is taken for a placeholder.
        .replace("/*{{DATA}}*/", &json)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
