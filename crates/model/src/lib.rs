//! The analysis result that the command-line tool embeds in the explorer
//! page and the viewer reads back.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(
    Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize, Hash, PartialOrd, Ord,
)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Mod,
    Struct,
    Enum,
    Union,
    Trait,
    Fn,
    Method,
    TraitMethod,
    Const,
    Static,
    TypeAlias,
    Macro,
    Impl,
    AssocConst,
    AssocType,
    /// An enum variant (only used for items outside the analyzed crates).
    Variant,
}

impl Kind {
    pub fn is_type_like(self) -> bool {
        matches!(
            self,
            Kind::Struct | Kind::Enum | Kind::Union | Kind::Trait | Kind::TypeAlias
        )
    }

    pub fn is_callable(self) -> bool {
        matches!(self, Kind::Fn | Kind::Method | Kind::TraitMethod)
    }

    pub fn is_member(self) -> bool {
        matches!(
            self,
            Kind::Method | Kind::TraitMethod | Kind::AssocConst | Kind::AssocType
        )
    }

    /// The word rustdoc uses for this kind of item.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Mod => "mod",
            Kind::Struct => "struct",
            Kind::Enum => "enum",
            Kind::Union => "union",
            Kind::Trait => "trait",
            Kind::Fn => "fn",
            Kind::Method | Kind::TraitMethod => "method",
            Kind::Const | Kind::AssocConst => "const",
            Kind::Static => "static",
            Kind::TypeAlias | Kind::AssocType => "type",
            Kind::Macro => "macro",
            Kind::Impl => "impl",
            Kind::Variant => "variant",
        }
    }

    /// Stable lowercase name, used as a CSS class.
    pub fn css(self) -> &'static str {
        match self {
            Kind::Mod => "mod",
            Kind::Struct => "struct",
            Kind::Enum => "enum",
            Kind::Union => "union",
            Kind::Trait => "trait",
            Kind::Fn => "fn",
            Kind::Method => "method",
            Kind::TraitMethod => "trait_method",
            Kind::Const => "const",
            Kind::Static => "static",
            Kind::TypeAlias => "type_alias",
            Kind::Macro => "macro",
            Kind::Impl => "impl",
            Kind::AssocConst => "assoc_const",
            Kind::AssocType => "assoc_type",
            Kind::Variant => "variant",
        }
    }
}

/// What a name refers to.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ref {
    /// Item id if the name refers to an item of an analyzed crate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Fully qualified path.
    pub path: String,
}

/// How an item uses another one.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UseCtx {
    Signature,
    Field,
    Impl,
    Body,
}

