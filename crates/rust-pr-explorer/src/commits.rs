//! Which changes each commit of the compared range made, for the viewer's
//! commit filter.
//!
//! The analysis compares only the two ends of the range, so a commit's
//! share of it comes from `git blame`: an item's added lines are blamed in
//! the PR's version of its file (the commit that last wrote each line),
//! and its removed lines in the base's with `--reverse` (the last commit
//! that still had the line; the next one removed it).

use crate::source::{GitRange, git_output};
use anyhow::{Context, Result, bail};
use model::{Commit, Item, Kind, Status};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::process::Command;

/// More commits than this are listed without what they touched: every
/// one needs blame runs, which fetch old file versions on demand.
const MAX_COMMITS: usize = 250;

/// The commits of the range, oldest first, with the items and files each
/// one touched. Problems become warnings; the page then has no filter.
pub fn commits(
    git: &GitRange,
    items: &BTreeMap<String, Item>,
    files: &[model::ChangedFile],
    warnings: &mut Vec<String>,
) -> Vec<Commit> {
    match collect(git, items, files) {
        Ok(list) => list,
        Err(e) => {
            let w = format!("could not tell which commit changed what: {e:#}");
            eprintln!("warning: {w}");
            warnings.push(w);
            Vec::new()
        }
    }
}

fn collect(
    git: &GitRange,
    items: &BTreeMap<String, Item>,
    files: &[model::ChangedFile],
) -> Result<Vec<Commit>> {
    let range = format!("{}..{}", git.base, git.head);
    let log = git_output(
        &git.dir,
        &[
            "log",
            "--reverse",
            "--topo-order",
            "--format=%H%x1f%P%x1f%an%x1f%aI%x1f%s",
            &range,
        ],
    )?;
    let mut list: Vec<Commit> = Vec::new();
    let mut parents: HashMap<String, Vec<String>> = HashMap::new();
    for line in log.lines().filter(|l| !l.is_empty()) {
        let f: Vec<&str> = line.split('\x1f').collect();
        let [sha, ps, author, date, title] = f[..] else {
            bail!("unexpected `git log` line: {line}");
        };
        let ps: Vec<String> = ps.split(' ').map(str::to_string).collect();
        list.push(Commit {
            sha: sha.to_string(),
            title: title.to_string(),
            author: author.to_string(),
            date: date.to_string(),
            merge: ps.len() > 1,
            ..Default::default()
        });
        parents.insert(sha.to_string(), ps);
    }
    if list.is_empty() {
        return Ok(list);
    }
    // The files each commit changed, relative to the analyzed directory.
    let changed: BTreeSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    let mut touched_files: HashMap<String, BTreeSet<String>> = HashMap::new();
    for c in list.iter_mut().filter(|c| !c.merge) {
        let names = git_output(
            &git.dir,
            &[
                "diff-tree",
                "--no-commit-id",
                "-r",
                "--name-only",
                "--no-renames",
                &c.sha,
            ],
        )?;
        let set: BTreeSet<String> = names
            .lines()
            .filter_map(|p| p.strip_prefix(git.prefix.as_str()))
            .map(str::to_string)
            .collect();
        c.files = set
            .iter()
            .filter(|p| changed.contains(p.as_str()))
            .cloned()
            .collect();
        touched_files.insert(c.sha.clone(), set);
    }
    if list.len() > MAX_COMMITS {
        bail!(
            "{} commits, more than the {MAX_COMMITS} that are traced line by line",
            list.len()
        );
    }
    // The commit that removed a line, from the last one that had it: its
    // child in the range that changed the file.
    let remover = |last: &str, path: &str| -> Option<String> {
        list.iter()
            .find(|c| {
                parents[&c.sha].iter().any(|p| p == last)
                    && touched_files.get(&c.sha).is_some_and(|f| f.contains(path))
            })
            .map(|c| c.sha.clone())
    };
    // Changed lines of the changed items, by file: (line, item).
    let mut added: BTreeMap<&str, Vec<(usize, &str)>> = BTreeMap::new();
    let mut removed: BTreeMap<&str, Vec<(usize, &str)>> = BTreeMap::new();
    for it in items.values() {
        if it.info.kind == Kind::Mod || it.status == Status::Unchanged {
            continue;
        }
        let (mut o, mut n) = match it.status {
            Status::Removed => (it.info.line, it.info.line),
            _ => (it.old_line.unwrap_or(it.info.line), it.info.line),
        };
        let old_file = it.old_file.as_deref().unwrap_or(&it.info.file);
        for l in &it.source {
            match l.0 {
                '+' => {
                    added
                        .entry(&it.info.file)
                        .or_default()
                        .push((n, &it.info.id));
                    n += 1;
                }
                '-' => {
                    removed.entry(old_file).or_default().push((o, &it.info.id));
                    o += 1;
                }
                _ => {
                    o += 1;
                    n += 1;
                }
            }
        }
    }
    let mut by_commit: HashMap<String, BTreeSet<&str>> = HashMap::new();
    for (path, lines) in &added {
        let blame = blame(git, path, false)?;
        for &(line, id) in lines {
            if let Some(sha) = blame.get(&line).filter(|s| parents.contains_key(*s)) {
                by_commit.entry(sha.clone()).or_default().insert(id);
            }
        }
    }
    for (path, lines) in &removed {
        let blame = blame(git, path, true)?;
        for &(line, id) in lines {
            if let Some(sha) = blame.get(&line).and_then(|last| remover(last, path)) {
                by_commit.entry(sha).or_default().insert(id);
            }
        }
    }
    for c in &mut list {
        if let Some(ids) = by_commit.get(&c.sha) {
            c.items = ids.iter().map(|s| s.to_string()).collect();
        }
    }
    Ok(list)
}

