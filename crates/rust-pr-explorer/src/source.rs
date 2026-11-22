//! Getting the two source trees of a pull request onto disk and finding the
//! crates inside them.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether the analyzed project's own code may be executed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Never run project code; strip configuration that could make tools
    /// run a program chosen by the project.
    Untrusted,
    /// The environment is sandboxed: build scripts and procedural macros may
    /// run, and the project's cargo and toolchain configuration is kept.
    Trusted,
}

/// A crate found in a source tree.
#[derive(Debug, Clone)]
pub struct CrateSrc {
    /// Crate name as used in paths (`-` replaced by `_`).
    pub name: String,
    /// Root source file (`lib.rs` or `main.rs`).
    pub root_file: PathBuf,
    /// Directory containing the crate's `Cargo.toml`.
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    pub manifest_dir: PathBuf,
}

/// Two checked out source trees plus a description of where they came from.
pub struct Checkout {
    pub base_dir: PathBuf,
    pub head_dir: PathBuf,
    pub title: String,
    pub base_label: String,
    pub head_label: String,
    pub url: Option<String>,
    // Keeps temporary directories alive for as long as the checkout is used.
    _temp: Option<tempfile::TempDir>,
}

impl Checkout {
    /// Copies two directories, so that tools run on them (`cargo metadata`
    /// writes a `Cargo.lock`) never touch the originals.
    pub fn from_dirs(base: &Path, head: &Path, trust: Trust) -> Result<Self> {
        for dir in [base, head] {
            if !dir.is_dir() {
                bail!("`{}` is not a directory", dir.display());
            }
        }
        let temp = tempfile::tempdir()?;
        let base_dir = temp.path().join("base");
        let head_dir = temp.path().join("head");
        copy_tree(base, &base_dir)?;
        copy_tree(head, &head_dir)?;
        sanitize(&base_dir, trust)?;
        sanitize(&head_dir, trust)?;
        Ok(Checkout {
            base_dir,
            head_dir,
            title: format!("{} → {}", base.display(), head.display()),
            base_label: base.display().to_string(),
            head_label: head.display().to_string(),
            url: None,
            _temp: Some(temp),
        })
    }

    /// Exports two revisions of a local git repository.
    pub fn from_git(repo: &Path, base: &str, head: &str, trust: Trust) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let base_rev = rev_parse(repo, base)?;
        let head_rev = rev_parse(repo, head)?;
        let base_dir = temp.path().join("base");
        let head_dir = temp.path().join("head");
        export_rev(repo, &base_rev, &base_dir)?;
        export_rev(repo, &head_rev, &head_dir)?;
        sanitize(&base_dir, trust)?;
        sanitize(&head_dir, trust)?;
        Ok(Checkout {
            base_dir,
            head_dir,
            title: format!("{base} → {head}"),
            base_label: format!("{base} ({})", short(&base_rev)),
            head_label: format!("{head} ({})", short(&head_rev)),
            url: None,
            _temp: Some(temp),
        })
    }

    /// Fetches a GitHub pull request. The base is the merge base of the PR
    /// head and the repository's default branch, unless `base` is given.
    pub fn from_github_pr(spec: &str, base: Option<&str>, trust: Trust) -> Result<Self> {
        let (owner, repo, number) = parse_pr_spec(spec)?;
        let url = format!("https://github.com/{owner}/{repo}.git");
        let temp = tempfile::tempdir()?;
        let git_dir = temp.path().join("repo");
        eprintln!("cloning {url} (blobs on demand)…");
        run_git(
            None,
            &[
                "clone",
                "--quiet",
                "--filter=blob:none",
                "--no-checkout",
                &url,
                git_dir.to_str().unwrap(),
            ],
        )?;
        eprintln!("fetching pull request #{number}…");
        run_git(
            Some(&git_dir),
            &[
                "fetch",
                "--quiet",
                "origin",
                &format!("pull/{number}/head:refs/pr/head"),
            ],
        )?;
        let head_rev = rev_parse(&git_dir, "refs/pr/head")?;
        let (base_rev, base_label) = match base {
            Some(b) => {
                let rev = rev_parse(&git_dir, b)
                    .or_else(|_| rev_parse(&git_dir, &format!("origin/{b}")))?;
                (rev.clone(), format!("{b} ({})", short(&rev)))
            }
            None => {
                let default = git_output(
                    &git_dir,
                    &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
                )?;
                let mut mb = git_output(&git_dir, &["merge-base", &default, &head_rev])?;
                if mb == head_rev {
                    // Already merged with a merge commit: diff against the
                    // state of the default branch before that merge.
                    let range = format!("{head_rev}..{default}");
                    let merges = git_output(
                        &git_dir,
                        &[
                            "rev-list",
                            "--ancestry-path",
                            "--merges",
                            "--reverse",
                            &range,
                        ],
                    )?;
                    if let Some(merge) = merges.lines().next() {
                        mb = git_output(
                            &git_dir,
                            &["merge-base", &format!("{merge}^1"), &head_rev],
                        )?;
                    }
                }
                (
                    mb.clone(),
                    format!("merge base with {default} ({})", short(&mb)),
                )
            }
        };
        let base_dir = temp.path().join("base");
        let head_dir = temp.path().join("head");
        for (rev, dir) in [(&base_rev, &base_dir), (&head_rev, &head_dir)] {
            run_git(
                Some(&git_dir),
                &[
                    "worktree",
                    "add",
                    "--quiet",
                    "--detach",
                    dir.to_str().unwrap(),
                    rev,
                ],
            )?;
        }
        sanitize(&base_dir, trust)?;
        sanitize(&head_dir, trust)?;
        Ok(Checkout {
            base_dir,
            head_dir,
            title: format!("{owner}/{repo}#{number}"),
            base_label,
            head_label: format!("PR head ({})", short(&head_rev)),
            url: Some(format!("https://github.com/{owner}/{repo}/pull/{number}")),
            _temp: Some(temp),
        })
    }
}