impl UseCtx {
    pub fn plural(self) -> &'static str {
        match self {
            UseCtx::Signature => "signatures",
            UseCtx::Field => "fields",
            UseCtx::Impl => "impls",
            UseCtx::Body => "bodies",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Call {
    pub to: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Use {
    pub to: String,
    pub ctx: UseCtx,
}

/// Everything known about an item on one side (base or head) of a PR.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ItemInfo {
    pub id: String,
    pub kind: Kind,
    /// Paths under which code outside the crate can name the item: its
    /// definition path if that is public, plus `pub use` re-exports.
    /// Shortest first; empty if it isn't reachable from outside.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_paths: Vec<String>,
    pub name: String,
    /// Id of the module the item is defined in (for a module: its parent;
    /// for a crate root: itself).
    pub module: String,
    /// `None` means `pub`; otherwise the module the item is visible in.
    pub vis_scope: Option<String>,
    pub vis_label: String,
    /// The item whose visibility additionally restricts this one (the
    /// module for free items, the type for methods).
    pub vis_parent: Option<String>,
    /// Reachable from outside the crate.
    pub public: bool,
    pub sig: String,
    /// Signature with private fields hidden, if different from `sig`.
    pub sig_public: Option<String>,
    pub docs: String,
    pub sig_refs: BTreeMap<String, Ref>,
    pub body_refs: BTreeMap<String, Ref>,
    pub doc_refs: BTreeMap<String, Ref>,
    /// Owning type, trait or impl for associated items.
    pub parent: Option<String>,
    /// `impl` header for associated items, e.g. `impl Display for Parser`.
    pub impl_header: Option<String>,
    pub trait_ref: Option<Ref>,
    pub self_ty: Option<String>,
    pub children: Vec<String>,
    pub file: String,
    pub line: usize,
    pub calls: BTreeSet<Call>,
    pub uses: BTreeSet<Use>,
    /// Call sites in the body whose target could not be determined (e.g.
    /// method calls without rust-analyzer). They are not in `calls`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unresolved_calls: u32,
    /// What names in the item's source refer to, by position: `line:column`
    /// (line as in the source file, column 0-based in chars) of a path's
    /// first segment or of a called method's name. Links in the source view
    /// are placed only at these positions.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_refs: BTreeMap<String, Ref>,
    /// Char offsets in `sig` where a resolved path starts: the only places
    /// where the signature links names (so a field or variant that happens
    /// to share a name with an item is never linked).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sig_links: Vec<u32>,
    /// Like `sig_links`, for `sig_public`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sig_public_links: Vec<u32>,
    /// For an item of another crate that this crate re-exports: the path
    /// of its definition, e.g. `serde_core::ser::Serialize`. Such an item
    /// stands for the re-export; its `sig` is the `pub use`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reexport_of: Option<String>,
    /// Code that exists only in tests of a library or binary: marked
    /// `#[test]` or `#[cfg(test)]`, or inside a test-only module, impl or
    /// trait. Never part of the public API.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test_only: bool,
    /// A test function: marked `#[test]`, or by an attribute macro that
    /// wraps it, such as `#[tokio::test]`. Set in integration tests too,
    /// where `test_only` is not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Added,
    Removed,
    Changed,
    #[default]
    Unchanged,
}

impl Status {
    pub fn css(self) -> &'static str {
        match self {
            Status::Added => "added",
            Status::Removed => "removed",
            Status::Changed => "changed",
            Status::Unchanged => "unchanged",
        }
    }
}

#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct Changes {
    pub signature: bool,
    pub body: bool,
    pub docs: bool,
    /// Only formatting, comments or import style differ.
    pub cosmetic: bool,
    /// Public paths the item gained or lost, e.g. through a `pub use`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports_added: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exports_removed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
}

/// A piece of a signature diff. Rendering the spans in order gives the new
/// signature, with removed parts shown where they used to be.
///
/// `g` groups the spans of one change (e.g. a replaced type's removed and
/// added text), so that the viewer can highlight them together.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DiffSpan {
    Same {
        text: String,
    },
    Added {
        text: String,
        g: u32,
    },
    Removed {
        text: String,
        g: u32,
    },
    /// A type was wrapped in another one (`T` → `Result<T, E>`), or with
    /// `unwrap` the wrapper was removed. `before` and `after` are the
    /// wrapper's text around the inner type.
    Wrap {
        before: String,
        inner: Vec<DiffSpan>,
        after: String,
        unwrap: bool,
        g: u32,
    },
}

/// One line of a signature diff's summary, e.g. "parameter `env` added".
/// The text uses backticks for code.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DiffNote {
    pub text: String,
    /// Groups of the spans this note describes.
    pub groups: Vec<u32>,
    /// The field or variant the note is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Its doc comment (Markdown): the new one, or for a removed member
    /// the old one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
}

/// A signature diff aligned on syntax: parameters, generic parameters,
/// where-clauses, fields and variants are matched by name, and types are
/// compared as trees.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SigDiff {
    pub spans: Vec<DiffSpan>,
    pub notes: Vec<DiffNote>,
}

/// One line of an item's source: ` `, `+` or `-` and the text.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SourceLine(pub char, pub String);

/// Lines of an item's base version (`old`, `old_len`) that its head
/// version has, only reformatted, at `new`, `new_len`. Lines are counted in
/// each version's lines: `source` without the lines of the other side.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reformatted {
    pub old: u32,
    pub old_len: u32,
    pub new: u32,
    pub new_len: u32,
}

