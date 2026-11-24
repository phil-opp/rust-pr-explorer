//! Every file a PR changes, with a line diff, and structured changes where
//! the format is understood (`Cargo.toml`, lock files, Python, JavaScript,
//! YAML, CSS, Markdown).

use crate::parts::{Def, Span, compare_defs, rust_outline};
use model::{ChangedFile, ManifestChange, PartChange, SourceLine, Status};
use similar::{ChangeTag, TextDiff};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Files larger than this are listed, but not diffed.
const MAX_BYTES: u64 = 1 << 20;
/// Lines of context around changes.
const CONTEXT: usize = 3;

/// Directories of build output and downloaded packages. A plain directory
/// usually has them; they are neither copied nor listed (see
/// [`changed_files`]).
pub const BUILD_DIRS: &[&str] = &["target", "node_modules"];

/// All regular files and symlinks under `root`, by path relative to it,
/// except version control data and directories named in `skip`. Symlinks
/// are not followed.
fn files(root: &Path, skip: &[&str]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    fn walk(root: &Path, dir: &Path, skip: &[&str], out: &mut BTreeSet<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(ty) = e.file_type() else { continue };
            let name = e.file_name();
            if name == ".git" {
                continue;
            }
            if ty.is_dir() {
                if !name.to_str().is_some_and(|n| skip.contains(&n)) {
                    walk(root, &path, skip, out);
                }
            } else if (ty.is_file() || ty.is_symlink())
                && let Ok(rel) = path.strip_prefix(root)
            {
                out.insert(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    walk(root, root, skip, &mut out);
    out
}

fn lang(path: &str, binary: bool) -> &'static str {
    if binary {
        return "binary";
    }
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "md" | "markdown" => "markdown",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "css" => "css",
        _ => "text",
    }
}

/// Why a file counts as generated or vendored: by its name, its directory,
/// or a marker in its first lines.
fn generated(path: &str, text: Option<&str>) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let locks = [
        "Cargo.lock",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "poetry.lock",
        "Pipfile.lock",
        "uv.lock",
        "go.sum",
        "Gemfile.lock",
        "composer.lock",
    ];
    if locks.contains(&name) {
        return Some("lock file".into());
    }
    if path
        .split('/')
        .any(|d| matches!(d, "vendor" | "third_party" | "node_modules"))
    {
        return Some("vendored".into());
    }
    if name.ends_with(".min.js") || name.ends_with(".min.css") {
        return Some("minified".into());
    }
    let head: String = text?.lines().take(5).collect::<Vec<_>>().join("\n");
    let lower = head.to_lowercase();
    if lower.contains("@generated") || lower.contains("do not edit") {
        return Some("marked as generated".into());
    }
    None
}

/// Changed lines with context, in hunks that start with an `@` line
/// holding "old_line new_line" (1-based).
fn hunks(old: &str, new: &str) -> (Vec<SourceLine>, u32, u32) {
    let diff = TextDiff::from_lines(old, new);
    let mut out = Vec::new();
    let (mut added, mut removed) = (0, 0);
    for group in diff.grouped_ops(CONTEXT) {
        let first = &group[0];
        out.push(SourceLine(
            '@',
            format!(
                "{} {}",
                first.old_range().start + 1,
                first.new_range().start + 1
            ),
        ));
        for op in &group {
            for c in diff.iter_changes(op) {
                let tag = match c.tag() {
                    ChangeTag::Equal => ' ',
                    ChangeTag::Insert => {
                        added += 1;
                        '+'
                    }
                    ChangeTag::Delete => {
                        removed += 1;
                        '-'
                    }
                };
                out.push(SourceLine(
                    tag,
                    c.value().trim_end_matches(['\n', '\r']).to_string(),
                ));
            }
        }
    }
    (out, added, removed)
}

