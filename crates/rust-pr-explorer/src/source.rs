//! Getting the two source trees of a pull request onto disk and finding the
//! crates inside them.

use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
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
    /// Names under which dependencies can be used in paths (`-` replaced
    /// by `_`, renames applied).
    pub deps: Vec<String>,
    /// Rust edition, which decides the prelude; `None` if unknown.
    pub edition: Option<u16>,
}

/// Two checked out source trees plus a description of where they came from.
pub struct Checkout {
    pub base_dir: PathBuf,
    pub head_dir: PathBuf,
    pub title: String,
    pub base_label: String,
    pub head_label: String,
    pub url: Option<String>,
    pub pr: Option<model::PrMeta>,
    /// Identifies the compared project without a PR (see
    /// [`model::Meta::project`]).
    pub project: Option<String>,
    /// Every file that differs, taken before anything was removed from
    /// the copies (see [`sanitize`]).
    pub files: Vec<model::ChangedFile>,
    /// What changed since an earlier commit of the PR (`pr --since`).
    pub since: Option<Since>,
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
        // From the originals: the copies have no symlinks. Without what
        // `copy_tree` leaves out, too.
        let files = crate::files::changed_files(base, head, crate::files::BUILD_DIRS);
        sanitize(&base_dir, trust)?;
        sanitize(&head_dir, trust)?;
        Ok(Checkout {
            base_dir,
            head_dir,
            title: format!("{} → {}", base.display(), head.display()),
            base_label: base.display().to_string(),
            head_label: head.display().to_string(),
            url: None,
            pr: None,
            project: Some(project_key(&[
                "dirs",
                &std::fs::canonicalize(base)?.to_string_lossy(),
                &std::fs::canonicalize(head)?.to_string_lossy(),
            ])),
            files,
            since: None,
            _temp: Some(temp),
        })
    }

    /// Checks out two revisions of a local git repository. `repo` may be a
    /// subdirectory of the repository: then only that subdirectory is
    /// analyzed and listed.
    pub fn from_git(repo: &Path, base: &str, head: &str, trust: Trust) -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let base_rev = rev_parse(repo, base)?;
        let head_rev = rev_parse(repo, head)?;
        // `git clone` needs the top-level directory of a work tree; a bare
        // repository (which has none) is cloned as it is.
        let bare = git_output(repo, &["rev-parse", "--is-bare-repository"])? == "true";
        let (top, prefix) = if bare {
            (
                repo.to_str()
                    .context("repository path is not UTF-8")?
                    .to_string(),
                String::new(),
            )
        } else {
            (
                git_output(repo, &["rev-parse", "--show-toplevel"])?,
                git_output(repo, &["rev-parse", "--show-prefix"])?,
            )
        };
        let base_tree = temp.path().join("base");
        let head_tree = temp.path().join("head");
        // A worktree of a shared clone: unlike `git archive`, it doesn't
        // apply the commit's `export-ignore` attributes, which a PR could
        // use to hide files; and the user's repository is not modified.
        let clone = temp.path().join("repo");
        run_git(
            None,
            &[
                "clone",
                "--quiet",
                "--shared",
                "--no-checkout",
                &top,
                clone.to_str().context("temporary path is not UTF-8")?,
            ],
        )?;
        for (rev, dir) in [(&base_rev, &base_tree), (&head_rev, &head_tree)] {
            run_git(
                Some(&clone),
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
        let (base_dir, head_dir) = (base_tree.join(&prefix), head_tree.join(&prefix));
        for dir in [&base_dir, &head_dir] {
            // The subdirectory may not exist in one of the revisions.
            std::fs::create_dir_all(dir)?;
        }
        let files = crate::files::changed_files(&base_dir, &head_dir, &[]);
        // The whole trees: cargo and rustup also read configuration from
        // directories above the analyzed one.
        sanitize(&base_tree, trust)?;
        sanitize(&head_tree, trust)?;
        let within = match prefix.trim_end_matches('/') {
            "" => String::new(),
            p => format!(" in {p}"),
        };
        Ok(Checkout {
            base_dir,
            head_dir,
            title: format!("{base} → {head}{within}"),
            base_label: format!("{base} ({})", short(&base_rev)),
            head_label: format!("{head} ({})", short(&head_rev)),
            url: None,
            pr: None,
            project: Some(project_key(&[
                "git",
                &std::fs::canonicalize(&top)?.to_string_lossy(),
                &prefix,
            ])),
            files,
            since: None,
            _temp: Some(temp),
        })
    }

    /// Fetches a GitHub pull request. The base is the merge base of the PR
    /// head and the repository's default branch, unless `base` is given.
    /// With `since`, an earlier commit of the PR, it also prepares the
    /// comparison of the head with that commit (see [`Since`]).
    pub fn from_github_pr(
        spec: &str,
        base: Option<&str>,
        since: Option<&str>,
        trust: Trust,
    ) -> Result<Self> {
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
        // Title and branch names are only known to GitHub's API.
        let gh = gh_pr_view(&owner, &repo, number);
        let threads = if gh.is_some() {
            gh_review_threads(&owner, &repo, number)
        } else {
            Vec::new()
        };
        let (base_rev, base_label, base_ref) = match base {
            Some(b) => {
                let rev = rev_parse(&git_dir, b)
                    .or_else(|_| rev_parse(&git_dir, &format!("origin/{b}")))?;
                (rev.clone(), format!("{b} ({})", short(&rev)), b.to_string())
            }
            None => {
                // The branch the PR targets, else the default branch.
                let default = match &gh {
                    Some(info) => format!("origin/{}", info.base_ref_name),
                    None => git_output(
                        &git_dir,
                        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
                    )?,
                };
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
                let base_ref = default.strip_prefix("origin/").unwrap_or(&default);
                (
                    mb.clone(),
                    format!("merge base with {default} ({})", short(&mb)),
                    base_ref.to_string(),
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
        let files = crate::files::changed_files(&base_dir, &head_dir, &[]);
        let since = since.map(|rev| {
            let dir = temp.path().join("since");
            let branch = format!("origin/{base_ref}");
            match since_checkout(&git_dir, &dir, rev, &branch, &base_rev, &head_rev) {
                Ok(Some((prev, label))) => Since::Changes(SinceChanges {
                    files: crate::files::changed_files(&dir, &head_dir, &[]),
                    dir,
                    commit: prev,
                    label,
                }),
                Ok(None) => Since::Nothing,
                Err(e) => Since::Unavailable(format!("{e:#}")),
            }
        });
        sanitize(&base_dir, trust)?;
        sanitize(&head_dir, trust)?;
        if let Some(Since::Changes(s)) = &since {
            sanitize(&s.dir, trust)?;
        }
        Ok(Checkout {
            base_dir,
            head_dir,
            title: format!("{owner}/{repo}#{number}"),
            base_label,
            head_label: format!("PR head ({})", short(&head_rev)),
            url: Some(format!("https://github.com/{owner}/{repo}/pull/{number}")),
            pr: Some(model::PrMeta {
                repo: format!("{owner}/{repo}"),
                number,
                title: gh.as_ref().map(|i| i.title.clone()),
                body: gh
                    .as_ref()
                    .map(|i| i.body.trim())
                    .filter(|b| !b.is_empty())
                    .map(str::to_string),
                base_ref,
                base_rev: short(&base_rev).to_string(),
                head_ref: gh.as_ref().map(|i| i.head_ref_name.clone()),
                head_owner: gh
                    .as_ref()
                    .filter(|i| i.is_cross_repository)
                    .and_then(|i| i.head_repository_owner["login"].as_str())
                    .map(str::to_string),
                head_rev: short(&head_rev).to_string(),
                head_commit: head_rev.clone(),
                custom_base: base.is_some(),
                since: None,
                threads,
            }),
            // The PR identifies the project.
            project: None,
            files,
            since,
            _temp: Some(temp),
        })
    }
}

/// What changed in a pull request since an earlier commit of it, such as
/// the one an earlier explorer page showed.
pub enum Since {
    /// The PR changed since then.
    Changes(SinceChanges),
    /// Nothing changed but the base branch: the PR only merged it or was
    /// rebased onto it.
    Nothing,
    /// The comparison could not be made, and why.
    Unavailable(String),
}

pub struct SinceChanges {
    /// What the head is compared with, checked out: the earlier commit,
    /// with the base branch's changes since then applied to it.
    pub dir: PathBuf,
    /// The earlier commit, in full.
    pub commit: String,
    /// What `dir` holds, for the page's header.
    pub label: String,
    /// Every file that differs between `dir` and the head.
    pub files: Vec<model::ChangedFile>,
}

/// Checks out what the PR's `head` is compared with to show its changes
/// since `rev`, an earlier commit of the PR, at `dir`. Returns the commit
/// in full and a label, or `None` when nothing changed but the base branch.
///
/// A plain diff from `rev` would also show every change the base branch
/// got in between, once the PR merged it or was rebased onto it. So the
/// base branch's changes since `rev`'s merge base (up to `base`, the PR's
/// merge base now) are applied to `rev` first, with `git merge-tree`, and
/// the head is compared with the result.
fn since_checkout(
    git_dir: &Path,
    dir: &Path,
    rev: &str,
    branch: &str,
    base: &str,
    head: &str,
) -> Result<Option<(String, String)>> {
    // After a force push, the earlier commit is on no branch any more, but
    // GitHub still serves it by its full id.
    let prev = match rev_parse(git_dir, rev) {
        Ok(prev) => prev,
        Err(_) => {
            eprintln!("fetching {rev}…");
            git_output(git_dir, &["fetch", "--quiet", "origin", rev])
                .with_context(|| format!("could not fetch {rev}"))?;
            rev_parse(git_dir, rev)?
        }
    };
    let tree_of = |rev: &str| git_output(git_dir, &["rev-parse", &format!("{rev}^{{tree}}")]);
    let old_base = git_output(git_dir, &["merge-base", branch, &prev])?;
    let tree = if old_base == base {
        tree_of(&prev)?
    } else {
        let merge_base = format!("--merge-base={old_base}");
        let out = Command::new("git")
            .arg("-C")
            .arg(git_dir)
            .args(["merge-tree", "--write-tree", "--no-messages", "--name-only"])
            .arg(&merge_base)
            .args([base, &prev])
            .output()
            .context("failed to run git")?;
        match out.status.code() {
            Some(0) => String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string(),
            // Conflicts. Whether the PR changed is then judged by its
            // commits: any that is no merge and not on the base branch.
            Some(1) => {
                let range = format!("{prev}..{head}");
                let not_base = format!("^{branch}");
                let own = git_output(git_dir, &["rev-list", "--no-merges", &range, &not_base])?;
                if own.is_empty() {
                    return Ok(None);
                }
                // The tree, then the conflicted files.
                let text = String::from_utf8_lossy(&out.stdout);
                let mut lines = text.lines();
                let tree = lines.next().unwrap_or_default();
                let conflicted: BTreeSet<&str> = lines.take_while(|l| !l.is_empty()).collect();
                resolve_as_merged(git_dir, tree, &conflicted, &prev, head, branch)?
            }
            _ => bail!(
                "`git merge-tree` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        }
    };
    if tree == tree_of(head)? {
        return Ok(None);
    }
    let (commit, label) = if tree == tree_of(&prev)? {
        (prev.clone(), format!("since {}", short(&prev)))
    } else {
        // A commit for the tree, so that it can be checked out.
        let message = format!(
            "{} with the base branch's changes up to {}",
            short(&prev),
            short(base)
        );
        let commit = git_output(
            git_dir,
            &[
                "-c",
                "user.name=rust-pr-explorer",
                "-c",
                "user.email=rust-pr-explorer@localhost",
                "commit-tree",
                &tree,
                "-p",
                &prev,
                "-m",
                &message,
            ],
        )?;
        (
            commit,
            format!(
                "since {}, with the base branch's changes since then",
                short(&prev)
            ),
        )
    };
    run_git(
        Some(git_dir),
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            dir.to_str().unwrap(),
            &commit,
        ],
    )?;
    Ok(Some((prev, label)))
}

/// Settles the conflicts of `tree`, the earlier commit `prev` with the
/// base branch's changes replayed on it, as the PR did: each conflicted
/// file is taken from the PR's last merge since `prev`. That version holds
/// the PR's changes up to `prev`, the base branch's and the resolution, and
/// is only right if no commit of the PR's own changed the file in between.
/// Otherwise, or without such a merge, the comparison is given up.
fn resolve_as_merged(
    git_dir: &Path,
    tree: &str,
    conflicted: &BTreeSet<&str>,
    prev: &str,
    head: &str,
    branch: &str,
) -> Result<String> {
    let range = format!("{prev}..{head}");
    let merge = git_output(
        git_dir,
        &["rev-list", "--merges", "--first-parent", "-n1", &range],
    )?;
    let conflict = || {
        anyhow::anyhow!(
            "the base branch's changes since {} conflict with the PR's changes up to it",
            short(prev)
        )
    };
    if merge.is_empty() {
        return Err(conflict());
    }
    let index_dir = tempfile::tempdir()?;
    let index = index_dir.path().join("index");
    let git = |args: &[&str]| -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(git_dir)
            .args(args)
            .env("GIT_INDEX_FILE", &index)
            .output()
            .context("failed to run git")?;
        if !out.status.success() {
            bail!(
                "`git {}` failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    git(&["read-tree", tree])?;
    let until_merge = format!("{prev}..{merge}");
    let not_base = format!("^{branch}");
    for path in conflicted {
        let own = git(&[
            "rev-list",
            "--no-merges",
            &until_merge,
            &not_base,
            "--",
            path,
        ])?;
        if !own.is_empty() {
            return Err(conflict());
        }
        // `mode type id\tpath`, or nothing if the merge has no such file.
        let entry = git(&["ls-tree", &merge, "--", path])?;
        let meta = entry
            .split_once('\t')
            .map(|(meta, _)| meta.split(' ').collect::<Vec<_>>());
        match meta {
            Some(meta) if meta.len() == 3 => {
                let info = format!("{},{},{path}", meta[0], meta[2]);
                git(&["update-index", "--add", "--cacheinfo", &info])?;
            }
            _ => {
                git(&["update-index", "--force-remove", "--", path])?;
            }
        }
    }
    git(&["write-tree"])
}

/// What `gh pr view --json` says about a pull request.
#[derive(Debug, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    title: String,
    /// The description, as Markdown; empty if there is none.
    #[serde(default)]
    body: String,
    base_ref_name: String,
    head_ref_name: String,
    /// `{"login": …}`; read loosely, since only the login is needed.
    #[serde(default)]
    head_repository_owner: serde_json::Value,
    #[serde(default)]
    is_cross_repository: bool,
}

/// Asks the GitHub CLI for the PR's title, description and branches. `None` (with a
/// note) if `gh` is missing, not logged in, or fails.
fn gh_pr_view(owner: &str, repo: &str, number: u64) -> Option<GhPr> {
    let out = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            &format!("{owner}/{repo}"),
            "--json",
            "title,body,baseRefName,headRefName,headRepositoryOwner,isCrossRepository",
        ])
        .output();
    let parsed = match out {
        Ok(o) if o.status.success() => serde_json::from_slice::<GhPr>(&o.stdout).ok(),
        _ => None,
    };
    if parsed.is_none() {
        eprintln!(
            "note: `gh pr view` failed; the PR's title, description and branch names are left out \
             and the base is the repository's default branch"
        );
    }
    parsed
}

/// The review threads of a PR, as GitHub's GraphQL API lists them.
const THREADS_QUERY: &str =
    "query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 50, after: $endCursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          path line startLine originalLine diffSide isResolved isOutdated
          comments(first: 100) { nodes { author { login } body createdAt url } }
        }
      }
    }
  }
}";