/// The commit of each line of `path`: in the head version, the one that
/// last wrote it; with `reverse`, in the base version, the last one that
/// still had it.
fn blame(git: &GitRange, path: &str, reverse: bool) -> Result<HashMap<usize, String>> {
    let range = format!("{}..{}", git.base, git.head);
    let full = format!("{}{path}", git.prefix);
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&git.dir).args(["blame", "--porcelain"]);
    if reverse {
        cmd.arg("--reverse");
    }
    cmd.args([range.as_str(), "--", full.as_str()]);
    let out = cmd.output().context("failed to run git blame")?;
    if !out.status.success() {
        bail!(
            "`git blame` of {full} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(parse_porcelain(&String::from_utf8_lossy(&out.stdout)))
}

/// Line number (in the blamed version) → commit, from `git blame
/// --porcelain`: each line's header is `<sha> <orig line> <final line>
/// [<lines in group>]`, followed by key-value lines and the line itself
/// after a tab.
fn parse_porcelain(text: &str) -> HashMap<usize, String> {
    let mut out = HashMap::new();
    let mut header = true;
    for line in text.lines() {
        if line.starts_with('\t') {
            header = true;
            continue;
        }
        if !header {
            continue;
        }
        let f: Vec<&str> = line.split(' ').collect();
        let is_sha = |s: &str| s.len() >= 40 && s.chars().all(|c| c.is_ascii_hexdigit());
        if f.len() >= 3
            && is_sha(f[0])
            && let Ok(n) = f[2].parse::<usize>()
        {
            out.insert(n, f[0].to_string());
            header = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn porcelain_lines_get_their_commit() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let text = format!(
            "{a} 1 1 2\nauthor X\nsummary one\nfilename f.rs\n\tfn f() {{\n{a} 2 2\n\t}}\n{b} 5 3 1\nauthor Y\nboundary\nfilename f.rs\n\t// {a} 1 9\n"
        );
        let m = parse_porcelain(&text);
        assert_eq!(m.len(), 3);
        assert_eq!(m[&1], a);
        assert_eq!(m[&2], a);
        assert_eq!(m[&3], b);
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn item(id: &str, status: Status, line: usize, old_line: usize, source: &str) -> Item {
        let mut it = Item::default();
        it.info.id = id.to_string();
        it.info.kind = Kind::Fn;
        it.info.file = "src/lib.rs".to_string();
        it.info.line = line;
        it.old_line = Some(old_line);
        it.status = status;
        it.source = source
            .lines()
            .map(|l| model::SourceLine(l.chars().next().unwrap(), l[1..].to_string()))
            .collect();
        it
    }

    #[test]
    fn each_commit_gets_the_items_it_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::create_dir(dir.join("src")).unwrap();
        let write = |text: &str| std::fs::write(dir.join("src/lib.rs"), text).unwrap();
        write("fn a() {}\nfn b() {}\nfn c() {}\n");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "base"]);
        let base = git(dir, &["rev-parse", "HEAD"]);
        // 1: changes `a`; 2: removes `c`; 3: touches only the README.
        write("fn a() { 1 }\nfn b() {}\nfn c() {}\n");
        git(dir, &["commit", "-qam", "Change a"]);
        write("fn a() { 1 }\nfn b() {}\n");
        git(dir, &["commit", "-qam", "Remove c"]);
        std::fs::write(dir.join("README.md"), "hi\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "Add a README"]);
        let head = git(dir, &["rev-parse", "HEAD"]);
        let items: BTreeMap<String, Item> = [
            item("a", Status::Changed, 1, 1, "-fn a() {}\n+fn a() { 1 }"),
            item("c", Status::Removed, 3, 3, "-fn c() {}"),
        ]
        .into_iter()
        .map(|it| (it.info.id.clone(), it))
        .collect();
        let files = ["src/lib.rs", "README.md"].map(|p| model::ChangedFile {
            path: p.to_string(),
            ..Default::default()
        });
        let range = GitRange {
            dir: dir.to_path_buf(),
            base,
            head,
            prefix: String::new(),
        };
        let mut warnings = Vec::new();
        let list = commits(&range, &items, &files, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        let got: Vec<(&str, Vec<&str>, Vec<&str>)> = list
            .iter()
            .map(|c| {
                (
                    c.title.as_str(),
                    c.items.iter().map(String::as_str).collect(),
                    c.files.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("Change a", vec!["a"], vec!["src/lib.rs"]),
                ("Remove c", vec!["c"], vec!["src/lib.rs"]),
                ("Add a README", vec![], vec!["README.md"]),
            ]
        );
    }
}
