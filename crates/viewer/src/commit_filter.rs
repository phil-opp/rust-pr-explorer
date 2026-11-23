//! Showing only what one commit of the PR changed, like GitHub's view of a
//! single commit: a menu in the header picks the commit, and every page
//! then lists only the items and files it touched.
//!
//! Which items a commit touched comes from the analysis (see
//! `Commit::items`). The items are still shown with their whole change,
//! from the PR's base to its head: the page holds no other versions.

use crate::code::esc;
use crate::data::Data;
use model::Commit;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

struct Filter {
    /// Index in `Meta::commits`.
    index: usize,
    items: HashSet<String>,
    files: HashSet<String>,
    /// `Data::touched` under the filter, once computed.
    touched: Option<HashMap<String, bool>>,
}

thread_local! {
    static FILTER: RefCell<Option<Filter>> = const { RefCell::new(None) };
}

/// Whether an item passes the filter (always without one).
pub fn item_in(id: &str) -> bool {
    FILTER.with(|f| f.borrow().as_ref().is_none_or(|f| f.items.contains(id)))
}

/// Whether a changed file passes the filter (always without one).
pub fn file_in(path: &str) -> bool {
    FILTER.with(|f| f.borrow().as_ref().is_none_or(|f| f.files.contains(path)))
}

/// `Data::touched` under the active filter, if there is one.
pub fn touched(id: &str) -> Option<bool> {
    FILTER.with(|f| {
        let f = f.borrow();
        let f = f.as_ref()?;
        Some(f.touched.as_ref()?.get(id).copied().unwrap_or(false))
    })
}

/// The selected entry of the menu, if any: a commit, or (past the last
/// commit) the changed items no commit was found for.
fn selected() -> Option<usize> {
    FILTER.with(|f| f.borrow().as_ref().map(|f| f.index))
}

/// The selected commit, if any.
pub fn current(d: &Data) -> Option<&Commit> {
    d.out.meta.commits.get(selected()?)
}

/// Changed items that no commit's lines account for, such as those whose
/// only change is a public path that a `pub use` elsewhere gave or took.
fn untraced(d: &Data) -> Vec<String> {
    let traced: HashSet<&str> = d
        .out
        .meta
        .commits
        .iter()
        .flat_map(|c| c.items.iter().map(String::as_str))
        .collect();
    d.out
        .items
        .values()
        .filter(|it| it.info.kind != model::Kind::Mod && it.status != model::Status::Unchanged)
        .filter(|it| !traced.contains(it.info.id.as_str()))
        .map(|it| it.info.id.clone())
        .collect()
}

/// Shows only what menu entry `index` stands for, or everything with
/// `None`.
pub fn set(d: &Data, index: Option<usize>) {
    let commits = &d.out.meta.commits;
    let (items, files): (HashSet<String>, HashSet<String>) = match index {
        Some(i) if i < commits.len() => (
            commits[i].items.iter().cloned().collect(),
            commits[i].files.iter().cloned().collect(),
        ),
        Some(i) if i == commits.len() && !commits.is_empty() => {
            (untraced(d).into_iter().collect(), HashSet::new())
        }
        _ => {
            FILTER.with(|f| *f.borrow_mut() = None);
            return;
        }
    };
    FILTER.with(|f| {
        *f.borrow_mut() = Some(Filter {
            index: index.unwrap_or_default(),
            items,
            files,
            touched: None,
        })
    });
    // With the filter in place, `changed` only counts the commit's items.
    let touched = d.compute_all_touched();
    FILTER.with(|f| {
        if let Some(f) = f.borrow_mut().as_mut() {
            f.touched = Some(touched);
        }
    });
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// "3 items, 1 file".
fn counts(c: &Commit) -> String {
    let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    match (c.items.len(), c.files.len()) {
        (0, 0) => "nothing on this page".to_string(),
        (i, f) => format!("{}, {}", plural(i, "item"), plural(f, "file")),
    }
}

/// The commit menu for the header; empty without commits.
pub fn menu_html(d: &Data) -> String {
    let list = &d.out.meta.commits;
    if list.is_empty() {
        return String::new();
    }
    let options: String = list
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut title: String = c.title.chars().take(70).collect();
            if title.len() < c.title.len() {
                title.push('…');
            }
            format!(
                r#"<option value="{i}">{} {}{} ({})</option>"#,
                short(&c.sha),
                esc(&title),
                if c.merge { " [merge]" } else { "" },
                counts(c)
            )
        })
        .collect();
    let untraced = untraced(d).len();
    let options = if untraced == 0 {
        options
    } else {
        format!(
            r#"{options}<option value="{}">Changes no commit was found for ({untraced} item{})</option>"#,
            list.len(),
            if untraced == 1 { "" } else { "s" }
        )
    };
    format!(
        r#"<label class="commit-pick" title="Show only what one commit changed"><span class="dim">Commits</span> <select id="commit-filter"><option value="">All {} commits</option>{options}</select></label>"#,
        list.len()
    )
}