/// A block of removed or added lines of an item's source that was moved.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MovedLines {
    /// `-` for removed lines (moved away), `+` for added ones (moved here).
    pub side: char,
    /// The block's first line and its number of lines, counted in that
    /// side's version of the source: `source` without the lines of the
    /// other side.
    pub start: u32,
    pub len: u32,
    /// The item the lines were moved to (for removed lines) or from (for
    /// added ones). It can be this item itself.
    pub other: String,
    /// The block's first line there, as a line number in that version's
    /// file.
    pub other_line: u32,
}

/// An item as compared between base and head.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Item {
    /// Head data, or base data for removed items.
    #[serde(flatten)]
    pub info: ItemInfo,
    pub status: Status,
    pub changes: Changes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_sig: Option<String>,
    /// Base version of `sig_public`, when `old_sig` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_sig_public: Option<String>,
    /// Structured diff of `old_sig` and `sig`, if both could be parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig_diff: Option<SigDiff>,
    /// Like `sig_diff`, for the public forms (private fields hidden), when
    /// they differ from the full ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig_diff_public: Option<SigDiff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_docs: Option<String>,
    /// The first line of the base version, for side-by-side diffs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_line: Option<usize>,
    /// The base version's file, when it differs (a moved item).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_file: Option<String>,
    pub source: Vec<SourceLine>,
    /// Like `info.source_refs`, for the base version's lines (by their
    /// line in the base file), when the source changed. For a removed
    /// item, `info` is the base version and has them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub old_source_refs: BTreeMap<String, Ref>,
    /// Lines of `source` that were only reformatted (rewrapped, as rustfmt
    /// does): the same tokens apart from trailing commas.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reformatted: Vec<Reformatted>,
    /// Blocks of `source`'s removed or added lines that were moved: they
    /// appear, the same apart from whitespace, among the added or removed
    /// lines of another item (e.g. an extracted helper) or elsewhere in
    /// this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moved_lines: Vec<MovedLines>,
    /// How the item's public API changed, if it is public on either side
    /// and its API changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiChange>,
    /// The `unsafe` code the item adds, changes or removes (see
    /// [`UnsafeChange`]). Empty if its `unsafe` code, if any, is unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsafe_code: Vec<UnsafeChange>,
}

/// A piece of `unsafe` code that a PR adds, changes or removes: an
/// `unsafe` block, function, impl, trait or `extern` block, or an
/// attribute that is unsafe to use (`#[unsafe(no_mangle)]`, `#[no_mangle]`,
/// `#[export_name]`, `#[link_section]`). Code inside a macro's arguments or
/// a `macro_rules!` body counts too, as far as its tokens show it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UnsafeChange {
    pub kind: UnsafeKind,
    /// `Added`, `Changed` or `Removed`.
    pub status: Status,
    /// What it is, as code: `unsafe fn read`, `unsafe impl Send for Buf`,
    /// `#[no_mangle]`; for a block, `unsafe { … }`.
    pub what: String,
    /// Its first and last line in the head's file (not for a removed one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<(u32, u32)>,
    /// Its first and last line in the base's file (not for an added one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_lines: Option<(u32, u32)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UnsafeKind {
    Block,
    Fn,
    Impl,
    Trait,
    Extern,
    Attr,
}

impl UnsafeKind {
    /// What the kind is called in a sentence: "`unsafe` block".
    pub fn label(self) -> &'static str {
        match self {
            UnsafeKind::Block => "unsafe block",
            UnsafeKind::Fn => "unsafe fn",
            UnsafeKind::Impl => "unsafe impl",
            UnsafeKind::Trait => "unsafe trait",
            UnsafeKind::Extern => "extern block",
            UnsafeKind::Attr => "unsafe attribute",
        }
    }
}

/// Whether a public API change is breaking, by Cargo's SemVer rules.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ApiChange {
    pub level: ApiLevel,
    /// Why, one entry per change, e.g. "variant `B` added: exhaustive
    /// `match`es outside the crate break". Code in backticks.
    pub reasons: Vec<String>,
    /// cargo-semver-checks lints that found what `reasons` already say,
    /// by id: they confirm the label without adding a reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmed_by: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ApiLevel {
    /// Needs a major version bump.
    Breaking,
    /// What the definitions show isn't enough to tell.
    Unknown,
    /// Needs at most a minor version bump.
    Compatible,
}

