//! Rust PR Explorer: shows a pull request as changed Rust items.

mod analyze;
mod merge;
mod source;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

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

const TEMPLATE: &str = include_str!("../assets/explorer.html");
const STYLE: &str = include_str!("../assets/explorer.css");
const SCRIPT: &str = include_str!("../assets/explorer.js");

fn main() -> Result<()> {
    let cli = Cli::parse();
    let checkout = match &cli.input {
        Input::Pr { pr, base } => source::Checkout::from_github_pr(pr, base.as_deref())?,
        Input::Git { repo, base, head } => source::Checkout::from_git(repo, base, head)?,
        Input::Dirs { base, head } => source::Checkout::from_dirs(base, head)?,
    };
    let output = analyze_checkout(&checkout);
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
        .filter(|i| i.status != merge::Status::Unchanged && i.data.kind != analyze::Kind::Mod)
        .count();
    eprintln!(
        "{} items analyzed, {changed} added/changed/removed → {}",
        output.items.len(),
        cli.out.display()
    );
    Ok(())
}

fn analyze_checkout(checkout: &source::Checkout) -> merge::Output {
    let base_crates = source::find_crates(&checkout.base_dir);
    let head_crates = source::find_crates(&checkout.head_dir);
    let base = analyze::Analyzer::run(&checkout.base_dir, &base_crates);
    let head = analyze::Analyzer::run(&checkout.head_dir, &head_crates);
    merge::merge(
        base,
        head,
        merge::Meta {
            title: checkout.title.clone(),
            base: checkout.base_label.clone(),
            head: checkout.head_label.clone(),
            url: checkout.url.clone(),
        },
    )
}

fn render_page(title: &str, json: &str) -> String {
    // Keep the embedded JSON from closing the script element.
    let json = json.replace("</", "<\\/");
    TEMPLATE
        .replace("{{TITLE}}", &html_escape(title))
        .replace("/*{{STYLE}}*/", STYLE)
        .replace("/*{{DATA}}*/", &json)
        .replace("/*{{SCRIPT}}*/", SCRIPT)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