/// Accepts `https://github.com/owner/repo/pull/123` or `owner/repo#123`.
pub fn parse_pr_spec(spec: &str) -> Result<(String, String, u64)> {
    let spec = spec.trim().trim_end_matches('/');
    if let Some(rest) = spec
        .strip_prefix("https://github.com/")
        .or_else(|| spec.strip_prefix("http://github.com/"))
        .or_else(|| spec.strip_prefix("github.com/"))
    {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 4 && parts[2] == "pull" {
            let number = parts[3].parse().context("invalid PR number")?;
            return Ok((parts[0].to_string(), parts[1].to_string(), number));
        }
    } else if let Some((repo, number)) = spec.split_once('#')
        && let Some((owner, repo)) = repo.split_once('/')
    {
        let number = number.parse().context("invalid PR number")?;
        return Ok((owner.to_string(), repo.to_string(), number));
    }
    bail!("expected `https://github.com/OWNER/REPO/pull/N` or `OWNER/REPO#N`, got `{spec}`")
}

fn short(rev: &str) -> &str {
    &rev[..rev.len().min(10)]
}

fn run_git(dir: Option<&Path>, args: &[&str]) -> Result<()> {
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }
    let status = cmd.args(args).status().context("failed to run git")?;
    if !status.success() {
        bail!("`git {}` failed", args.join(" "));
    }
    Ok(())
}

fn git_output(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

fn rev_parse(repo: &Path, rev: &str) -> Result<String> {
    git_output(
        repo,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
}

fn export_rev(repo: &Path, rev: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let archive = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["archive", "--format=tar", rev])
        .output()
        .context("failed to run git archive")?;
    if !archive.status.success() {
        bail!("git archive {rev} failed");
    }
    tar::Archive::new(archive.stdout.as_slice())
        .unpack(dest)
        .with_context(|| format!("extracting {rev}"))
}

/// Removes what could make tools run code chosen by the analyzed project.
///
/// Without trust, analysis never builds the code, but `cargo metadata` and
/// rustup read configuration from the source tree: `.cargo/config.toml` can
/// name a `rustc` (wrapper) to run and `rust-toolchain.toml` a toolchain
/// directory. Those files are removed unless the project is trusted.
/// Symlinks are always removed, so that no file outside the tree is read
/// and embedded in the page. Only ever called on temporary copies.
fn sanitize(dir: &Path, trust: Trust) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            std::fs::remove_file(&path)?;
        } else if file_type.is_dir() {
            if name == ".cargo" && trust == Trust::Untrusted {
                for config in ["config", "config.toml"] {
                    let config = path.join(config);
                    if config.exists() {
                        std::fs::remove_file(&config)?;
                    }
                }
            }
            sanitize(&path, trust)?;
        } else if trust == Trust::Untrusted
            && matches!(
                name.to_str(),
                Some("rust-toolchain" | "rust-toolchain.toml")
            )
        {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Copies a source tree, skipping build output and version control data.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if matches!(name.to_str(), Some("target" | ".git" | "node_modules")) {
                continue;
            }
            copy_tree(&path, &to.join(&name))?;
        } else if file_type.is_file() {
            std::fs::copy(&path, to.join(&name))
                .with_context(|| format!("copying {}", path.display()))?;
        }
    }
    Ok(())
}

/// Finds all library (or, if there is no library, binary) crates in a tree.
pub fn find_crates(root: &Path) -> Vec<CrateSrc> {
    let mut manifests = Vec::new();
    walk_manifests(root, &mut manifests, 0);
    // Outermost manifests first: the root crate is the main subject.
    manifests.sort_by_key(|m| (m.components().count(), m.clone()));
    let mut crates = Vec::new();
    for manifest in manifests {
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let Ok(value) = text.parse::<toml::Table>() else {
            continue;
        };
        let Some(package) = value.get("package").and_then(|p| p.as_table()) else {
            continue;
        };
        let Some(name) = package.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let dir = manifest.parent().unwrap();
        let lib = value.get("lib").and_then(|l| l.as_table());
        let lib_path = lib
            .and_then(|l| l.get("path"))
            .and_then(|p| p.as_str())
            .map(|p| dir.join(p))
            .unwrap_or_else(|| dir.join("src/lib.rs"));
        let lib_name = lib
            .and_then(|l| l.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or(name);
        let root_file = if lib_path.is_file() {
            lib_path
        } else if dir.join("src/main.rs").is_file() {
            dir.join("src/main.rs")
        } else {
            continue;
        };
        crates.push(CrateSrc {
            name: lib_name.replace('-', "_"),
            root_file,
            manifest_dir: dir.to_path_buf(),
        });
    }
    crates
}

fn walk_manifests(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name.starts_with('.')
                || matches!(&*name, "target" | "node_modules" | "vendor" | "fixtures")
            {
                continue;
            }
            walk_manifests(&path, out, depth + 1);
        } else if name == "Cargo.toml" {
            out.push(path);
        }
    }
}