impl ApiLevel {
    pub fn label(self) -> &'static str {
        match self {
            ApiLevel::Breaking => "breaking",
            ApiLevel::Unknown => "check by hand",
            ApiLevel::Compatible => "non-breaking",
        }
    }

    pub fn css(self) -> &'static str {
        match self {
            ApiLevel::Breaking => "breaking",
            ApiLevel::Unknown => "unknown",
            ApiLevel::Compatible => "compatible",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Meta {
    pub title: String,
    pub base: String,
    pub head: String,
    pub url: Option<String>,
    /// How calls were resolved, e.g. "rust-analyzer" or "syntax only".
    pub resolution: String,
    /// The pull request, when one was analyzed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrMeta>,
    /// Without a pull request: which project was compared, as a hash of
    /// the repository's canonical path (`git`) or of both directories'
    /// (`dirs`). The viewer keeps review marks per project with it. Only a
    /// hash, so that the page doesn't contain the paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The commits between base and head, oldest first, with what each
    /// one touched. Empty without a git history (`dirs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commits: Vec<Commit>,
    /// The library crates' versions in `Cargo.toml`, by crate name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub versions: BTreeMap<String, CrateVersion>,
}

/// A library crate's version on both sides.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct CrateVersion {
    /// `None`: the crate is new, or its version can't be read.
    pub base: Option<String>,
    pub head: Option<String>,
    /// Whether the head's manifest allows publishing it.
    pub publish: bool,
    /// The base's manifest allows breaking changes without a major version
    /// bump (`allow-breaking-changes` under
    /// `[package.metadata.semantic-review]`). Taken from the base, so that
    /// a PR can't silence the warning about its own breaking change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_breaking: bool,
    /// The same setting in the head's manifest, to note when a PR changes
    /// it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_breaking_head: bool,
}

/// A commit of the compared range, and the changes of the page it made.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    /// The first line of its message.
    pub title: String,
    pub author: String,
    /// ISO 8601.
    pub date: String,
    /// More than one parent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub merge: bool,
    /// Changed items with a line that this commit added and that is still
    /// in the PR's version, or a line of the base that this commit
    /// removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
    /// Changed files the commit modified (relative to the analyzed
    /// directory), whether or not their change survives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
}

/// FNV-1a: the same value on every platform and with every Rust version
/// (unlike `std`'s hasher), for keys that are stored and compared later.
pub fn stable_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// A GitHub pull request and the revisions compared.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct PrMeta {
    /// `owner/repo`
    pub repo: String,
    pub number: u64,
    /// From the GitHub CLI, if it was available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The PR's description (Markdown), from the GitHub CLI; `None` if it
    /// is empty or `gh` wasn't available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The branch the PR targets, e.g. `main`.
    pub base_ref: String,
    /// The compared base commit (abbreviated): the merge base of the PR
    /// with `base_ref`, unless another base was given.
    pub base_rev: String,
    /// The compared base commit in full, for links to its lines. Empty for
    /// pages from before it was recorded.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_commit: String,
    /// The PR's branch, e.g. `fix-parser`, if known (only the GitHub CLI
    /// knows it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    /// The owner of the PR's branch when it comes from a fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_owner: Option<String>,
    /// The compared head commit (abbreviated).
    pub head_rev: String,
    /// The compared head commit in full, for a review posted to GitHub.
    /// Empty for pages from before it was recorded.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub head_commit: String,
    /// The base was given with `--base` instead of being the merge base,
    /// so the page's diff may not be the one GitHub shows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub custom_base: bool,
    /// The page shows only what changed since this earlier commit of the
    /// PR (in full), e.g. the one an earlier explorer page showed, leaving
    /// out what the base branch changed in between. `base_rev` is then
    /// that commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// The PR's review threads on GitHub when the page was built, open and
    /// resolved, in the order GitHub lists them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<ReviewThread>,
    /// Review marks that reviewers saved in their reviews of the PR (see
    /// [`REVIEW_MARKS_HEADER`]): the latest of each reviewer, so that a page
    /// built after new commits can offer them again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub saved_marks: Vec<SavedMarks>,
}