/// The line numbers of the added lines (in the new version) and of the
/// removed lines (in the old one) of a file's hunks.
pub fn changed_lines(hunks: &[SourceLine]) -> (Vec<u32>, Vec<u32>) {
    let (mut added, mut removed) = (Vec::new(), Vec::new());
    let (mut o, mut n) = (0u32, 0u32);
    for l in hunks {
        match l.0 {
            '@' => {
                let mut it = l.1.split(' ').filter_map(|x| x.parse::<u32>().ok());
                o = it.next().unwrap_or(1);
                n = it.next().unwrap_or(1);
            }
            '+' => {
                added.push(n);
                n += 1;
            }
            '-' => {
                removed.push(o);
                o += 1;
            }
            _ => {
                o += 1;
                n += 1;
            }
        }
    }
    (added, removed)
}

/// Text of a file, if it is text and not too large. `Err` means binary.
/// A symlink reads as the path it points to; it is never followed.
fn read(path: &Path) -> Result<Option<String>, ()> {
    if let Ok(target) = std::fs::read_link(path) {
        return Ok(Some(format!("symbolic link to {}\n", target.display())));
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(None);
    };
    if meta.len() > MAX_BYTES {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|_| ())?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return Err(());
    }
    String::from_utf8(bytes).map(Some).map_err(|_| ())
}

/// Hash of one side of a changed file (see `ChangedFile::new_hash`): of
/// its bytes, or of the path a symlink points to. Empty without the file.
fn side_hash(present: bool, path: &Path) -> String {
    if !present {
        return String::new();
    }
    match std::fs::read_link(path) {
        Ok(target) => model::content_hash(format!("symlink {}", target.display()).as_bytes()),
        Err(_) => std::fs::read(path)
            .map(|b| model::content_hash(&b))
            .unwrap_or_default(),
    }
}

/// The files that differ between two trees, leaving out directories named
/// in `skip`.
pub fn changed_files(base: &Path, head: &Path, skip: &[&str]) -> Vec<ChangedFile> {
    let (a, b) = (files(base, skip), files(head, skip));
    let mut out = Vec::new();
    for path in a.union(&b) {
        let (pa, pb) = (base.join(path), head.join(path));
        let status = match (a.contains(path), b.contains(path)) {
            (true, true) => {
                let same = match (std::fs::read_link(&pa), std::fs::read_link(&pb)) {
                    (Ok(x), Ok(y)) => x == y,
                    (Err(_), Err(_)) => std::fs::read(&pa).ok() == std::fs::read(&pb).ok(),
                    _ => false,
                };
                if same {
                    continue;
                }
                Status::Changed
            }
            (false, true) => Status::Added,
            _ => Status::Removed,
        };
        let read_side = |present: bool, p: &Path| {
            if present {
                read(p)
            } else {
                Ok(Some(String::new()))
            }
        };
        let (ta, tb) = (
            read_side(a.contains(path), &pa),
            read_side(b.contains(path), &pb),
        );
        let binary = ta.is_err() || tb.is_err();
        let (ta, tb) = (ta.ok().flatten(), tb.ok().flatten());
        let symlink = [&pa, &pb].iter().any(|p| std::fs::read_link(p).is_ok());
        let mut file = ChangedFile {
            path: path.clone(),
            status,
            lang: if symlink {
                "symlink".to_string()
            } else {
                lang(path, binary).to_string()
            },
            generated: generated(path, tb.as_deref().or(ta.as_deref())),
            old_hash: side_hash(a.contains(path), &pa),
            new_hash: side_hash(b.contains(path), &pb),
            ..Default::default()
        };
        match (&ta, &tb) {
            (Some(old), Some(new)) => {
                let (lines, added, removed) = hunks(old, new);
                file.hunks = lines;
                file.added = added;
                file.removed = removed;
                match file.lang.as_str() {
                    "python" => file.parts = crate::python::compare(old, new),
                    "javascript" => file.parts = crate::javascript::compare(old, new),
                    "yaml" => {
                        use crate::parts::{compare_defs, yaml_outline};
                        file.parts = compare_defs(&yaml_outline(old), &yaml_outline(new), old, new);
                    }
                    "css" => file.styles = crate::css::compare(old, new),
                    "markdown" => {
                        file.old_text = Some(old.clone());
                        file.new_text = Some(new.clone());
                    }
                    _ => {}
                }
                if path == "Cargo.toml" || path.ends_with("/Cargo.toml") {
                    file.manifest = manifest_changes(old, new);
                }
                if crate::lockfile::is_lock(path) {
                    file.lock = crate::lockfile::compare(path, old, new);
                }
            }
            _ => file.too_large = !binary,
        }
        out.push(file);
    }
    out
}

