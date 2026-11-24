//! Saves the analysis of a PR's head, so that a later run can reuse it
//! instead of analyzing the same tree again: the page of the changes since
//! the previous comment compares the head with the commit that comment
//! showed, which the run before analyzed as its head.
//!
//! A saved analysis comes from another run, and with a PR from a fork maybe
//! from the fork's own code, so it is read as untrusted input: it is only
//! used if its key says it is an analysis of exactly the same tree, by the
//! same build of the tool, in the same mode and with the same toolchain;
//! its size is limited; and the comparison that uses it falls back to a
//! fresh analysis if it fails (see `analyze_all`). What it can't rule out
//! is an analysis made up to match the key: whoever runs the tool decides
//! whether the run that saved it is trusted enough (see the action's
//! `action/previous-analysis.sh`).

use crate::analyze::Analysis;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

/// Changes whenever the saved form changes in a way that the tool
/// fingerprint would not already show (it changes with every build).
const FORMAT: u32 = 1;

/// The largest saved analysis that is read, uncompressed. A large
/// workspace's (e.g. dora's, ~1000 files) is a few hundred MB.
const MAX_SIZE: u64 = 2 << 30;

/// What an analysis depends on besides the tool's settings: the same key,
/// the same analysis.
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug, Clone)]
pub struct Key {
    format: u32,
    /// The build of the tool (see [`tool_fingerprint`]).
    tool: String,
    /// The analyzed git tree.
    tree: String,
    /// Build scripts and proc macros ran.
    trusted: bool,
    /// rust-analyzer resolved the calls.
    semantic: bool,
    /// The toolchain whose standard library rust-analyzer read (`rustc
    /// -V` in the analyzed directory, which in trusted mode honors its
    /// `rust-toolchain.toml`); empty without rust-analyzer.
    rustc: String,
}

impl Key {
    /// The key of the analysis of `dir`, a checkout of `tree`.
    pub fn new(tree: &str, dir: &Path, trusted: bool, semantic: bool) -> Key {
        let rustc = if semantic {
            std::process::Command::new("rustc")
                .arg("-V")
                .current_dir(dir)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        } else {
            String::new()
        };
        Key {
            format: FORMAT,
            tool: tool_fingerprint().to_string(),
            tree: tree.to_string(),
            trusted,
            semantic,
            rustc,
        }
    }

    /// Why an analysis with key `self` doesn't fit where `wanted` is
    /// needed, if it doesn't.
    fn mismatch(&self, wanted: &Key) -> Option<String> {
        let differs = |what: &str, a: &dyn std::fmt::Debug, b: &dyn std::fmt::Debug| {
            Some(format!("it is for {what} {a:?}, not {b:?}"))
        };
        if self.format != wanted.format {
            differs("format", &self.format, &wanted.format)
        } else if self.tool != wanted.tool {
            Some("it was made by another build of the tool".into())
        } else if self.tree != wanted.tree {
            differs("tree", &self.tree, &wanted.tree)
        } else if self.trusted != wanted.trusted {
            differs("trust-project-code", &self.trusted, &wanted.trusted)
        } else if self.semantic != wanted.semantic {
            differs("rust-analyzer", &self.semantic, &wanted.semantic)
        } else if self.rustc != wanted.rustc {
            differs("toolchain", &self.rustc, &wanted.rustc)
        } else {
            None
        }
    }
}

/// Identifies the running build of the tool: a hash of its executable. Not
/// a security measure (a made-up file can copy it), only so that an
/// analysis is never read by a build whose analysis may differ.
fn tool_fingerprint() -> &'static str {
    static FINGERPRINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        use std::hash::Hasher;
        let exe = std::env::current_exe().and_then(std::fs::read);
        match exe {
            Ok(bytes) => {
                let mut h = std::hash::DefaultHasher::new();
                h.write(&bytes);
                format!("{} {:016x}", env!("CARGO_PKG_VERSION"), h.finish())
            }
            // Matches nothing, not even itself: an analysis is then never
            // reused.
            Err(e) => format!("unknown ({e}) {:?}", std::time::SystemTime::now()),
        }
    })
}