/// Review marks saved in a review of the PR.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedMarks {
    /// Who posted the review.
    pub login: String,
    /// When, as GitHub gives it (ISO 8601).
    pub at: String,
    pub url: String,
    /// The marks: the lines after [`REVIEW_MARKS_HEADER`].
    pub data: String,
}

/// The first line of the review marks that the viewer adds to a review it
/// posts, in a code block in a collapsed `<details>` at the end of the
/// review's text. One line per mark follows (see the viewer's `carry`).
pub const REVIEW_MARKS_HEADER: &str = "rust-pr-explorer review marks v1";

/// The lines of review marks in a review's text: those after
/// [`REVIEW_MARKS_HEADER`], up to the end of its code block.
pub fn review_marks_in(text: &str) -> Option<String> {
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    lines.find(|l| l.trim() == REVIEW_MARKS_HEADER)?;
    let marks: Vec<&str> = lines
        .take_while(|l| !l.trim_start().starts_with("```"))
        .filter(|l| !l.trim().is_empty())
        .collect();
    (!marks.is_empty()).then(|| marks.join("\n"))
}

/// What starts review marks copied to paste on another page, followed by
/// the marks as base64 JSON (with the reviewed versions and comments,
/// which a review leaves out).
pub const MARKS_MARKER: &str = "rust-pr-explorer-marks v1 ";

/// The base64 after [`MARKS_MARKER`] in a text, with any line breaks taken
/// out.
pub fn marks_in(text: &str) -> Option<String> {
    let start = text.find(MARKS_MARKER)? + MARKS_MARKER.len();
    let data: String = text[start..]
        .chars()
        .filter(|c| !c.is_whitespace())
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
        .collect();
    (!data.is_empty()).then_some(data)
}

/// A review thread of a pull request: comments on a line of a file.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewThread {
    /// The file, relative to the repository root.
    pub path: String,
    /// The line it is on in the PR's current diff: in the PR's version of
    /// the file, or in the base's with `left`. `None` when the thread is
    /// outdated (its line is gone) or on the whole file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// The first line, for a comment on several lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    /// The line in the diff the thread was started on, for an outdated one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_line: Option<usize>,
    /// On the base's version of the line (GitHub's `LEFT` side).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub left: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resolved: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub outdated: bool,
    /// The first comment on GitHub.
    #[serde(default)]
    pub url: String,
    pub comments: Vec<ThreadComment>,
}

/// One comment of a review thread.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadComment {
    /// GitHub login; empty for a deleted account.
    pub author: String,
    /// Markdown.
    pub body: String,
    /// ISO 8601, e.g. `2026-11-23T15:33:01Z`.
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Output {
    pub meta: Meta,
    /// Crate root ids, the main crate first.
    pub crates: Vec<String>,
    pub items: BTreeMap<String, Item>,
    /// Every file the PR changes, including those the Rust analysis
    /// doesn't cover.
    #[serde(default)]
    pub files: Vec<ChangedFile>,
    /// Items outside the analyzed crates (the standard library and
    /// dependencies) that the analyzed code names, by the path it uses.
    #[serde(default)]
    pub externals: BTreeMap<String, External>,
    pub warnings: Vec<String>,
    /// What cargo-semver-checks found, if it was run (`--semver-checks`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semver_checks: Option<SemverChecks>,
}

/// A run of cargo-semver-checks on the two versions. Its findings are
/// folded into the items' SemVer labels; `findings` keeps all of them,
/// with the item each one was matched to.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SemverChecks {
    /// Its version, e.g. `0.51.0`.
    pub version: String,
    /// Why it couldn't check the crates (e.g. the code doesn't build).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Which features it built with: "most features" (its default) or,
    /// if the crates don't build that way, "default features".
    #[serde(default)]
    pub features: String,
    /// Lints run, and how long the run took (both sides' rustdoc builds
    /// included).
    pub checks: u32,
    pub seconds: f32,
    pub findings: Vec<SemverFinding>,
}

