//! Lock files compared package by package: `Cargo.lock`, and the Python
//! lock files in the same TOML shape (`uv.lock`, `poetry.lock`), whose
//! packages are `[[package]]` tables with a `name` and a `version`.

use model::LockChange;
use std::collections::{BTreeMap, BTreeSet};

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

/// The locked versions of each package, or `None` if the file doesn't
/// parse.
fn packages(text: &str) -> Option<BTreeMap<String, Vec<Locked>>> {
    let t: toml::Table = text.parse().ok()?;
    let mut out: BTreeMap<String, Vec<Locked>> = BTreeMap::new();
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
    }
    Some(out)
}

/// Sources of the default registries, which aren't worth showing.
fn default_source(s: &str) -> bool {
    s == "registry+https://github.com/rust-lang/crates.io-index"
        || s == "sparse+https://index.crates.io/"
        || s == "registry https://pypi.org/simple"
}

/// The packages whose locked versions, sources or checksums changed, by
/// name. Empty if either version doesn't parse.
pub fn compare(old: &str, new: &str) -> Vec<LockChange> {
    let (Some(a), Some(b)) = (packages(old), packages(new)) else {
        return Vec::new();
    };
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
        out.push(LockChange {
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
        let c = compare(&old, &new);
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
        let c = compare(&poetry("8.1.7"), &poetry("8.1.8"));
        assert_eq!(
            (c[0].old.clone(), c[0].new.clone()),
            (vec!["8.1.7".to_string()], vec!["8.1.8".to_string()])
        );
        assert!(is_lock("sub/uv.lock") && !is_lock("yarn.lock"));
    }
}