/// The note at the top of each page while a commit is selected.
pub fn banner_html(d: &Data) -> String {
    if selected().is_none() {
        return String::new();
    }
    let Some(c) = current(d) else {
        let n = untraced(d).len();
        return format!(
            r#"<div class="commit-banner"><div><b>Changes no commit was found for</b></div><div class="dim">{n} changed item{} whose changed lines no commit accounts for, such as items whose only change is a public path that a <code>pub use</code> elsewhere added or removed.</div><button class="mini" data-commit-clear>Show all commits</button></div>"#,
            if n == 1 { "" } else { "s" }
        );
    };
    let what = if c.merge {
        "This is a merge commit; its changes are not traced, so nothing is listed.".to_string()
    } else {
        format!(
            "Showing only what this commit changed: {}. Items show their whole change in the PR, not only this commit's part.",
            counts(c)
        )
    };
    format!(
        r#"<div class="commit-banner"><div><b>Commit <code>{}</code></b> {} <span class="dim">· {} · {}</span></div><div class="dim">{what}</div><button class="mini" data-commit-clear>Show all commits</button></div>"#,
        short(&c.sha),
        esc(&c.title),
        esc(&c.author),
        esc(c.date.get(..10).unwrap_or(&c.date)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Item, Kind, Output, Status};

    fn item(id: &str, kind: Kind, status: Status, module: &str, parent: Option<&str>) -> Item {
        let mut it = Item::default();
        it.info.id = id.to_string();
        it.info.kind = kind;
        it.info.module = module.to_string();
        it.info.parent = parent.map(str::to_string);
        it.status = status;
        it
    }

    #[test]
    fn a_commit_shows_only_its_items() {
        let items = vec![
            item("c", Kind::Mod, Status::Changed, "c", None),
            {
                let mut t = item("c::T", Kind::Struct, Status::Unchanged, "c", None);
                t.info.children = vec!["c::T::m".to_string()];
                t
            },
            item("c::T::m", Kind::Method, Status::Changed, "c", Some("c::T")),
            item("c::f", Kind::Fn, Status::Changed, "c", None),
        ];
        let mut out = Output {
            meta: Default::default(),
            crates: vec!["c".to_string()],
            items: items
                .into_iter()
                .map(|it| (it.info.id.clone(), it))
                .collect(),
            files: Vec::new(),
            externals: Default::default(),
            warnings: Vec::new(),
        };
        out.meta.commits = vec![Commit {
            sha: "0123456789abcdef".to_string(),
            title: "Change m".to_string(),
            items: vec!["c::T::m".to_string()],
            files: vec!["src/lib.rs".to_string()],
            ..Default::default()
        }];
        let d = Data::new(out);
        let get = |id: &str| d.item(id).unwrap();
        assert!(crate::data::changed(get("c::f")));
        set(&d, Some(0));
        assert!(!crate::data::changed(get("c::f")));
        assert!(crate::data::changed(get("c::T::m")));
        assert!(d.touched(get("c::T")) && !d.touched(get("c::f")));
        assert!(file_in("src/lib.rs") && !file_in("README.md"));
        assert!(banner_html(&d).contains("1 item, 1 file"));
        assert!(menu_html(&d).contains("0123456 Change m"));
        // `c::f` is in no commit.
        assert!(menu_html(&d).contains("Changes no commit was found for (1 item)"));
        set(&d, Some(1));
        assert!(crate::data::changed(get("c::f")) && !crate::data::changed(get("c::T::m")));
        set(&d, None);
        assert!(crate::data::changed(get("c::f")) && file_in("README.md"));
        assert!(banner_html(&d).is_empty());
    }
}