/// One result of one cargo-semver-checks lint.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SemverFinding {
    /// The lint's id, e.g. `enum_variant_added`.
    pub lint: String,
    /// The lint's name, e.g. "enum variant added on exhaustive enum".
    pub title: String,
    /// Major lints are breaking, minor ones non-breaking.
    pub level: ApiLevel,
    /// What it found, without the file location, e.g. "variant
    /// Error:DivisionByZero".
    pub message: String,
    /// The Cargo reference section the lint is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The item it was matched to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// Where it points, relative to the checkout, e.g. `src/lib.rs:21`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

/// An item outside the analyzed crates, as rust-analyzer describes it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct External {
    pub kind: Kind,
    /// Where it is defined, e.g. `core::result` for `std::result::Result`.
    pub module: String,
    pub name: String,
    /// Its declaration as rust-analyzer shows it (rustdoc style).
    pub sig: String,
    /// Doc comment (Markdown); intra-doc links point to the web.
    pub docs: String,
    /// Its documentation on docs.rs or doc.rust-lang.org.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A file the PR adds, removes or changes.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChangedFile {
    /// Path relative to the repository root.
    pub path: String,
    pub status: Status,
    /// `rust`, `python`, `markdown`, `toml`, `yaml`, `css`, `binary`,
    /// `symlink` or `text`.
    pub lang: String,
    /// Part of the Rust analysis (shown as items, not as a file).
    #[serde(default)]
    pub analyzed: bool,
    /// For an analyzed file: changed lines outside every item (unnamed
    /// macro invocations, `extern` blocks, test code, …), which the items
    /// don't show.
    #[serde(default)]
    pub outside_items: u32,
    /// Why the file counts as generated or vendored, if it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<String>,
    pub added: u32,
    pub removed: u32,
    /// The changed lines with 3 lines of context. `@` lines start a hunk
    /// and hold "old_line new_line". Empty for binary or very large files.
    /// For an analyzed file, only the changes outside every item (the
    /// items show the others).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hunks: Vec<SourceLine>,
    /// For an analyzed file: the line numbers of all its added lines (in
    /// the PR's version) and removed lines (in the base), including those
    /// inside items, which `hunks` leaves out. A review comment on an item
    /// is posted to GitHub on one of them, since GitHub only takes
    /// comments on lines its diff shows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_lines: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_lines: Vec<u32>,
    /// The file was too large to diff line by line.
    #[serde(default)]
    pub too_large: bool,
    /// Structured changes of a `Cargo.toml`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manifest: Vec<ManifestChange>,
    /// The packages whose locked versions changed, for `Cargo.lock`,
    /// `uv.lock` and `poetry.lock`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lock: Vec<LockChange>,
    /// The file's changed parts, each shown as a box: functions and
    /// classes of a Python file, keys of a YAML file, and for an analyzed
    /// Rust file the code that belongs to no item (tests, macro calls,
    /// `extern` blocks).
    #[serde(default, skip_serializing_if = "Vec::is_empty", alias = "python")]
    pub parts: Vec<PartChange>,
    /// What changed in a CSS file, selector by selector.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub styles: Vec<StyleChange>,
    /// Both versions of a Markdown file, compared section by section in
    /// the viewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_text: Option<String>,
    /// Hashes of the file's whole content in the base and in the head
    /// (see [`content_hash`]), empty for a side without the file. They
    /// identify a version of the file across pages that compare it with
    /// different revisions, for sharing review marks between them.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub old_hash: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub new_hash: String,
}

/// Hash of a file's content, as hex: FNV-1a like [`stable_hash`], over the
/// raw bytes.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// One entry of a `Cargo.toml` that changed.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestChange {
    /// e.g. `dependencies`, `dev-dependencies`,
    /// `target.'cfg(windows)'.dependencies`, `features`, `package`.
    pub section: String,
    pub name: String,
    /// The entry as written before and after, `None` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
}

