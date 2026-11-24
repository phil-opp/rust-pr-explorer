//! Runs cargo-semver-checks on the two versions (`--semver-checks`) and
//! folds its findings into the items' SemVer labels.
//!
//! cargo-semver-checks compares the rustdoc JSON of both versions. That
//! covers what the definitions alone can't settle: auto traits (`Send`,
//! `Sync`, …), trait impls that derives and macros generate, and items
//! behind `cfg`s the build enables. Building rustdoc JSON runs build
//! scripts and procedural macros, so this only runs when asked for
//! (`--semver-checks`). It doesn't need rust-analyzer's trusted mode,
//! whose `cargo check` of the whole workspace costs much more.
//!
//! The tool has no machine-readable output. With `-vv` it prints each
//! result's values as JSON after the result's message; that JSON, with the
//! file and line the result points at, is what is read here. Whether a
//! lint needs a major or a minor version comes from `--list`.

use model::{ApiChange, ApiLevel, Item, Kind, Output, SemverChecks, SemverFinding, Status};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// One result as printed, before it is matched to an item.
#[derive(Debug, Default)]
struct Raw {
    lint: String,
    title: String,
    description: String,
    reference: Option<String>,
    message: String,
    values: Option<Value>,
}

/// Runs cargo-semver-checks with `head` as the current version and `base`
/// as the baseline, and folds what it finds into `out`. Failures are kept
/// in the result, not returned: the page says why there are no findings.
pub fn run(base: &Path, head: &Path, out: &mut Output) {
    let started = Instant::now();
    let mut report = SemverChecks::default();
    match check(base, head) {
        Ok((version, features, levels, checks, raw)) => {
            report.version = version;
            report.features = features;
            report.checks = checks;
            report.findings = raw
                .into_iter()
                .map(|r| finding(r, &levels, base, head, &out.items))
                .collect();
            fold(out, &report.findings);
        }
        Err((version, e)) => {
            report.version = version;
            report.error = Some(e);
        }
    }
    report.seconds = started.elapsed().as_secs_f32();
    out.semver_checks = Some(report);
}

type Checked = (String, String, HashMap<String, ApiLevel>, u32, Vec<Raw>);

fn check(base: &Path, head: &Path) -> Result<Checked, (String, String)> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let version = Command::new(&cargo)
        .args(["semver-checks", "--version"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            let v = String::from_utf8_lossy(&o.stdout);
            v.trim().rsplit(' ').next().unwrap_or_default().to_string()
        })
        .ok_or_else(|| {
            (
                String::new(),
                "cargo-semver-checks is not installed (`cargo install cargo-semver-checks`)"
                    .to_string(),
            )
        })?;
    let err = |e: String| (version.clone(), e);
    let list = Command::new(&cargo)
        .args(["semver-checks", "--list"])
        .output()
        .map_err(|e| err(e.to_string()))?;
    let levels = lint_levels(&String::from_utf8_lossy(&list.stdout));
    if !head.join("Cargo.toml").is_file() || !base.join("Cargo.toml").is_file() {
        return Err(err(
            "there is no `Cargo.toml` at the root of both versions".to_string()
        ));
    }
    // Its default is every feature except ones named like `unstable` or
    // `nightly`. Crates with mutually exclusive features (`log`'s
    // `max_level_*`) don't build that way; then the default features.
    let mut errors = Vec::new();
    for (features, flag) in [
        ("most features", None),
        ("default features", Some("--default-features")),
    ] {
        let o = Command::new(&cargo)
            .current_dir(head)
            // No backtraces in its errors.
            .env("RUST_BACKTRACE", "0")
            .env("RUST_LIB_BACKTRACE", "0")
            .args([
                "semver-checks",
                "check-release",
                "--workspace",
                "--baseline-root",
            ])
            .arg(base)
            // Every lint's results, whatever the version numbers say.
            .args(["--release-type", "patch", "--color", "never", "-vv"])
            .args(flag)
            .output()
            .map_err(|e| err(e.to_string()))?;
        let stdout = String::from_utf8_lossy(&o.stdout);
        let stderr = String::from_utf8_lossy(&o.stderr);
        // Exit code 1 means it found something; a run that checked
        // nothing prints no summary.
        if !stderr
            .lines()
            .any(|l| l.trim_start().starts_with("Summary"))
        {
            errors.push(format!("with {features}: {}", error_lines(&stderr)));
            continue;
        }
        let checks = stderr
            .lines()
            .filter_map(|l| {
                let rest = l.trim_start().strip_prefix("Checked")?;
                let n = rest.split("] ").nth(1)?.split(' ').next()?;
                n.parse::<u32>().ok()
            })
            .max()
            .unwrap_or_default();
        return Ok((
            version,
            features.to_string(),
            levels,
            checks,
            parse(&stdout),
        ));
    }
    Err(err(errors.join("\n")))
}

