//! Turns one source tree into a flat map of items with resolved names,
//! rustdoc-style signatures and call/use edges.

use crate::source::CrateSrc;
use model::{ItemInfo, Kind, Ref, UseCtx};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use syn::{Attribute, Item, Visibility, parse_quote};
#[cfg(feature = "semantic")]
pub use syntax::wrap_fn_sig;
pub use syntax::{extract_docs, is_extra_target};

mod collector;
mod items;
mod load;
mod public;
mod resolve;
mod syntax;

/// A position in a source file: 1-based line, 0-based column in chars
/// (the convention of `proc_macro2`).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct LineCol {
    pub line: usize,
    pub column: usize,
}

impl From<proc_macro2::LineColumn> for LineCol {
    fn from(lc: proc_macro2::LineColumn) -> Self {
        LineCol {
            line: lc.line,
            column: lc.column,
        }
    }
}

/// A place in a function body that looks like a call.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CallSite {
    /// Position of the called name (the method name, or the last segment
    /// of a path).
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    pub pos: LineCol,
    pub method: bool,
    /// Inside a macro invocation, where the syntax alone doesn't prove that
    /// the code runs (or runs as a call).
    pub in_macro: bool,
    /// The path's target is known (maybe outside the analyzed crates).
    pub known: bool,
    /// For path calls outside macros: the function the path names, by
    /// Rust's name resolution rules.
    pub target: Option<String>,
}

/// A name space of Rust paths: a name can refer to a type and to a value
/// at the same time (e.g. `mod foo` and `fn foo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ns {
    Type,
    Value,
}

/// One item of one side (base or head) of the pull request.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ItemData {
    pub info: ItemInfo,
    /// Signature text plus the targets of all names in it.
    pub sig_norm: String,
    /// Canonical body text plus the targets of all names in it.
    pub body_norm: String,
    /// The item's source text.
    pub source: String,
    /// Where the item's name is, for named items other than modules and
    /// impls.
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    pub name_pos: Option<LineCol>,
    /// Marked `#[doc(hidden)]`: rustdoc leaves it out, and it is not part
    /// of the public API.
    pub doc_hidden: bool,
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    pub call_sites: Vec<CallSite>,
    /// For each path outside the analyzed crates, where one occurrence
    /// names it (the position of its last segment).
    #[cfg_attr(not(feature = "semantic"), allow(dead_code))]
    pub ext_sites: BTreeMap<String, LineCol>,
    /// The item's own `unsafe` code (not its members', which are items of
    /// their own).
    pub unsafe_sites: Vec<crate::unsafety::Site>,
}

/// Serializable, so that a later run can reuse it (see `crate::reuse`).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Analysis {
    pub items: BTreeMap<String, ItemData>,
    /// Items outside the analyzed crates that the code names, as
    /// rust-analyzer describes them.
    pub externals: BTreeMap<String, model::External>,
    pub crates: Vec<String>,
    pub warnings: Vec<String>,
}

struct SrcFile {
    rel: String,
    lines: Vec<String>,
}

#[derive(Default)]
struct ModData {
    parent: Option<String>,
    defs: HashMap<String, Vec<String>>,
    uses: Vec<UseEntry>,
}

struct UseEntry {
    /// `None` for glob imports.
    alias: Option<String>,
    path: Vec<String>,
    public: bool,
    /// The `use` is marked `#[doc(hidden)]`: the name it adds is not part
    /// of the public API.
    doc_hidden: bool,
}

enum Node {
    Item(Item),
    Mod {
        attrs: Vec<Attribute>,
        vis: Visibility,
    },
    ImplFn(syn::ImplItemFn),
    ImplConst(syn::ImplItemConst),
    ImplType(syn::ImplItemType),
    TraitFn(syn::TraitItemFn),
    TraitConst(syn::TraitItemConst),
    TraitType(syn::TraitItemType),
}

struct Pending {
    id: String,
    name: String,
    module: String,
    file: Rc<SrcFile>,
    node: Node,
    /// Owning type/trait/impl item.
    parent: Option<String>,
    /// Item id that `Self` refers to.
    self_ty: Option<String>,
    impl_ctx: Option<Rc<ImplCtx>>,
    /// Generic parameters of the enclosing impl or trait.
    outer_generics: Vec<String>,
}

struct ImplCtx {
    header: String,
    trait_ref: Option<Ref>,
    /// Local self type, if any.
    self_ty: Option<String>,
    /// Visibility parent of the members.
    vis_parent: Option<String>,
    /// The impl is test-only (`#[cfg(test)]`, or in a test-only module).
    test_only: bool,
}

/// Output of [`Analyzer::render_node`].
struct Rendered {
    attrs: Vec<Attribute>,
    vis: Visibility,
    sig: String,
    sig_public: Option<String>,
    body_norm: String,
    span: Option<proc_macro2::Span>,
    use_ctx: UseCtx,
}

