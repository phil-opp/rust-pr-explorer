//! Rust PR Explorer: shows a pull request as changed Rust items.

mod analyze;
mod commits;
mod css;
mod files;
mod javascript;
mod lockfile;
mod merge;
mod moved;
mod parts;
#[cfg(feature = "semantic")]
mod public_api;
mod python;
mod reformat;
#[cfg(feature = "semantic")]
mod semantic;
mod semver;
mod semver_checks;
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
    /// Also run cargo-semver-checks on the two versions and add what it
    /// finds to the SemVer labels. It builds both versions' rustdoc JSON,
    /// which runs build scripts and procedural macros: like
    /// `--trust-project-code`, only use this in a sandboxed environment.
    /// rust-analyzer stays in its mode (much faster without
    /// `--trust-project-code`).
    #[arg(long, global = true)]
    semver_checks: bool,
    /// With `--trust-project-code`: the cargo target directory for the
    /// build scripts and proc macros, shared by both versions (and the
    /// "since" one). Keep it between runs (e.g. in a CI cache) to only
    /// build the dependencies once. Defaults to a temporary directory.
    #[arg(
        long,
        global = true,
        env = "RUST_PR_EXPLORER_TARGET_DIR",
        requires = "trust_project_code"
    )]
    target_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Input {
    /// A GitHub pull request: `https://github.com/OWNER/REPO/pull/N` or `OWNER/REPO#N`.
    Pr {
        pr: String,
        /// Base revision (defaults to the merge base with the default branch).
        #[arg(long)]
        base: Option<String>,
        /// An earlier commit of the PR (in full): also write a page with
        /// only what changed since then, leaving out what the base branch
        /// changed in between. It goes next to `--out` (and `--json`), with
        /// `-since` added to the name, if the PR changed since then. Prints
        /// `since: changes`, `since: none` or `since: unavailable (reason)`
        /// on stdout.
        #[arg(long, conflicts_with = "base")]
        since: Option<String>,
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
const IOSEVKA_400: &[u8] = include_bytes!("../../viewer/assets/fonts/iosevka-400.woff2");
const IOSEVKA_700: &[u8] = include_bytes!("../../viewer/assets/fonts/iosevka-700.woff2");
const STYLE: &str = include_str!("../../viewer/assets/explorer.css");
// Built by build.rs from the `viewer` crate.
const GLUE: &str = include_str!(concat!(env!("OUT_DIR"), "/viewer.js"));
// Gzipped, like the data (see `render_page`).
const WASM_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/viewer_bg.wasm.gz"));

fn main() -> Result<()> {
    let cli = Cli::parse();
    let trust = if cli.trust_project_code {
        eprintln!("trusting project code: build scripts and proc macros may run");
        source::Trust::Trusted
    } else {
        source::Trust::Untrusted
    };
    let checkout = match &cli.input {
        Input::Pr { pr, base, since } => {
            source::Checkout::from_github_pr(pr, base.as_deref(), since.as_deref(), trust)?
        }
        Input::Git { repo, base, head } => source::Checkout::from_git(repo, base, head, trust)?,
        Input::Dirs { base, head } => source::Checkout::from_dirs(base, head, trust)?,
    };
    // Build scripts and proc macros are built once, for all versions: the
    // registry dependencies are the same in each (only the workspace's own
    // crates are rebuilt, because their paths differ).
    let temp_target;
    let given_target;
    let target_dir = match (trust, &cli.target_dir) {
        (source::Trust::Untrusted, _) => None,
        (source::Trust::Trusted, Some(dir)) => {
            // cargo runs in each checkout: a relative path would point into
            // it.
            given_target = std::path::absolute(dir)?;
            Some(given_target.as_path())
        }
        (source::Trust::Trusted, None) => {
            temp_target = tempfile::tempdir()?;
            Some(temp_target.path())
        }
    };
    let how = Settings {
        semantic: !cli.syntax_only,
        trust,
        target_dir,
    };
    let (mut output, since) = analyze_all(&checkout, &how);
    if cli.semver_checks {
        eprintln!("running cargo-semver-checks: build scripts and proc macros may run");
        // Base and head: the page of the changes since an earlier commit
        // keeps the definitions' labels.
        semver_checks::run(&checkout.base_dir, &checkout.head_dir, &mut output);
        if let Some(r) = &output.semver_checks {
            match &r.error {
                Some(e) => eprintln!("warning: {e}"),
                None => eprintln!(
                    "cargo-semver-checks {}: {} findings in {:.0} s",
                    r.version,
                    r.findings.len(),
                    r.seconds
                ),
            }
        }
    }
    write_outputs(&output, &cli.out, cli.json.as_deref())?;
    match (&checkout.since, since) {
        (Some(source::Since::Changes(s)), Some(since)) => {
            eprintln!("changes since {}:", s.commit);
            write_outputs(
                &since,
                &with_suffix(&cli.out, "-since"),
                cli.json
                    .as_deref()
                    .map(|j| with_suffix(j, "-since"))
                    .as_deref(),
            )?;
            println!("since: changes");
        }
        (Some(source::Since::Nothing), _) => {
            eprintln!("nothing changed since the given commit but the base branch");
            println!("since: none");
        }
        (Some(source::Since::Unavailable(why)), _) => {
            eprintln!("warning: no page of the changes since the given commit: {why}");
            // One line, for scripts.
            let why = why.split_whitespace().collect::<Vec<_>>().join(" ");
            println!("since: unavailable ({why})");
        }
        _ => {}
    }
    Ok(())
}

/// Writes the page, and the analysis as JSON if asked to.
fn write_outputs(output: &model::Output, out: &Path, json: Option<&Path>) -> Result<()> {
    for w in &output.warnings {
        eprintln!("warning: {w}");
    }
    if let Some(path) = json {
        std::fs::write(path, serde_json::to_string_pretty(output)?)
            .with_context(|| format!("writing {}", path.display()))?;
    }
    let data = serde_json::to_string(output)?;
    std::fs::write(out, render_page(&output.meta.title, &data))
        .with_context(|| format!("writing {}", out.display()))?;
    let changed = output
        .items
        .values()
        .filter(|i| i.status != model::Status::Unchanged && i.info.kind != model::Kind::Mod)
        .count();
    eprintln!(
        "{} items analyzed, {changed} added/changed/removed → {}",
        output.items.len(),
        out.display()
    );
    Ok(())
}

/// `dir/name.ext` → `dir/name{suffix}.ext`.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem}{suffix}.{}", ext.to_string_lossy()),
        None => format!("{stem}{suffix}"),
    };
    path.with_file_name(name)
}

