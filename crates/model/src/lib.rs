//! The analysis result that the command-line tool embeds in the explorer
//! page and the viewer reads back.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
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
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemInfo {
    pub id: String,
    pub kind: Kind,
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
    /// Targets of method calls by position (`line:column` of the method
    /// name, both as in the source file, column 0-based in chars).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub site_refs: BTreeMap<String, Ref>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Added,
    Removed,
    Changed,
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
#[derive(Serialize, Deserialize, Debug, Clone)]
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
    pub source: Vec<SourceLine>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Meta {
    pub title: String,
    pub base: String,
    pub head: String,
    pub url: Option<String>,
    /// How calls were resolved, e.g. "rust-analyzer" or "syntax only".
    pub resolution: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Output {
    pub meta: Meta,
    /// Crate root ids, the main crate first.
    pub crates: Vec<String>,
    pub items: BTreeMap<String, Item>,
    pub warnings: Vec<String>,
}