/// The errors in cargo-semver-checks' output, without the compiler's
/// details: what failed to build, and why.
fn error_lines(stderr: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for l in stderr.lines().map(str::trim) {
        if l.starts_with("error") && !l.starts_with("error: aborting") && !lines.contains(&l) {
            lines.push(l);
        }
    }
    if lines.is_empty() {
        return "it stopped without a report".to_string();
    }
    lines.truncate(6);
    lines.join("\n")
}

/// Lint ids and their level, from `cargo semver-checks --list`.
fn lint_levels(list: &str) -> HashMap<String, ApiLevel> {
    list.lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let id = words.next()?;
            let level = match words.next()? {
                "major" => ApiLevel::Breaking,
                "minor" => ApiLevel::Compatible,
                _ => return None,
            };
            Some((id.to_string(), level))
        })
        .collect()
}

/// The results in cargo-semver-checks' report (stdout, with `-vv`).
fn parse(stdout: &str) -> Vec<Raw> {
    let mut out: Vec<Raw> = Vec::new();
    // The lint being reported, and whether its results have started.
    let mut lint: Option<Raw> = None;
    let mut in_results = false;
    let mut json: Option<String> = None;
    let mut lines = stdout.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(buf) = json.as_mut() {
            buf.push_str(line);
            buf.push('\n');
            if line == "    }" || line == "}" {
                let v = serde_json::from_str::<Value>(buf).ok();
                json = None;
                if let Some(last) = out.last_mut() {
                    last.values = v;
                }
            }
            continue;
        }
        if let Some(rest) = line
            .strip_prefix("--- failure ")
            .or_else(|| line.strip_prefix("--- warning "))
            .and_then(|r| r.strip_suffix(" ---"))
        {
            let (id, title) = rest.split_once(": ").unwrap_or((rest, ""));
            lint = Some(Raw {
                lint: id.to_string(),
                title: title.to_string(),
                ..Default::default()
            });
            in_results = false;
            continue;
        }
        let Some(l) = lint.as_mut() else { continue };
        if line == "Description:" {
            if let Some(d) = lines.next() {
                l.description = d.trim().to_string();
            }
        } else if let Some(r) = line.trim_start().strip_prefix("ref: ") {
            l.reference = Some(r.trim().to_string());
        } else if line == "Failed in:" {
            in_results = true;
        } else if !in_results {
            continue;
        } else if line == "\tlint rule output values:" {
            json = Some(String::new());
        } else if line == "{" {
            // A lint without a message template prints its values only.
            out.push(Raw {
                message: l.title.clone(),
                ..copy(l)
            });
            json = Some("{\n".to_string());
        } else if let Some(msg) = line.strip_prefix("  ")
            && !msg.starts_with(' ')
        {
            out.push(Raw {
                message: msg.to_string(),
                ..copy(l)
            });
        }
    }
    out
}

fn copy(l: &Raw) -> Raw {
    Raw {
        lint: l.lint.clone(),
        title: l.title.clone(),
        description: l.description.clone(),
        reference: l.reference.clone(),
        ..Default::default()
    }
}

/// Which version a file is in, and its path relative to that version's
/// root.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Base,
    Head,
}