/// How to analyze each version.
struct Settings<'a> {
    /// Resolve calls with rust-analyzer.
    semantic: bool,
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    trust: source::Trust,
    /// The cargo target directory for build scripts and proc macros (only
    /// when trusted). `None` uses the project's own.
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    target_dir: Option<&'a Path>,
}

#[cfg(test)]
fn analyze_checkout(
    checkout: &source::Checkout,
    semantic: bool,
    trust: source::Trust,
) -> model::Output {
    let how = Settings {
        semantic,
        trust,
        target_dir: None,
    };
    analyze_all(checkout, &how).0
}

/// The analysis of the checkout, and of the changes since an earlier
/// commit if it has them (see [`source::Since`]). The head is analyzed
/// once for both.
fn analyze_all(
    checkout: &source::Checkout,
    how: &Settings,
) -> (model::Output, Option<model::Output>) {
    let base = analyze_side(&checkout.base_dir, how, "base");
    let head = analyze_side(&checkout.head_dir, how, "head");
    let meta = |label: &str, pr: Option<model::PrMeta>| model::Meta {
        title: checkout.title.clone(),
        base: label.to_string(),
        head: checkout.head_label.clone(),
        url: checkout.url.clone(),
        resolution: String::new(),
        pr,
        project: checkout.project.clone(),
        commits: Vec::new(),
        versions: Default::default(),
    };
    let since = match &checkout.since {
        Some(source::Since::Changes(s)) => {
            let since_base = analyze_side(&s.dir, how, "since");
            let pr = checkout.pr.clone().map(|pr| model::PrMeta {
                base_rev: s.commit[..s.commit.len().min(10)].to_string(),
                custom_base: true,
                since: Some(s.commit.clone()),
                ..pr
            });
            Some(compare(
                (&s.dir, since_base),
                (&checkout.head_dir, head.clone()),
                s.files.clone(),
                meta(&s.label, pr),
                // Commits are traced from the PR's base, which this page
                // doesn't compare with.
                None,
            ))
        }
        _ => None,
    };
    let output = compare(
        (&checkout.base_dir, base),
        (&checkout.head_dir, head),
        // Taken by the checkout before anything was removed from the copies.
        checkout.files.clone(),
        meta(&checkout.base_label, checkout.pr.clone()),
        checkout.git.as_ref(),
    );
    (output, since)
}