/// The saved form, after the key's line.
#[derive(Serialize, Deserialize)]
struct Saved {
    /// How calls were resolved (see `analyze_side`).
    note: String,
    analysis: Analysis,
}

/// Writes `analysis` and its note with `key` to `path`, gzipped: the key
/// on the first line, so that a file for another key is refused without
/// reading the rest.
pub fn save(path: &Path, key: &Key, (analysis, note): &(Analysis, String)) -> Result<()> {
    let file =
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut gz =
        flate2::write::GzEncoder::new(std::io::BufWriter::new(file), flate2::Compression::fast());
    serde_json::to_writer(&mut gz, key)?;
    gz.write_all(b"\n")?;
    let saved = SavedRef { note, analysis };
    serde_json::to_writer(&mut gz, &saved)?;
    gz.finish()?.flush()?;
    Ok(())
}

/// [`Saved`], borrowed.
#[derive(Serialize)]
struct SavedRef<'a> {
    note: &'a str,
    analysis: &'a Analysis,
}

/// Reads an analysis that [`save`] wrote, if its key is `wanted`.
pub fn load(path: &Path, wanted: &Key) -> Result<(Analysis, String)> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut input = BufReader::new(flate2::read::GzDecoder::new(file).take(MAX_SIZE + 1));
    let mut line = Vec::new();
    // The key is small: a longer first line is no key.
    (&mut input).take(4096).read_until(b'\n', &mut line)?;
    let key: Key = serde_json::from_slice(&line).context("its key is malformed")?;
    if let Some(why) = key.mismatch(wanted) {
        bail!("{why}");
    }
    let mut rest = Vec::new();
    input.read_to_end(&mut rest)?;
    if (line.len() + rest.len()) as u64 > MAX_SIZE {
        bail!("it is larger than {} MB", MAX_SIZE >> 20);
    }
    let saved: Saved = serde_json::from_slice(&rest).context("it is malformed")?;
    Ok((saved.analysis, saved.note))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(tree: &str) -> Key {
        Key {
            format: FORMAT,
            tool: tool_fingerprint().to_string(),
            tree: tree.into(),
            trusted: false,
            semantic: true,
            rustc: "rustc 1.0.0".into(),
        }
    }

    fn analysis() -> Analysis {
        Analysis {
            items: Default::default(),
            externals: Default::default(),
            crates: vec!["a".into()],
            warnings: vec!["w".into()],
        }
    }

    #[test]
    fn a_saved_analysis_is_read_back_only_with_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json.gz");
        save(&path, &key("t1"), &(analysis(), "note".into())).unwrap();
        let (a, note) = load(&path, &key("t1")).unwrap();
        assert_eq!(
            (a.crates, a.warnings, note.as_str()),
            (vec!["a".into()], vec!["w".into()], "note")
        );

        let err = load(&path, &key("t2")).map(|_| ()).unwrap_err().to_string();
        assert!(err.contains("tree"), "{err}");
        let trusted = Key {
            trusted: true,
            ..key("t1")
        };
        assert!(load(&path, &trusted).is_err());
        let other_tool = Key {
            tool: "other".into(),
            ..key("t1")
        };
        assert!(load(&path, &other_tool).is_err());
    }

    #[test]
    fn a_malformed_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.json.gz");
        // Not gzipped.
        std::fs::write(&path, "{}\n{}").unwrap();
        assert!(load(&path, &key("t")).is_err());
        // The right key, then something else.
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        serde_json::to_writer(&mut gz, &key("t")).unwrap();
        gz.write_all(b"\n{\"note\": 1}").unwrap();
        std::fs::write(&path, gz.finish().unwrap()).unwrap();
        let err = load(&path, &key("t")).map(|_| ()).unwrap_err().to_string();
        assert!(err.contains("malformed"), "{err}");
    }
}