/// The changes of an analyzed Rust file that the item view doesn't show:
/// code outside every item. Code the analysis skips (tests, macro calls,
/// `extern` blocks, …) is split into parts (`texts` are the base and head
/// file); the rest are changed lines outside items and parts. Imports and
/// module declarations (their effect is on the items), blank lines and
/// comments don't count. Returns the number of those lines, the hunks
/// reduced to them (with context), and the changed parts.
pub fn outside_items<'a>(
    file: &ChangedFile,
    items: impl Iterator<Item = &'a model::Item>,
    texts: (&str, &str),
) -> (u32, Vec<SourceLine>, Vec<PartChange>) {
    // Line spans (1-based, inclusive) of the items on each side.
    let (old_parts, old_decls) = rust_outline(texts.0);
    let (new_parts, new_decls) = rust_outline(texts.1);
    let mut old: Vec<Span> = Vec::new();
    let mut new: Vec<Span> = Vec::new();
    for it in items.filter(|i| i.info.kind != model::Kind::Mod) {
        let count = |tags: &[char]| it.source.iter().filter(|l| tags.contains(&l.0)).count();
        let old_file = it.old_file.as_deref().unwrap_or(&it.info.file);
        if it.status != Status::Removed && it.info.file == file.path {
            let n = count(&[' ', '+']);
            new.push((it.info.line, it.info.line + n.saturating_sub(1)));
        }
        if it.status != Status::Added && old_file == file.path {
            let start = it.old_line.unwrap_or(it.info.line);
            let n = count(&[' ', '-']);
            old.push((start, start + n.saturating_sub(1)));
        }
    }
    let inside = |spans: &[Span], l: usize| spans.iter().any(|&(a, b)| a <= l && l <= b);
    // Parts that no item covers (in test targets, tests are items).
    let outside = |parts: Vec<Def>, items: &[Span]| -> Vec<Def> {
        parts
            .into_iter()
            .filter(|d| !(d.first + 1..=d.last + 1).any(|l| inside(items, l)))
            .collect()
    };
    let (old_parts, new_parts) = (outside(old_parts, &old), outside(new_parts, &new));
    let parts = compare_defs(&old_parts, &new_parts, texts.0, texts.1);
    for (spans, decls, parts) in [
        (&mut old, old_decls, &old_parts),
        (&mut new, new_decls, &new_parts),
    ] {
        spans.extend(decls);
        spans.extend(parts.iter().map(|d| (d.first + 1, d.last + 1)));
    }
    let ignorable = |text: &str| {
        let t = text.trim();
        t.is_empty()
            || t.starts_with("//")
            || t.starts_with("use ")
            || t.starts_with("pub use ")
            || (t.starts_with("pub(") && t.contains(") use "))
            || (t.ends_with(';') && (t.starts_with("mod ") || t.starts_with("pub mod ")))
            || t == "}"
            // Inherent impl headers: their methods are the items, and their
            // cards show the header.
            || ((t.starts_with("impl ") || t.starts_with("impl<") || t.starts_with("unsafe impl"))
                && t.ends_with('{'))
    };
    // Each line of the hunks with its base and head line numbers, and
    // whether it is a change outside every item.
    let (mut o, mut n) = (0usize, 0usize);
    let mut rows: Vec<(usize, usize, bool)> = Vec::new();
    for l in &file.hunks {
        let (lo, ln) = (o, n);
        let outside = match l.0 {
            '@' => {
                let mut it = l.1.split(' ').filter_map(|x| x.parse::<usize>().ok());
                o = it.next().unwrap_or(1);
                n = it.next().unwrap_or(1);
                false
            }
            '+' => {
                n += 1;
                !inside(&new, ln) && !ignorable(&l.1)
            }
            '-' => {
                o += 1;
                !inside(&old, lo) && !ignorable(&l.1)
            }
            _ => {
                o += 1;
                n += 1;
                false
            }
        };
        rows.push((lo, ln, outside));
    }
    let count = rows.iter().filter(|r| r.2).count() as u32;
    // Only those changes, with context, in hunks of their own.
    let mut out = Vec::new();
    let mut open = false;
    for (i, l) in file.hunks.iter().enumerate() {
        let keep = l.0 != '@' && (rows[i].2 || near_change(&rows, &file.hunks, i));
        if keep {
            if !open {
                out.push(SourceLine('@', format!("{} {}", rows[i].0, rows[i].1)));
                open = true;
            }
            out.push(l.clone());
        } else {
            open = false;
        }
    }
    (count, out, parts)
}