fn relative<'a>(file: &'a str, base: &Path, head: &Path) -> Option<(Side, &'a str)> {
    for (side, root) in [(Side::Head, head), (Side::Base, base)] {
        let root = root.to_string_lossy();
        if let Some(rest) = file.strip_prefix(root.as_ref())
            && let Some(rest) = rest.strip_prefix('/')
        {
            return Some((side, rest));
        }
    }
    None
}

/// The message without the location it ends with (a temporary path).
fn without_location(msg: &str, base: &Path, head: &Path) -> String {
    let mut cut = msg.len();
    for root in [base, head] {
        let root = root.to_string_lossy();
        if let Some(at) = msg.find(root.as_ref()) {
            let before = &msg[..at];
            let start = [", previously in file ", " in file ", " in "]
                .iter()
                .find_map(|sep| before.strip_suffix(sep).map(str::len))
                .unwrap_or(at);
            cut = cut.min(start);
        }
    }
    msg[..cut].trim_end_matches([',', ' ']).to_string()
}

fn finding(
    r: Raw,
    levels: &HashMap<String, ApiLevel>,
    base: &Path,
    head: &Path,
    items: &std::collections::BTreeMap<String, Item>,
) -> SemverFinding {
    let v = r.values.as_ref();
    let at = v.and_then(|v| {
        let file = v.get("span_filename")?.as_str()?;
        let line = v.get("span_begin_line")?.as_u64()? as usize;
        Some((relative(file, base, head)?, line))
    });
    let names: Vec<&str> = v
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter(|(k, _)| k.ends_with("name"))
                .filter_map(|(_, v)| v.as_str())
                .chain(
                    o.get("path")
                        .and_then(Value::as_array)
                        .and_then(|p| p.last())
                        .and_then(Value::as_str),
                )
                .collect()
        })
        .unwrap_or_default();
    let path: Option<String> = v
        .and_then(|v| v.get("path"))
        .and_then(Value::as_array)
        .map(|p| {
            p.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("::")
        });
    let item = at
        .and_then(|((side, file), line)| item_at(items, side, file, line, &names))
        .or_else(|| path.as_deref().and_then(|p| item_by_path(items, p)));
    SemverFinding {
        level: levels.get(&r.lint).copied().unwrap_or(ApiLevel::Unknown),
        message: without_location(&r.message, base, head),
        reference: r.reference,
        title: if r.title.is_empty() {
            r.description
        } else {
            r.title
        },
        lint: r.lint,
        item,
        location: at.map(|((_, file), line)| format!("{file}:{line}")),
    }
}

/// The item whose definition contains `line` of `file` (the closest one
/// starting at or before it) on that side, if its name is one the result
/// names. Without a name to compare, nothing is matched.
fn item_at(
    items: &std::collections::BTreeMap<String, Item>,
    side: Side,
    file: &str,
    line: usize,
    names: &[&str],
) -> Option<String> {
    let start = |it: &Item| -> Option<usize> {
        match side {
            Side::Head if it.status != Status::Removed => {
                (it.info.file == file).then_some(it.info.line)
            }
            Side::Base if it.status == Status::Removed => {
                (it.info.file == file).then_some(it.info.line)
            }
            Side::Base if it.status != Status::Added => {
                let f = it.old_file.as_deref().unwrap_or(&it.info.file);
                (f == file).then(|| it.old_line.unwrap_or(it.info.line))
            }
            _ => None,
        }
    };
    let best = items
        .values()
        .filter(|it| it.info.kind != Kind::Mod)
        .filter_map(|it| Some((start(it)?, it)))
        .filter(|(l, _)| *l <= line)
        .max_by_key(|(l, _)| *l)?;
    let it = best.1;
    // Names as rustdoc writes them: `Error:DivisionByZero` names `Error`.
    let named = names
        .iter()
        .any(|n| *n == it.info.name || n.split([':', ' ']).any(|p| p == it.info.name));
    named.then(|| it.info.id.clone())
}