/// Compares two analyzed trees: the base and the head, each with the note
/// of [`analyze_side`].
fn compare(
    (base_dir, (base, base_note)): (&Path, (analyze::Analysis, String)),
    (head_dir, (head, head_note)): (&Path, (analyze::Analysis, String)),
    mut files: Vec<model::ChangedFile>,
    meta: model::Meta,
    git: Option<&source::GitRange>,
) -> model::Output {
    let resolution = if base_note == head_note {
        head_note
    } else {
        format!("base: {base_note}; head: {head_note}")
    };
    let versions = source::crate_versions(base_dir, head_dir);
    let mut output = merge::merge(
        base,
        head,
        model::Meta {
            resolution,
            versions,
            ..meta
        },
    );
    // Files the Rust analysis covers: one per module.
    let analyzed: std::collections::BTreeSet<&str> = output
        .items
        .values()
        .filter(|i| i.info.kind == model::Kind::Mod)
        .map(|i| i.info.file.as_str())
        .collect();
    for f in &mut files {
        f.analyzed = analyzed.contains(f.path.as_str());
        if f.analyzed {
            let text = |dir: &Path| std::fs::read_to_string(dir.join(&f.path)).unwrap_or_default();
            let (old, new) = (text(base_dir), text(head_dir));
            (f.added_lines, f.removed_lines) = files::changed_lines(&f.hunks);
            // The items show the other changes: keep only these.
            (f.outside_items, f.hunks, f.parts) =
                files::outside_items(f, output.items.values(), (&old, &new));
        }
    }
    output.files = files;
    if let Some(git) = git {
        eprintln!("tracing which commit changed what…");
        let mut warnings = Vec::new();
        output.meta.commits = commits::commits(git, &output.items, &output.files, &mut warnings);
        output.warnings.extend(warnings);
    }
    output
}

/// Analyzes one source tree. Returns the analysis and a note on how calls
/// were resolved.
fn analyze_side(root: &Path, how: &Settings, label: &str) -> (analyze::Analysis, String) {
    let crates = source::find_crates(root);
    #[allow(unused_mut)]
    let mut analysis = analyze::Analyzer::run(root, &crates);
    if !how.semantic {
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
        match semantic::refine(root, &dirs, how.trust, how.target_dir, &mut analysis) {
            Ok(stats) => {
                eprintln!(
                    "{label}: {} functions, {} call sites resolved, {} unresolved, {} external items described",
                    stats.functions, stats.resolved_sites, stats.unresolved_sites, stats.externals
                );
                let mut note = match how.trust {
                    source::Trust::Untrusted => "rust-analyzer".to_string(),
                    source::Trust::Trusted => {
                        "rust-analyzer, with build scripts and proc macros".to_string()
                    }
                };
                if stats.functions == 0 && !stats.failed.is_empty() {
                    note = "syntax only (rust-analyzer could not load the workspace)".into();
                } else if !stats.failed.is_empty() {
                    // Items of the other workspaces keep the syntactic
                    // results.
                    note.push_str(&format!(
                        "; syntax only in {} workspace{} it could not load",
                        stats.failed.len(),
                        if stats.failed.len() == 1 { "" } else { "s" }
                    ));
                }
                for f in &stats.failed {
                    let w = format!("rust-analyzer could not load {f}");
                    eprintln!("{label}: {w}");
                    analysis.warnings.push(w);
                }
                (analysis, note)
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
        let _ = (label, how);
        (analysis, "syntax only (built without rust-analyzer)".into())
    }
}

fn render_page(title: &str, json: &str) -> String {
    use base64::Engine;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    // The wasm and the data are embedded gzipped and base64 encoded, and
    // unpacked by the browser: a large PR's page is about a fourth of its
    // plain size. Base64 also has no `<`, so the data can't end or change
    // the script element it is in.
    let data = b64(&gzip(json.as_bytes()));
    let wasm = b64(WASM_GZ);
    // The default code font, embedded so that the page works offline.
    let fonts: String = [(400, IOSEVKA_400), (700, IOSEVKA_700)]
        .iter()
        .map(|(weight, bytes)| {
            format!(
                "@font-face {{ font-family: \"Iosevka\"; font-weight: {weight}; font-display: block; \
                 src: url(data:font/woff2;base64,{}) format(\"woff2\"); }}\n",
                b64(bytes)
            )
        })
        .collect();
    // Base64 has no braces, so the data can't be taken for a placeholder.
    TEMPLATE
        .replace("{{DATA}}", &data)
        .replace("{{WASM}}", &wasm)
        .replace("/*{{FONTS}}*/", &fonts)
        .replace("/*{{STYLE}}*/", STYLE)
        .replace("/*{{GLUE}}*/", GLUE)
        // Last, so that nothing in the PR's title is taken for a placeholder.
        .replace("{{TITLE}}", &html_escape(title))
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(bytes).expect("gzip into memory");
    gz.finish().expect("gzip into memory")
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
