//! The version a crate needs after its public API changes, by Cargo's
//! SemVer rules, compared with the version the PR sets.

use crate::code::esc;
use model::CrateVersion;

/// A version without pre-release or build parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u64, u64, u64);

impl Version {
    /// `None` for anything but `MAJOR.MINOR.PATCH`, such as `1.0.0-rc.1`:
    /// what comes after a pre-release isn't for this page to guess.
    fn parse(s: &str) -> Option<Version> {
        let mut parts = s.trim().split('.').map(|p| p.parse::<u64>().ok());
        let v = Version(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(v)
    }

    /// The next version with a breaking change: the first non-zero number
    /// goes up (`1.4.2` → `2.0.0`, `0.3.1` → `0.4.0`, `0.0.3` → `0.0.4`).
    fn breaking(self) -> Version {
        match self {
            Version(0, 0, p) => Version(0, 0, p + 1),
            Version(0, m, _) => Version(0, m + 1, 0),
            Version(major, ..) => Version(major + 1, 0, 0),
        }
    }

    /// The next version with only compatible changes: the number after
    /// the first non-zero one goes up (`1.4.2` → `1.5.0`, `0.3.1` →
    /// `0.3.2`). In `0.0.x`, every change is breaking.
    fn compatible(self) -> Version {
        match self {
            Version(0, 0, p) => Version(0, 0, p + 1),
            Version(0, m, p) => Version(0, m, p + 1),
            Version(major, m, _) => Version(major, m + 1, 0),
        }
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// How severe a crate's API changes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Breaking,
    /// No breaking change, but some to check by hand.
    Unknown,
    Compatible,
}

/// The sentence on the version `name` needs, and whether the PR's version
/// is enough. `None` when there is nothing specific to say: the crate is
/// new, isn't published, or a version isn't a plain `MAJOR.MINOR.PATCH`.
pub fn sentence(name: &str, v: &CrateVersion, level: Level) -> Option<String> {
    if !v.publish {
        return None;
    }
    let base = Version::parse(v.base.as_deref()?)?;
    let head = v.head.as_deref().and_then(Version::parse);
    let name = format!("<code>{}</code>", esc(name));
    let code = |v: Version| format!("<code>{v}</code>");
    let (breaking, compatible) = (base.breaking(), base.compatible());
    let needs = match level {
        Level::Breaking => format!(
            "{name} needs a breaking release: {} → {}.",
            code(base),
            code(breaking)
        ),
        Level::Compatible => format!(
            "{name} needs a compatible release: {} → {}.",
            code(base),
            code(compatible)
        ),
        Level::Unknown if breaking == compatible => format!(
            "{name} needs a new release: {} → {}.",
            code(base),
            code(breaking)
        ),
        Level::Unknown => format!(
            "{name} needs at least a compatible release: {} → {}, or {} if a change to check by hand is breaking.",
            code(base),
            code(compatible),
            code(breaking)
        ),
    };
    let enough = match level {
        Level::Breaking | Level::Unknown => breaking,
        Level::Compatible => compatible,
    };
    let pr = match head {
        None => String::new(),
        Some(h) if h == base => " The PR doesn't change the version yet.".to_string(),
        Some(h) if h < base => format!(" The PR lowers it to {}.", code(h)),
        Some(h) if h >= enough => format!(" The PR sets {}, which is enough.", code(h)),
        Some(h) if level == Level::Unknown && h >= compatible => format!(
            " The PR sets {}, which is enough unless a change to check by hand is breaking.",
            code(h)
        ),
        Some(h) => format!(" The PR sets {}, which is not enough.", code(h)),
    };
    Some(format!("{needs}{pr}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(base: &str, head: &str) -> CrateVersion {
        CrateVersion {
            base: Some(base.into()),
            head: Some(head.into()),
            publish: true,
        }
    }

    fn text(base: &str, head: &str, level: Level) -> String {
        let html = sentence("c", &v(base, head), level).unwrap();
        html.replace("<code>", "").replace("</code>", "")
    }

    #[test]
    fn the_first_non_zero_number_is_the_major_one() {
        let p = |s| Version::parse(s).unwrap();
        assert_eq!(p("1.4.2").breaking(), p("2.0.0"));
        assert_eq!(p("1.4.2").compatible(), p("1.5.0"));
        assert_eq!(p("0.3.1").breaking(), p("0.4.0"));
        assert_eq!(p("0.3.1").compatible(), p("0.3.2"));
        assert_eq!(p("0.0.3").breaking(), p("0.0.4"));
        assert_eq!(p("0.0.3").compatible(), p("0.0.4"));
        assert_eq!(Version::parse("1.0.0-rc.1"), None);
        assert_eq!(Version::parse("1.0"), None);
    }

    #[test]
    fn says_whether_the_prs_version_is_enough() {
        use Level::*;
        assert_eq!(
            text("1.4.2", "1.4.2", Breaking),
            "c needs a breaking release: 1.4.2 → 2.0.0. The PR doesn't change the version yet."
        );
        assert_eq!(
            text("0.3.1", "0.4.0", Breaking),
            "c needs a breaking release: 0.3.1 → 0.4.0. The PR sets 0.4.0, which is enough."
        );
        assert_eq!(
            text("0.3.1", "0.3.2", Breaking),
            "c needs a breaking release: 0.3.1 → 0.4.0. The PR sets 0.3.2, which is not enough."
        );
        assert_eq!(
            text("1.4.2", "1.5.0", Compatible),
            "c needs a compatible release: 1.4.2 → 1.5.0. The PR sets 1.5.0, which is enough."
        );
        assert_eq!(
            text("1.4.2", "1.4.3", Compatible),
            "c needs a compatible release: 1.4.2 → 1.5.0. The PR sets 1.4.3, which is not enough."
        );
        assert_eq!(
            text("0.3.1", "0.3.2", Unknown),
            "c needs at least a compatible release: 0.3.1 → 0.3.2, or 0.4.0 if a change to check by hand is breaking. The PR sets 0.3.2, which is enough unless a change to check by hand is breaking."
        );
        assert_eq!(
            text("2.0.0", "1.9.0", Compatible),
            "c needs a compatible release: 2.0.0 → 2.1.0. The PR lowers it to 1.9.0."
        );
    }

    #[test]
    fn nothing_specific_for_new_unpublished_or_pre_release_crates() {
        let mut new = v("1.0.0", "1.0.0");
        new.base = None;
        assert_eq!(sentence("c", &new, Level::Breaking), None);
        let mut private = v("1.0.0", "1.0.0");
        private.publish = false;
        assert_eq!(sentence("c", &private, Level::Breaking), None);
        assert_eq!(
            sentence("c", &v("1.0.0-rc.1", "1.0.0"), Level::Breaking),
            None
        );
    }
}