/// The item that has (or had) `path` as a public path.
fn item_by_path(items: &std::collections::BTreeMap<String, Item>, path: &str) -> Option<String> {
    let mut found = items.values().filter(|it| {
        it.info.public_paths.iter().any(|p| p == path)
            || it.changes.exports_removed.iter().any(|p| p == path)
    });
    let first = found.next()?;
    // Never guess between two items.
    found.next().is_none().then(|| first.info.id.clone())
}

/// Adds each matched finding to its item's label. A finding gives a
/// reason when the label from the definitions doesn't already cover it:
/// the item had no label, or the finding needs a bigger version bump
/// (which raises the label), or it is a non-breaking change next to
/// breaking ones. Otherwise the lint is recorded as confirming the label.
fn fold(out: &mut Output, findings: &[SemverFinding]) {
    // The labels from the definitions, before any finding is added.
    let own: HashMap<String, Option<ApiLevel>> = findings
        .iter()
        .filter_map(|f| f.item.clone())
        .map(|id| {
            let level = out
                .items
                .get(&id)
                .and_then(|it| it.api.as_ref())
                .map(|a| a.level);
            (id, level)
        })
        .collect();
    for f in findings {
        let Some(id) = f.item.as_ref() else { continue };
        let Some(it) = out.items.get_mut(id) else {
            continue;
        };
        let reason = format!(
            "{}{} (cargo-semver-checks `{}`)",
            f.message,
            if f.level == ApiLevel::Compatible {
                ", non-breaking"
            } else {
                ""
            },
            f.lint
        );
        // `Breaking` sorts first: a lower level is a bigger bump.
        let adds = match own[id] {
            None => true,
            Some(level) => f.level < level || (f.level > level && f.level == ApiLevel::Compatible),
        };
        let api = it.api.get_or_insert_with(|| ApiChange {
            level: f.level,
            reasons: Vec::new(),
            confirmed_by: Vec::new(),
        });
        if adds {
            api.level = api.level.min(f.level);
            if !api.reasons.contains(&reason) {
                api.reasons.push(reason);
            }
        } else if !api.confirmed_by.contains(&f.lint) {
            api.confirmed_by.push(f.lint.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = r#"
--- failure enum_variant_added: enum variant added on exhaustive enum ---

Description:
A publicly-visible enum without #[non_exhaustive] has a new variant.
        ref: https://doc.rust-lang.org/cargo/reference/semver.html#enum-variant-new
       impl: https://github.com/obi1kenobi/cargo-semver-checks/tree/v0.51.0/src/lints/enum_variant_added.ron

Failed in:
  variant Error:DivisionByZero in /tmp/x/head/src/lib.rs:23
	lint rule output values:
    {
      "enum_name": "Error",
      "path": [
        "c",
        "Error"
      ],
      "span_begin_line": 23,
      "span_end_line": 23,
      "span_filename": "/tmp/x/head/src/lib.rs",
      "variant_name": "DivisionByZero"
    }

--- failure struct_missing: pub struct removed or renamed ---

Description:
A publicly-visible struct cannot be imported by its prior path.
        ref: https://doc.rust-lang.org/cargo/reference/semver.html#item-remove
       impl: https://github.com/obi1kenobi/cargo-semver-checks/tree/v0.51.0/src/lints/struct_missing.ron

Failed in:
  struct c::Parser, previously in file /tmp/x/base/src/parser.rs:9
	lint rule output values:
    {
      "name": "Parser",
      "path": [
        "c",
        "Parser"
      ],
      "span_begin_line": 9,
      "span_filename": "/tmp/x/base/src/parser.rs"
    }
"#;

    #[test]
    fn reads_the_report() {
        let raw = parse(REPORT);
        assert_eq!(raw.len(), 2);
        assert_eq!(raw[0].lint, "enum_variant_added");
        assert_eq!(raw[0].title, "enum variant added on exhaustive enum");
        assert_eq!(
            raw[0].reference.as_deref(),
            Some("https://doc.rust-lang.org/cargo/reference/semver.html#enum-variant-new")
        );
        let v = raw[0].values.as_ref().unwrap();
        assert_eq!(v["span_begin_line"], 23);
        assert_eq!(
            raw[1].message,
            "struct c::Parser, previously in file /tmp/x/base/src/parser.rs:9"
        );
        let (base, head) = (Path::new("/tmp/x/base"), Path::new("/tmp/x/head"));
        assert_eq!(
            without_location(&raw[1].message, base, head),
            "struct c::Parser"
        );
        assert_eq!(
            without_location(&raw[0].message, base, head),
            "variant Error:DivisionByZero"
        );
    }

    #[test]
    fn reads_lint_levels() {
        let levels = lint_levels(
            "id   type  description\n==   ====  ===\nenum_variant_added  major A …\nfunction_must_use_added minor B …\n",
        );
        assert_eq!(levels["enum_variant_added"], ApiLevel::Breaking);
        assert_eq!(levels["function_must_use_added"], ApiLevel::Compatible);
        assert_eq!(levels.len(), 2);
    }

    fn item(id: &str, file: &str, line: usize, status: Status) -> (String, Item) {
        let mut it = Item {
            status,
            ..Default::default()
        };
        it.info.id = id.into();
        it.info.name = id.rsplit("::").next().unwrap().into();
        it.info.kind = Kind::Struct;
        it.info.file = file.into();
        it.info.line = line;
        it.info.public = true;
        it.info.public_paths = vec![id.into()];
        (id.into(), it)
    }

    #[test]
    fn findings_go_to_their_items_and_raise_labels() {
        let mut items: std::collections::BTreeMap<String, Item> = [
            item("c::Error", "src/lib.rs", 11, Status::Changed),
            item("c::calc", "src/lib.rs", 30, Status::Unchanged),
            item("c::Parser", "src/parser.rs", 7, Status::Removed),
        ]
        .into_iter()
        .collect();
        items.get_mut("c::Error").unwrap().api = Some(ApiChange {
            level: ApiLevel::Breaking,
            reasons: vec!["variant added".into()],
            confirmed_by: Vec::new(),
        });
        let levels: HashMap<String, ApiLevel> = [
            ("enum_variant_added".to_string(), ApiLevel::Breaking),
            ("struct_missing".to_string(), ApiLevel::Breaking),
        ]
        .into_iter()
        .collect();
        let (base, head) = (Path::new("/tmp/x/base"), Path::new("/tmp/x/head"));
        let findings: Vec<SemverFinding> = parse(REPORT)
            .into_iter()
            .map(|r| finding(r, &levels, base, head, &items))
            .collect();
        assert_eq!(findings[0].item.as_deref(), Some("c::Error"));
        assert_eq!(findings[0].location.as_deref(), Some("src/lib.rs:23"));
        assert_eq!(findings[1].item.as_deref(), Some("c::Parser"));

        let mut out = Output {
            meta: model::Meta::default(),
            crates: vec!["c".into()],
            items,
            files: Vec::new(),
            externals: Default::default(),
            warnings: Vec::new(),
            semver_checks: None,
        };
        fold(&mut out, &findings);
        let api = out.items["c::Error"].api.as_ref().unwrap();
        // Already breaking: confirmed, no second reason.
        assert_eq!(api.reasons.len(), 1);
        assert_eq!(api.confirmed_by, vec!["enum_variant_added"]);
        // No label before: it gets the finding's.
        let api = out.items["c::Parser"].api.as_ref().unwrap();
        assert_eq!(api.level, ApiLevel::Breaking);
        assert!(
            api.reasons[0].contains("`struct_missing`"),
            "{:?}",
            api.reasons
        );
    }

    #[test]
    fn a_line_in_an_item_of_another_name_is_not_matched() {
        let items: std::collections::BTreeMap<String, Item> =
            [item("c::Other", "src/lib.rs", 1, Status::Changed)]
                .into_iter()
                .collect();
        assert_eq!(
            item_at(&items, Side::Head, "src/lib.rs", 5, &["Error"]),
            None
        );
        assert_eq!(
            item_at(&items, Side::Head, "src/lib.rs", 5, &["Other"]).as_deref(),
            Some("c::Other")
        );
    }
}
