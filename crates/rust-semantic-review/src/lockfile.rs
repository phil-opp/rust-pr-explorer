//! Lock files compared package by package: `Cargo.lock`, and the Python
//! lock files in the same TOML shape (`uv.lock`, `poetry.lock`), whose
//! packages are `[[package]]` tables with a `name` and a `version`.
//!
//! For `Cargo.lock`, the dependency lists tell which packages are the
//! workspace's own, which are its direct dependencies and which depend on
//! each package. A crate's description, repository, changelog and whether
//! it runs code at build time come from its sources, if Cargo downloaded
//! them for the analysis: nothing is fetched here.

use model::{CrateInfo, DepKind, LockChange};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The lock files this module reads, by file name.
pub fn is_lock(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(name, "Cargo.lock" | "uv.lock" | "poetry.lock")
}

/// One locked package version.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Locked {
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

/// A parsed lock file.
#[derive(Default)]
struct Lock {
    /// The locked versions of each package.
    packages: BTreeMap<String, Vec<Locked>>,
    /// For `Cargo.lock`: the packages that depend on each package, by
    /// name.
    dependents: BTreeMap<String, BTreeSet<String>>,
}

impl Lock {
    /// A `Cargo.lock` package without a source is the workspace's own or
    /// a path dependency.
    fn local(&self, name: &str) -> bool {
        self.packages
            .get(name)
            .is_some_and(|l| l.iter().any(|p| p.source.is_none()))
    }

    fn dep_kind(&self, name: &str) -> DepKind {
        if self.local(name) {
            DepKind::Local
        } else if self
            .dependents
            .get(name)
            .is_some_and(|d| d.iter().any(|d| self.local(d)))
        {
            DepKind::Direct
        } else {
            DepKind::Transitive
        }
    }

    fn crates_io(&self, name: &str) -> bool {
        self.packages.get(name).is_some_and(|l| {
            l.iter()
                .any(|p| p.source.as_deref().is_some_and(crates_io_source))
        })
    }
}

/// The lock file's packages, or `None` if the file doesn't parse.
fn packages(text: &str) -> Option<Lock> {
    let t: toml::Table = text.parse().ok()?;
    let mut out: BTreeMap<String, Vec<Locked>> = BTreeMap::new();
    let mut dependents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in t.get("package")?.as_array()? {
        let Some(p) = p.as_table() else { continue };
        let (Some(name), Some(version)) = (
            p.get("name").and_then(|v| v.as_str()),
            p.get("version").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let source = match p.get("source") {
            Some(toml::Value::String(s)) => Some(s.clone()),
            // uv: `source = { registry = "…" }`, `{ git = "…" }`,
            // `{ editable = "." }`, …; poetry: `[package.source] type, url`.
            Some(toml::Value::Table(t)) => {
                let parts: Vec<String> = t
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| format!("{k} {v}")))
                    .collect();
                (!parts.is_empty()).then(|| parts.join(", "))
            }
            _ => None,
        };
        let checksum = p
            .get("checksum")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| {
                // uv: the hash of the source distribution.
                p.get("sdist")
                    .and_then(|s| s.get("hash"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });
        out.entry(name.to_string()).or_default().push(Locked {
            version: version.to_string(),
            source,
            checksum,
        });
        // `Cargo.lock`: "name", or "name version" or "name version
        // (source)" when it is locked in several versions.
        for d in p
            .get("dependencies")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|d| d.as_str())
        {
            if let Some(dep) = d.split_whitespace().next() {
                dependents
                    .entry(dep.to_string())
                    .or_default()
                    .insert(name.to_string());
            }
        }
    }
    Some(Lock {
        packages: out,
        dependents,
    })
}

/// The sources of crates.io in `Cargo.lock`.
fn crates_io_source(s: &str) -> bool {
    s == "registry+https://github.com/rust-lang/crates.io-index"
        || s == "sparse+https://index.crates.io/"
}