/// Whether hunk line `i` is within [`CONTEXT`] lines of a change outside
/// every item, in the same hunk.
fn near_change(rows: &[(usize, usize, bool)], hunks: &[SourceLine], i: usize) -> bool {
    let same_hunk = |a: usize, b: usize| !hunks[a.min(b) + 1..=a.max(b)].iter().any(|l| l.0 == '@');
    let lo = i.saturating_sub(CONTEXT);
    let hi = (i + CONTEXT).min(rows.len() - 1);
    (lo..=hi).any(|j| rows[j].2 && same_hunk(i, j))
}

/// One `Cargo.toml` entry as written, on one line.
fn show(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => format!("{s:?}"),
        toml::Value::Table(t) => {
            let parts: Vec<String> = t
                .iter()
                .map(|(k, v)| format!("{k} = {}", show(v)))
                .collect();
            format!("{{ {} }}", parts.join(", "))
        }
        toml::Value::Array(a) => {
            format!("[{}]", a.iter().map(show).collect::<Vec<_>>().join(", "))
        }
        other => other.to_string(),
    }
}

/// The entries of the sections worth comparing, by (section, name).
fn entries(text: &str) -> Option<BTreeMap<(String, String), String>> {
    let t: toml::Table = text.parse().ok()?;
    let mut out = BTreeMap::new();
    let mut add_table = |section: String, table: &toml::Table| {
        for (k, v) in table {
            out.insert((section.clone(), k.clone()), show(v));
        }
    };
    for s in [
        "dependencies",
        "dev-dependencies",
        "build-dependencies",
        "features",
    ] {
        if let Some(table) = t.get(s).and_then(|v| v.as_table()) {
            add_table(s.to_string(), table);
        }
    }
    if let Some(targets) = t.get("target").and_then(|v| v.as_table()) {
        for (cfg, target) in targets {
            for s in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(table) = target.get(s).and_then(|v| v.as_table()) {
                    add_table(format!("target.'{cfg}'.{s}"), table);
                }
            }
        }
    }
    // Sources replaced for the whole build: they matter most.
    if let Some(patch) = t.get("patch").and_then(|v| v.as_table()) {
        for (registry, table) in patch {
            if let Some(table) = table.as_table() {
                add_table(format!("patch.{registry}"), table);
            }
        }
    }
    if let Some(table) = t.get("replace").and_then(|v| v.as_table()) {
        add_table("replace".into(), table);
    }
    if let Some(ws) = t.get("workspace").and_then(|v| v.as_table()) {
        if let Some(table) = ws.get("dependencies").and_then(|v| v.as_table()) {
            add_table("workspace.dependencies".into(), table);
        }
        if let Some(members) = ws.get("members") {
            out.insert(("workspace".into(), "members".into()), show(members));
        }
    }
    if let Some(p) = t.get("package").and_then(|v| v.as_table()) {
        for k in ["version", "edition", "rust-version", "name"] {
            if let Some(v) = p.get(k) {
                out.insert(("package".into(), k.into()), show(v));
            }
        }
    }
    Some(out)
}

