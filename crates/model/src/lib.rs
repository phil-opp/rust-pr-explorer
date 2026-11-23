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
    /// The branch the PR targets, e.g. `main`.
    pub base_ref: String,
    /// The compared base commit (abbreviated): the merge base of the PR
    /// with `base_ref`, unless another base was given.
    pub base_rev: String,
    /// The PR's branch, e.g. `fix-parser`, if known (only the GitHub CLI
    /// knows it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    /// The owner of the PR's branch when it comes from a fork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_owner: Option<String>,
    /// The compared head commit (abbreviated).
    pub head_rev: String,
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
    /// `rust`, `python`, `markdown`, `toml`, `yaml`, `binary`, `symlink` or
    /// `text`.
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hunks: Vec<SourceLine>,
    /// The file was too large to diff line by line.
    #[serde(default)]
    pub too_large: bool,
    /// Structured changes of a `Cargo.toml`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub manifest: Vec<ManifestChange>,
    /// Changed definitions of a Python file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub python: Vec<PyChange>,
    /// Both versions of a Markdown file, compared section by section in
    /// the viewer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_text: Option<String>,
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

/// A Python function or class that changed.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PyChange {
    /// Qualified name, as Python's `__qualname__` (`Class.method`,
    /// `outer.<locals>.inner`), or `<module>` for top-level code.
    pub name: String,
    /// `def`, `async def`, `class` or `module`.
    pub kind: String,
    pub status: Status,
    /// The header (`def f(a, b) -> int`), decorators included.
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
}