/// Asks the GitHub CLI for the PR's review threads, open and resolved.
/// Empty (with a note) if that fails.
fn gh_review_threads(owner: &str, repo: &str, number: u64) -> Vec<model::ReviewThread> {
    let out = std::process::Command::new("gh")
        .args([
            "api",
            "graphql",
            "--paginate",
            "-f",
            &format!("query={THREADS_QUERY}"),
            // `-f` keeps them strings; `-F` makes the number an integer.
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("repo={repo}"),
            "-F",
            &format!("number={number}"),
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => match parse_threads(&o.stdout) {
            Ok(list) => return list,
            Err(e) => eprintln!("note: could not read the PR's review threads: {e}"),
        },
        Ok(o) => eprintln!(
            "note: could not fetch the PR's review threads: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => eprintln!("note: could not fetch the PR's review threads: {e}"),
    }
    Vec::new()
}

/// The threads in `gh api graphql --paginate` output: one JSON document
/// per page.
fn parse_threads(json: &[u8]) -> Result<Vec<model::ReviewThread>> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Thread {
        path: String,
        line: Option<usize>,
        start_line: Option<usize>,
        original_line: Option<usize>,
        diff_side: String,
        is_resolved: bool,
        is_outdated: bool,
        comments: Nodes<Comment>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Comment {
        /// `null` for a deleted account.
        author: Option<Author>,
        body: String,
        created_at: String,
        url: String,
    }
    #[derive(serde::Deserialize)]
    struct Author {
        login: String,
    }
    #[derive(serde::Deserialize)]
    struct Nodes<T> {
        nodes: Vec<T>,
    }
    let mut out = Vec::new();
    for page in serde_json::Deserializer::from_slice(json).into_iter::<serde_json::Value>() {
        let page = page?;
        let threads = &page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"];
        let threads: Vec<Thread> =
            serde_json::from_value(threads.clone()).context("unexpected reply")?;
        for t in threads {
            out.push(model::ReviewThread {
                path: t.path,
                line: t.line,
                start_line: t.start_line,
                original_line: t.original_line,
                left: t.diff_side == "LEFT",
                resolved: t.is_resolved,
                outdated: t.is_outdated,
                url: t
                    .comments
                    .nodes
                    .first()
                    .map(|c| c.url.clone())
                    .unwrap_or_default(),
                comments: t
                    .comments
                    .nodes
                    .into_iter()
                    .map(|c| model::ThreadComment {
                        author: c.author.map(|a| a.login).unwrap_or_default(),
                        body: c.body,
                        created_at: c.created_at,
                    })
                    .collect(),
            });
        }
    }
    Ok(out)
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

/// A project key from its parts (see [`model::Meta::project`]).
fn project_key(parts: &[&str]) -> String {
    format!("{:016x}", model::stable_hash(&parts.join("\n")))
}

fn rev_parse(repo: &Path, rev: &str) -> Result<String> {
    git_output(
        repo,
        &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
    )
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

/// The `[workspace.package] edition` of the nearest workspace manifest
/// above (or at) `dir`.
fn workspace_edition(manifests: &[(PathBuf, toml::Table)], dir: &Path) -> Option<u16> {
    manifests
        .iter()
        .filter(|(path, t)| {
            t.contains_key("workspace") && path.parent().is_some_and(|p| dir.starts_with(p))
        })
        .max_by_key(|(path, _)| path.components().count())
        .and_then(|(_, t)| {
            t.get("workspace")?
                .get("package")?
                .get("edition")?
                .as_str()?
                .parse()
                .ok()
        })
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
            if name
                .to_str()
                .is_some_and(|n| n == ".git" || crate::files::BUILD_DIRS.contains(&n))
            {
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
    // Manifest paths (`[lib] path`, `[[bin]] path`, `package.build`) may
    // point anywhere: the analyzer refuses files outside the checkout, with
    // a warning on the page.
    let mut manifests = Vec::new();
    walk_manifests(root, &mut manifests, 0);
    // Outermost manifests first: the root crate is the main subject.
    manifests.sort_by_key(|m| (m.components().count(), m.clone()));
    let manifests_text: Vec<(PathBuf, toml::Table)> = manifests
        .iter()
        .filter_map(|m| {
            let table = std::fs::read_to_string(m)
                .ok()?
                .parse::<toml::Table>()
                .ok()?;
            Some((m.clone(), table))
        })
        .collect();
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
        let has_lib = lib_path.is_file();
        let mut deps = Vec::new();
        for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
            if let Some(table) = value.get(section).and_then(|d| d.as_table()) {
                deps.extend(table.keys().map(|k| k.replace('-', "_")));
            }
        }
        if let Some(targets) = value.get("target").and_then(|t| t.as_table()) {
            for target in targets.values().filter_map(|t| t.as_table()) {
                for section in ["dependencies", "dev-dependencies"] {
                    if let Some(table) = target.get(section).and_then(|d| d.as_table()) {
                        deps.extend(table.keys().map(|k| k.replace('-', "_")));
                    }
                }
            }
        }
        let edition = match package.get("edition") {
            Some(toml::Value::String(e)) => e.parse().ok(),
            // `edition.workspace = true`: inherited from the workspace.
            Some(toml::Value::Table(_)) => workspace_edition(&manifests_text, dir),
            // Cargo's default.
            None => Some(2015),
            Some(_) => None,
        };
        let lib_name = lib_name.replace('-', "_");
        if has_lib {
            crates.push(CrateSrc {
                name: lib_name.clone(),
                root_file: lib_path,
                manifest_dir: dir.to_path_buf(),
                deps: deps.clone(),
                edition,
            });
            // The other targets use the library by its name.
            deps.push(lib_name.clone());
        }
        // Binaries, the build script, examples, tests and benches: each is
        // a crate of its own, named `kind:name` (a name no path can use).
        let mut seen: BTreeSet<PathBuf> = crates.iter().map(|c| c.root_file.clone()).collect();
        for (kind, name, file) in targets(&value, dir, package, name) {
            if !file.is_file() || !seen.insert(file.clone()) {
                continue;
            }
            let mut id = format!("{kind}:{}", name.replace('-', "_"));
            if crates.iter().any(|c| c.name == id) {
                id = format!("{kind}:{}:{}", lib_name, name.replace('-', "_"));
            }
            crates.push(CrateSrc {
                name: id,
                root_file: file,
                manifest_dir: dir.to_path_buf(),
                deps: deps.clone(),
                edition,
            });
        }
    }
    crates
}

/// A package's targets other than the library: those declared in the
/// manifest (`[[bin]]`, `[[test]]`, …, `package.build`) and those Cargo
/// finds by convention (`src/main.rs`, `src/bin/`, `build.rs`,
/// `examples/`, `tests/`, `benches/`).
fn targets(
    manifest: &toml::Table,
    dir: &Path,
    package: &toml::Table,
    package_name: &str,
) -> Vec<(&'static str, String, PathBuf)> {
    let mut out = Vec::new();
    for (key, kind) in [
        ("bin", "bin"),
        ("example", "example"),
        ("test", "test"),
        ("bench", "bench"),
    ] {
        for t in manifest
            .get(key)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_table())
        {
            let Some(name) = t.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let path = match t.get("path").and_then(|p| p.as_str()) {
                Some(p) => dir.join(p),
                None => match kind {
                    "bin" => dir.join(format!("src/bin/{name}.rs")),
                    "example" => dir.join(format!("examples/{name}.rs")),
                    "test" => dir.join(format!("tests/{name}.rs")),
                    _ => dir.join(format!("benches/{name}.rs")),
                },
            };
            out.push((kind, name.to_string(), path));
        }
    }
    match package.get("build") {
        Some(toml::Value::String(p)) => out.push(("build", package_name.to_string(), dir.join(p))),
        Some(toml::Value::Boolean(false)) => {}
        _ => out.push(("build", package_name.to_string(), dir.join("build.rs"))),
    }
    out.push(("bin", package_name.to_string(), dir.join("src/main.rs")));
    for (sub, kind) in [
        ("src/bin", "bin"),
        ("examples", "example"),
        ("tests", "test"),
        ("benches", "bench"),
    ] {
        let Ok(entries) = std::fs::read_dir(dir.join(sub)) else {
            continue;
        };
        let mut found: Vec<(String, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let path = e.path();
                let stem = path.file_stem()?.to_string_lossy().to_string();
                if path.is_file() && path.extension().is_some_and(|x| x == "rs") {
                    Some((stem, path))
                } else if path.join("main.rs").is_file() {
                    Some((
                        e.file_name().to_string_lossy().to_string(),
                        path.join("main.rs"),
                    ))
                } else {
                    None
                }
            })
            .collect();
        found.sort();
        out.extend(found.into_iter().map(|(n, p)| (kind, n, p)));
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `since_checkout` after merges, rebases and new commits of a PR.
    #[test]
    fn changes_since_an_earlier_commit_leave_out_the_base_branch() {
        let repo = tempfile::tempdir().unwrap();
        let dirs = tempfile::tempdir().unwrap();
        let r = repo.path();
        let git = |args: &[&str]| {
            git_output(
                r,
                &[&["-c", "user.name=t", "-c", "user.email=t@t"], args].concat(),
            )
            .unwrap()
        };
        let write = |name: &str, text: &str| std::fs::write(r.join(name), text).unwrap();
        let commit = |msg: &str| {
            git(&["add", "."]);
            git(&["commit", "-q", "-m", msg]);
            git(&["rev-parse", "HEAD"])
        };
        let n = std::cell::Cell::new(0);
        // Compares the PR's current head with `prev`, as `pr --since` does.
        let since = |prev: &str| {
            let head = git(&["rev-parse", "HEAD"]);
            let base = git(&["merge-base", "main", &head]);
            n.set(n.get() + 1);
            let dir = dirs.path().join(n.get().to_string());
            since_checkout(r, &dir, prev, "main", &base, &head)
                .unwrap()
                .map(|(commit, label)| {
                    assert_eq!(commit, prev);
                    let files = crate::files::changed_files(&dir, &r.join("."), &[".git"]);
                    let paths: Vec<String> = files.into_iter().map(|f| f.path).collect();
                    (paths, label)
                })
        };
        git(&["init", "-q", "-b", "main"]);
        write("a.txt", "a\n");
        write("b.txt", "b\n");
        commit("base");
        git(&["checkout", "-q", "-b", "pr"]);
        write("a.txt", "a, changed by the PR\n");
        let first = commit("pr 1");

        // Only the base branch changed, merged into the PR.
        git(&["checkout", "-q", "main"]);
        write("b.txt", "b, changed on main\n");
        commit("main 1");
        git(&["checkout", "-q", "pr"]);
        git(&["merge", "-q", "--no-edit", "main"]);
        assert_eq!(since(&first), None);

        // A new commit after the merge: only its file differs, not main's.
        write("c.txt", "c\n");
        let second = commit("pr 2");
        let (paths, label) = since(&first).unwrap();
        assert_eq!(paths, ["c.txt"]);
        assert!(label.contains("with the base branch's changes"), "{label}");
        // From the merged commit no change needs replaying.
        write("c.txt", "c, again\n");
        commit("pr 3");
        let (paths, label) = since(&second).unwrap();
        assert_eq!(paths, ["c.txt"]);
        assert_eq!(label, format!("since {}", short(&second)));

        // Rebased onto a newer main without other changes.
        let before = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-q", "main"]);
        write("d.txt", "d\n");
        commit("main 2");
        git(&["checkout", "-q", "pr"]);
        git(&["rebase", "-q", "main"]);
        assert_eq!(since(&before), None);

        // Main changes the PR's line; the PR merges it and resolves the
        // conflict. Replaying main conflicts, and the PR has no commit of
        // its own since: nothing changed.
        let before = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-q", "main"]);
        write("a.txt", "a, changed on main\n");
        commit("main 3");
        git(&["checkout", "-q", "pr"]);
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(r)
                .args(["merge", "-q", "main"])
                .output()
                .unwrap()
                .status
                .code()
                == Some(1)
        );
        write("a.txt", "a, changed by both\n");
        commit("merge main");
        assert_eq!(since(&before), None);
        // With a commit of its own after that, the conflicted file is
        // compared from the merge, so only the new commit's file differs.
        write("e.txt", "e\n");
        commit("pr 4");
        let (paths, _) = since(&before).unwrap();
        assert_eq!(paths, ["e.txt"]);
        // A commit of the PR's own changed the conflicted file before the
        // merge: the comparison is given up.
        let before = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-q", "main"]);
        write("a.txt", "a, changed on main again\n");
        commit("main 4");
        git(&["checkout", "-q", "pr"]);
        write("a.txt", "a, changed by both, then by the PR\n");
        commit("pr 5");
        Command::new("git")
            .arg("-C")
            .arg(r)
            .args(["merge", "-q", "main"])
            .output()
            .unwrap();
        write("a.txt", "a, changed by all\n");
        commit("merge main again");
        write("f.txt", "f\n");
        commit("pr 6");
        let head = git(&["rev-parse", "HEAD"]);
        let base = git(&["merge-base", "main", &head]);
        let err =
            since_checkout(r, &dirs.path().join("x"), &before, "main", &base, &head).unwrap_err();
        assert!(err.to_string().contains("conflict"), "{err}");
    }

    #[test]
    fn gh_json_is_parsed() {
        // As printed by `gh pr view --json title,body,baseRefName,headRefName,headRepositoryOwner,isCrossRepository`.
        let json = r#"{"baseRefName":"master","body":"Fixes #12.\r\n\r\n- one","headRefName":"value-bag","headRepositoryOwner":{"id":"MDQ6VXNlcjE=","login":"someone","name":"Some One"},"isCrossRepository":true,"title":"Use value-bag"}"#;
        let pr: GhPr = serde_json::from_str(json).unwrap();
        assert_eq!(pr.body, "Fixes #12.\r\n\r\n- one");
        assert_eq!(pr.base_ref_name, "master");
        assert_eq!(pr.head_ref_name, "value-bag");
        assert!(pr.is_cross_repository);
        assert_eq!(pr.head_repository_owner["login"], "someone");
    }

    #[test]
    fn review_threads_are_read_from_every_page() {
        let page = |thread: &str| {
            format!(
                r#"{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{thread}]}}}}}}}}}}"#
            )
        };
        let json = page(
            r#"{"path":"src/lib.rs","line":12,"startLine":null,"originalLine":10,"diffSide":"RIGHT","isResolved":false,"isOutdated":false,"comments":{"nodes":[{"author":{"login":"a"},"body":"Why?","createdAt":"2026-11-23T10:00:00Z","url":"https://github.com/o/r/pull/1#discussion_r1"},{"author":null,"body":"Because.","createdAt":"2026-11-23T11:00:00Z","url":"https://github.com/o/r/pull/1#discussion_r2"}]}}"#,
        ) + &page(
            r#"{"path":"README.md","line":null,"startLine":null,"originalLine":3,"diffSide":"LEFT","isResolved":true,"isOutdated":true,"comments":{"nodes":[]}}"#,
        );
        let threads = parse_threads(json.as_bytes()).unwrap();
        assert_eq!(threads.len(), 2);
        let t = &threads[0];
        assert_eq!(
            (t.path.as_str(), t.line, t.left),
            ("src/lib.rs", Some(12), false)
        );
        assert_eq!(t.url, "https://github.com/o/r/pull/1#discussion_r1");
        assert_eq!(t.comments[1].author, "");
        assert_eq!(t.comments[1].body, "Because.");
        let t = &threads[1];
        assert!(t.left && t.resolved && t.outdated);
        assert_eq!((t.line, t.original_line), (None, Some(3)));
    }
}