struct RawImpl {
    module: String,
    file: Rc<SrcFile>,
    item: syn::ItemImpl,
}

type LookupKey = (String, String, Option<Ns>);

#[derive(Clone)]
struct Res {
    id: Option<String>,
    path: String,
    /// Index of the first segment to keep when displaying the path.
    display_start: usize,
}

pub struct Analyzer {
    root: PathBuf,
    /// The canonical root, so that no file outside the checkout is read.
    canonical_root: PathBuf,
    /// Per (crate, canonical file): the module ids it was loaded as, so
    /// that a `#[path]` cycle (a file inside its own module) ends.
    loaded: HashMap<(String, PathBuf), Vec<String>>,
    /// Per crate: modules read from a file the crate had already loaded.
    /// Capped, because each such file can declare its modules again, so
    /// that their number can double with every file (see `load_file`).
    repeated_loads: HashMap<String, usize>,
    /// Name lookups, memoized (see `lookup_name`).
    lookup_memo: std::cell::RefCell<HashMap<LookupKey, Option<Res>>>,
    lookup_active: std::cell::RefCell<std::collections::HashSet<LookupKey>>,
    lookup_cut: std::cell::Cell<bool>,
    modules: HashMap<String, ModData>,
    kinds: HashMap<String, Kind>,
    pending: Vec<Pending>,
    impls: Vec<RawImpl>,
    /// Type or trait id → associated items as (name, id).
    assoc: HashMap<String, Vec<(String, String)>>,
    enum_variants: HashMap<String, Vec<String>>,
    /// Declared visibility scope of module-level definitions (`None` = pub),
    /// to know what a glob import brings in.
    def_vis: HashMap<String, Option<String>>,
    /// Tuple and unit structs, which are also values (constructors).
    value_structs: std::collections::HashSet<String>,
    /// Per crate root: dependency names and edition.
    crate_info: HashMap<String, (BTreeSet<String>, Option<u16>)>,
    children: HashMap<String, Vec<String>>,
    crate_roots: BTreeSet<String>,
    /// Test-only modules, traits and impls (`#[cfg(test)]`, or inside a
    /// test-only module) of libraries and binaries.
    test_ids: std::collections::HashSet<String>,
    warnings: Vec<String>,
}

impl Analyzer {
    pub fn run(root: &Path, crates: &[CrateSrc]) -> Analysis {
        let mut an = Analyzer {
            root: root.to_path_buf(),
            canonical_root: std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()),
            loaded: Default::default(),
            repeated_loads: Default::default(),
            lookup_memo: Default::default(),
            lookup_active: Default::default(),
            lookup_cut: Default::default(),
            modules: HashMap::new(),
            kinds: HashMap::new(),
            pending: Vec::new(),
            impls: Vec::new(),
            assoc: HashMap::new(),
            enum_variants: HashMap::new(),
            def_vis: HashMap::new(),
            value_structs: Default::default(),
            crate_info: HashMap::new(),
            children: HashMap::new(),
            crate_roots: BTreeSet::new(),
            test_ids: Default::default(),
            warnings: Vec::new(),
        };
        let mut crate_order = Vec::new();
        for krate in crates {
            if !an.crate_roots.insert(krate.name.clone()) {
                an.warnings.push(format!(
                    "{} is left out: another crate is also named `{}`",
                    krate
                        .root_file
                        .strip_prefix(root)
                        .unwrap_or(&krate.root_file)
                        .display(),
                    krate.name
                ));
                continue;
            }
            crate_order.push(krate.name.clone());
            an.crate_info.insert(
                krate.name.clone(),
                (krate.deps.iter().cloned().collect(), krate.edition),
            );
            match an.load_file(&krate.root_file, &krate.name).map_err(|e| e.0) {
                Ok((file, src)) => {
                    let dir = krate.root_file.parent().unwrap().to_path_buf();
                    an.kinds.insert(krate.name.clone(), Kind::Mod);
                    an.modules.insert(krate.name.clone(), ModData::default());
                    an.pending.push(Pending {
                        id: krate.name.clone(),
                        name: krate.name.clone(),
                        module: krate.name.clone(),
                        file: src.clone(),
                        node: Node::Mod {
                            attrs: file.attrs.clone(),
                            vis: parse_quote!(pub),
                        },
                        parent: None,
                        self_ty: None,
                        impl_ctx: None,
                        outer_generics: Vec::new(),
                    });
                    an.collect_items(&krate.name, &file.items, &src, &dir);
                }
                Err(e) => an.warnings.push(e),
            }
        }
        an.collect_impls();
        let mut items = an.build_items();
        // Binaries, tests, examples, … have no public API.
        for item in items.values_mut() {
            if is_extra_target(&item.info.id) {
                item.info.public = false;
                item.info.public_paths.clear();
            }
        }
        Analysis {
            externals: BTreeMap::new(),
            items,
            crates: crate_order,
            warnings: an.warnings,
        }
    }
}