/// What changed in a `Cargo.toml`: dependencies, `[patch]` and
/// `[replace]`, features, workspace members and package version, edition
/// and minimum Rust version.
pub fn manifest_changes(old: &str, new: &str) -> Vec<ManifestChange> {
    let (Some(a), Some(b)) = (entries(old), entries(new)) else {
        return Vec::new();
    };
    let keys: BTreeSet<&(String, String)> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| ManifestChange {
            section: k.0.clone(),
            name: k.1.clone(),
            old: a.get(k).cloned(),
            new: b.get(k).cloned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_entries() {
        let old = "[package]\nname = \"t\"\nversion = \"0.1.0\"\n[dependencies]\nserde = \"1\"\nlog = { version = \"0.4\", features = [\"std\"] }\n";
        let new = "[package]\nname = \"t\"\nversion = \"0.2.0\"\n[dependencies]\nlog = { version = \"0.4\", features = [\"std\", \"kv\"] }\nregex = \"1\"\n[target.'cfg(windows)'.dependencies]\nwinapi = \"0.3\"\n[features]\nfast = [\"regex\"]\n";
        let c = manifest_changes(old, new);
        let show: Vec<(&str, &str, Option<&str>, Option<&str>)> = c
            .iter()
            .map(|m| {
                (
                    m.section.as_str(),
                    m.name.as_str(),
                    m.old.as_deref(),
                    m.new.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            show,
            [
                (
                    "dependencies",
                    "log",
                    Some("{ features = [\"std\"], version = \"0.4\" }"),
                    Some("{ features = [\"std\", \"kv\"], version = \"0.4\" }")
                ),
                ("dependencies", "regex", None, Some("\"1\"")),
                ("dependencies", "serde", Some("\"1\""), None),
                ("features", "fast", None, Some("[\"regex\"]")),
                ("package", "version", Some("\"0.1.0\""), Some("\"0.2.0\"")),
                (
                    "target.'cfg(windows)'.dependencies",
                    "winapi",
                    None,
                    Some("\"0.3\"")
                ),
            ]
        );
    }

    #[test]
    fn changed_files_with_hunks() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let lines: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(a.path().join("notes.txt"), &lines).unwrap();
        std::fs::write(
            b.path().join("notes.txt"),
            lines.replace("line 10\n", "line ten\n"),
        )
        .unwrap();
        std::fs::write(a.path().join("same.txt"), "x").unwrap();
        std::fs::write(b.path().join("same.txt"), "x").unwrap();
        std::fs::create_dir(b.path().join("vendor")).unwrap();
        std::fs::write(b.path().join("vendor/lib.js"), "x").unwrap();
        std::fs::write(b.path().join("logo.png"), [0u8, 1, 2]).unwrap();
        let files = changed_files(a.path(), b.path(), &[]);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["logo.png", "notes.txt", "vendor/lib.js"]);
        assert_eq!(files[0].lang, "binary");
        assert!(files[0].hunks.is_empty());
        let notes = &files[1];
        assert_eq!((notes.added, notes.removed), (1, 1));
        // One hunk: 3 lines of context on each side.
        assert_eq!(notes.hunks[0], SourceLine('@', "7 7".into()));
        assert_eq!(notes.hunks.len(), 1 + 3 + 2 + 3);
        // The changed line is line 10 in both versions.
        assert_eq!(changed_lines(&notes.hunks), (vec![10], vec![10]));
        assert_eq!(files[2].generated.as_deref(), Some("vendored"));
        assert_eq!(files[2].status, Status::Added);
        // Each side's content hash, none for the side without the file.
        assert_eq!(notes.old_hash, model::content_hash(lines.as_bytes()));
        assert_ne!(notes.new_hash, notes.old_hash);
        assert_eq!(files[0].old_hash, "");
        assert_eq!(files[0].new_hash, model::content_hash(&[0, 1, 2]));
    }

    #[test]
    fn manifest_patches() {
        let old = "[patch.crates-io]\nserde = { path = \"../serde\" }\n";
        let new = "[patch.crates-io]\nserde = { git = \"https://example.com/serde\" }\n[replace]\n\"log:0.4.0\" = { path = \"log\" }\n";
        let c = manifest_changes(old, new);
        let keys: Vec<(&str, &str)> = c
            .iter()
            .map(|m| (m.section.as_str(), m.name.as_str()))
            .collect();
        assert_eq!(
            keys,
            [("patch.crates-io", "serde"), ("replace", "log:0.4.0")]
        );
    }
}