/// Sources of the default registries, which aren't worth showing.
fn default_source(s: &str) -> bool {
    crates_io_source(s) || s == "registry https://pypi.org/simple"
}

/// The packages of the lock file at `path` whose locked versions, sources
/// or checksums changed, by name. Empty if either version doesn't parse.
/// [`add_info`] adds what the crates' sources say, once Cargo has
/// downloaded them.
pub fn compare(path: &str, old: &str, new: &str) -> Vec<LockChange> {
    let cargo = path.rsplit('/').next() == Some("Cargo.lock");
    let (Some(lock_a), Some(lock_b)) = (packages(old), packages(new)) else {
        return Vec::new();
    };
    let (a, b) = (&lock_a.packages, &lock_b.packages);
    let names: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let mut out = Vec::new();
    for name in names {
        let (o, n) = (
            a.get(name).cloned().unwrap_or_default(),
            b.get(name).cloned().unwrap_or_default(),
        );
        if o == n {
            continue;
        }
        let versions = |l: &[Locked]| l.iter().map(|p| p.version.clone()).collect::<Vec<_>>();
        let sources = |l: &[Locked]| -> BTreeSet<String> {
            l.iter()
                .filter_map(|p| p.source.clone())
                .filter(|s| !default_source(s))
                .collect()
        };
        let (os, ns) = (sources(&o), sources(&n));
        let show = |s: BTreeSet<String>| {
            (!s.is_empty()).then(|| s.into_iter().collect::<Vec<_>>().join("; "))
        };
        // The same version, from the same source, with another checksum.
        let checksum_changed = n.iter().any(|p| {
            o.iter().any(|q| {
                q.version == p.version
                    && q.source == p.source
                    && q.checksum.is_some()
                    && p.checksum.is_some()
                    && q.checksum != p.checksum
            })
        });
        let source_changed = os != ns;
        let (ov, nv) = (versions(&o), versions(&n));
        if ov == nv && !source_changed && !checksum_changed {
            // Only its dependency list changed, which the other entries
            // show.
            continue;
        }
        // A removed package is described as it was.
        let side = if n.is_empty() { &lock_a } else { &lock_b };
        out.push(LockChange {
            dep: cargo.then(|| side.dep_kind(name)),
            dependents: if cargo {
                side.dependents
                    .get(name.as_str())
                    .map(|d| d.iter().cloned().collect())
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
            crates_io: cargo && side.crates_io(name),
            info: None,
            name: name.clone(),
            old: ov,
            new: nv,
            old_source: if source_changed {
                show(os.clone())
            } else {
                None
            },
            new_source: if source_changed || o.is_empty() {
                show(ns)
            } else {
                None
            },
            checksum_changed,
        });
    }
    out
}

/// The directories that Cargo extracts crates.io packages into:
/// `$CARGO_HOME/registry/src/index.crates.io-…/`.
pub fn registry_sources() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
    else {
        return Vec::new();
    };
    let Ok(dirs) = std::fs::read_dir(home.join("registry/src")) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = dirs
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            // The sparse index, and the git index Cargo used before 1.70.
            name.starts_with("index.crates.io-") || name.starts_with("github.com-")
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// Fills in `info` of each crates.io package from the manifest of its
/// newest locked version (the removed version for a removed package), if
/// one of `sources` has it.
pub fn add_info(changes: &mut [LockChange], sources: &[PathBuf]) {
    for c in changes.iter_mut().filter(|c| c.crates_io) {
        let Some(version) = c.new.last().or(c.old.last()) else {
            continue;
        };
        let dir = format!("{}-{version}", c.name);
        c.info = sources
            .iter()
            .map(|s| s.join(&dir))
            .find_map(|d| crate_info(&d));
    }
}

/// Files a crate's changelog is usually in, in order of preference.
const CHANGELOGS: [&str; 8] = [
    "CHANGELOG.md",
    "CHANGES.md",
    "RELEASES.md",
    "HISTORY.md",
    "NEWS.md",
    "CHANGELOG",
    "CHANGELOG.txt",
    "CHANGES",
];

/// What the extracted package in `dir` says about itself.
fn crate_info(dir: &Path) -> Option<CrateInfo> {
    let manifest: toml::Table = std::fs::read_to_string(dir.join("Cargo.toml"))
        .ok()?
        .parse()
        .ok()?;
    let package = manifest.get("package")?.as_table()?;
    let text = |key: &str| {
        package
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|s| !s.is_empty())
    };
    let files: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let changelog = CHANGELOGS
        .iter()
        .find_map(|c| files.iter().find(|f| f.eq_ignore_ascii_case(c)))
        .cloned();
    // Cargo builds `build.rs` unless the manifest names another script or
    // says `build = false`.
    let build_script = match package.get("build") {
        Some(toml::Value::Boolean(b)) => *b,
        Some(toml::Value::String(_)) => true,
        _ => files.iter().any(|f| f == "build.rs"),
    };
    let lib = manifest.get("lib").and_then(|l| l.as_table());
    let proc_macro = lib.is_some_and(|l| {
        ["proc-macro", "proc_macro"]
            .iter()
            .any(|k| l.get(*k).and_then(|v| v.as_bool()) == Some(true))
    });
    Some(CrateInfo {
        description: text("description"),
        repository: text("repository").filter(|r| r.starts_with("https://")),
        changelog,
        build_script,
        proc_macro,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, version: &str, extra: &str) -> String {
        format!("[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n{extra}\n")
    }

    const CRATES: &str = "source = \"registry+https://github.com/rust-lang/crates.io-index\"\n";

    #[test]
    fn cargo_lock() {
        let old = [
            "version = 4\n\n".to_string(),
            pkg("app", "0.1.0", "dependencies = [\"serde\"]"),
            pkg("serde", "1.0.200", &format!("{CRATES}checksum = \"aa\"")),
            pkg("syn", "1.0.109", &format!("{CRATES}checksum = \"bb\"")),
            pkg("syn", "2.0.50", &format!("{CRATES}checksum = \"cc\"")),
            pkg("log", "0.4.20", &format!("{CRATES}checksum = \"dd\"")),
            pkg("old", "1.0.0", CRATES),
        ]
        .concat();
        let new = [
            "version = 4\n\n".to_string(),
            pkg("app", "0.1.0", "dependencies = [\"serde\", \"regex\"]"),
            pkg("serde", "1.0.210", &format!("{CRATES}checksum = \"ab\"")),
            pkg("syn", "2.0.50", &format!("{CRATES}checksum = \"cc\"")),
            pkg("log", "0.4.20", &format!("{CRATES}checksum = \"de\"")),
            pkg(
                "regex",
                "1.10.0",
                "source = \"git+https://example.com/regex#abc\"",
            ),
        ]
        .concat();
        let c = compare("Cargo.lock", &old, &new);
        let rows: Vec<(&str, Vec<&str>, Vec<&str>)> = c
            .iter()
            .map(|l| {
                (
                    l.name.as_str(),
                    l.old.iter().map(|s| s.as_str()).collect(),
                    l.new.iter().map(|s| s.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("log", vec!["0.4.20"], vec!["0.4.20"]),
                ("old", vec!["1.0.0"], vec![]),
                ("regex", vec![], vec!["1.10.0"]),
                ("serde", vec!["1.0.200"], vec!["1.0.210"]),
                ("syn", vec!["1.0.109", "2.0.50"], vec!["2.0.50"]),
            ]
        );
        assert!(c[0].checksum_changed);
        assert!(!c[3].checksum_changed);
        assert_eq!(
            c[2].new_source.as_deref(),
            Some("git+https://example.com/regex#abc")
        );
        assert_eq!(c[3].new_source, None);
        // `serde` is a dependency of the workspace's `app`; `old`, `log`
        // and `syn` of nothing local.
        let kinds: Vec<Option<DepKind>> = c.iter().map(|l| l.dep).collect();
        use DepKind::*;
        assert_eq!(
            kinds,
            [
                Some(Transitive),
                Some(Transitive),
                Some(Direct),
                Some(Direct),
                Some(Transitive)
            ]
        );
        assert_eq!(c[2].dependents, ["app"]);
        assert!(c[3].crates_io && !c[2].crates_io);
    }

    #[test]
    fn transitive_and_local() {
        let old = [
            pkg("app", "0.1.0", "dependencies = [\"a\"]"),
            pkg("a", "1.0.0", CRATES),
        ]
        .concat();
        let new = [
            pkg("app", "0.2.0", "dependencies = [\"a\"]"),
            pkg(
                "a",
                "1.1.0",
                &format!("{CRATES}dependencies = [\"b 2.0.0\"]"),
            ),
            pkg("b", "2.0.0", CRATES),
        ]
        .concat();
        let c = compare("sub/Cargo.lock", &old, &new);
        let rows: Vec<(&str, Option<DepKind>, Vec<&str>)> = c
            .iter()
            .map(|l| {
                (
                    l.name.as_str(),
                    l.dep,
                    l.dependents.iter().map(|s| s.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("a", Some(DepKind::Direct), vec!["app"]),
                ("app", Some(DepKind::Local), vec![]),
                ("b", Some(DepKind::Transitive), vec!["a"]),
            ]
        );
        assert!(!c[1].crates_io);
    }

    #[test]
    fn crate_metadata() {
        let src = tempfile::tempdir().unwrap();
        let dir = src.path().join("b-2.0.0");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"b\"\nversion = \"2.0.0\"\ndescription = \"\"\"\nDoes\n  things\"\"\"\nrepository = \"https://github.com/x/b\"\n\n[lib]\nproc-macro = true\n",
        )
        .unwrap();
        std::fs::write(dir.join("build.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.join("Changelog.md"), "").unwrap();
        let mut c = compare(
            "Cargo.lock",
            "",
            &[pkg("b", "2.0.0", CRATES), pkg("c", "1.0.0", CRATES)].concat(),
        );
        // An empty file doesn't parse as a lock file.
        assert!(c.is_empty());
        c = compare(
            "Cargo.lock",
            &pkg("x", "1.0.0", CRATES),
            &[pkg("b", "2.0.0", CRATES), pkg("c", "1.0.0", CRATES)].concat(),
        );
        add_info(&mut c, &[src.path().to_path_buf()]);
        assert_eq!(
            c[0].info,
            Some(CrateInfo {
                description: Some("Does things".into()),
                repository: Some("https://github.com/x/b".into()),
                changelog: Some("Changelog.md".into()),
                build_script: true,
                proc_macro: true,
            })
        );
        // Not downloaded.
        assert_eq!((c[1].name.as_str(), &c[1].info), ("c", &None));
    }

    #[test]
    fn uv_and_poetry_lock() {
        let uv = |v: &str, src: &str| {
            format!(
                "version = 1\n\n[[package]]\nname = \"requests\"\nversion = \"{v}\"\nsource = {src}\n"
            )
        };
        let pypi = "{ registry = \"https://pypi.org/simple\" }";
        let c = compare(
            "uv.lock",
            &uv("2.31.0", pypi),
            &uv("2.32.0", "{ git = \"https://x\" }"),
        );
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].old_source.as_deref(), c[0].new_source.as_deref()),
            (None, Some("git https://x"))
        );
        let poetry = |v: &str| {
            format!(
                "[[package]]\nname = \"click\"\nversion = \"{v}\"\ndescription = \"x\"\noptional = false\n\n[metadata]\nlock-version = \"2.0\"\n"
            )
        };
        let c = compare("poetry.lock", &poetry("8.1.7"), &poetry("8.1.8"));
        assert_eq!(
            (c[0].old.clone(), c[0].new.clone()),
            (vec!["8.1.7".to_string()], vec!["8.1.8".to_string()])
        );
        assert!(is_lock("sub/uv.lock") && !is_lock("yarn.lock"));
    }
}