/// A package of a lock file whose locked versions, source or checksum
/// changed.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockChange {
    pub name: String,
    /// The locked versions before and after: usually one, several when
    /// the package is locked in more than one version, none when absent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub old: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new: Vec<String>,
    /// Where it comes from, when that changed (or for an added package),
    /// unless that is the default registry (crates.io, PyPI): a git
    /// repository, another registry, a path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_source: Option<String>,
    /// A version that is locked on both sides, from the same source, has
    /// another checksum: the published package itself differs.
    #[serde(default)]
    pub checksum_changed: bool,
    /// For `Cargo.lock`: how the package is used, in the head (in the
    /// base for a removed package).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dep: Option<DepKind>,
    /// For `Cargo.lock`: the packages that depend on it, in the head (in
    /// the base for a removed package).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependents: Vec<String>,
    /// The package comes from crates.io (in the head, or in the base for a
    /// removed package), so diff.rs, docs.rs and crates.io have it.
    #[serde(default)]
    pub crates_io: bool,
    /// What its manifest says, from the package's sources that Cargo
    /// downloaded for the analysis. Absent when they aren't there (e.g.
    /// with `--syntax-only`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<CrateInfo>,
}

/// How a locked package is used.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DepKind {
    /// A package of the workspace, or a path dependency.
    Local,
    /// A dependency of a local package.
    Direct,
    /// Only a dependency of other dependencies.
    Transitive,
}

/// A crate's metadata, from the manifest of its newest locked version.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CrateInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The `repository` URL of its manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The changelog file in the published package, e.g. `CHANGELOG.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
    /// It has a build script, which runs at build time.
    #[serde(default)]
    pub build_script: bool,
    /// It is a procedural macro, which runs at build time.
    #[serde(default)]
    pub proc_macro: bool,
}

/// A selector of a stylesheet whose properties changed. Its properties
/// are those of every rule for it in the file (in the same context),
/// merged by the cascade: what applies to an element it matches, as far as
/// this file decides.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StyleChange {
    /// The selector, normalized (`.a > .b`). Several when they changed in
    /// the same way, as the selectors of one rule `h1, h2 { … }` do.
    /// Nested rules are resolved (`.a { &:hover { … } }` is `.a:hover`).
    /// For a statement (`@import …;`), the statement.
    pub selectors: Vec<String>,
    /// The at-rules around it, outermost first: `@media (width < 600px)`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<String>,
    pub status: Status,
    /// A statement at-rule (`@import`, `@charset`, `@layer a, b;`), which
    /// has no properties.
    #[serde(default)]
    pub statement: bool,
    /// First line of each rule for the selector, in the head version (base
    /// for a removed one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<usize>,
    /// Properties added, removed or changed, and those whose declarations
    /// changed while the value that applies didn't (with a `note`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<PropChange>,
    /// The selector's other properties, as they apply in the head version
    /// (`new` only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unchanged: Vec<PropChange>,
}

/// One property of a selector, before and after.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PropChange {
    /// `color`, `--accent`.
    pub name: String,
    /// The value that applies, `None` if the property isn't set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<PropValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<PropValue>,
    /// Why the declarations changed without effect, e.g. "Line 12 sets
    /// `red`, but line 20 overrides it: no effect."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A property's value (whitespace normalized, `!important` included) and
/// the line of the declaration it comes from.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PropValue {
    pub value: String,
    pub line: usize,
}

/// A part of a file that changed: a Python function or class, a YAML
/// key, or a piece of Rust code outside the analyzed items.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartChange {
    /// Qualified name: for Python as `__qualname__` (`Class.method`,
    /// `outer.<locals>.inner`) or `<module>` for top-level code, for YAML
    /// the key path (`jobs.test`), for Rust the path in the file
    /// (`tests::adds`).
    pub name: String,
    /// Python: `def`, `async def`, `class` or `module`. YAML: `key`. Rust:
    /// `test`, `bench`, `fn`, `struct`, `impl`, `macro call`, `extern`, ….
    pub kind: String,
    pub status: Status,
    /// The header (`def f(a, b) -> int`, `#[test] fn adds()`), decorators
    /// and attributes included.
    pub sig: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_sig: Option<String>,
    #[serde(default)]
    pub doc_changed: bool,
    #[serde(default)]
    pub body_changed: bool,
    /// Only whitespace or comments differ.
    #[serde(default)]
    pub cosmetic: bool,
    /// First line in the head version (base for removed ones).
    pub line: usize,
    /// Diff of the definition's own lines (nested definitions are
    /// entries of their own).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<SourceLine>,
    /// For Rust code: the `unsafe` code it adds, changes or removes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsafe_code: Vec<UnsafeChange>,
}
