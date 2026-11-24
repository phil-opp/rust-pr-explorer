//! "Review first": the overview's short list of the changes most likely to
//! break something, ranked by reasons a reviewer can check: `unsafe` code,
//! the SemVer label, how much code calls or uses the item, and whether a
//! test reaches it.
//!
//! It is a list of its own above the overview's sections, not a new order
//! for them: those follow the module tree, as rustdoc does, and keep their
//! places from one review to the next.

use crate::code::esc;
use crate::data::{Data, changed};
use model::{ApiLevel, Item, Kind, Status};
use std::collections::{HashSet, VecDeque};

/// Items shown before "N more".
const SHOWN: usize = 8;
/// Callers and users from which an item counts as widely used.
const MANY: usize = 3;
/// …and as very widely used, which ranks above a test gap.
const VERY_MANY: usize = 10;

/// Why an item is risky.
pub struct Risk<'a> {
    pub it: &'a Item,
    /// Adds or changes `unsafe` code.
    unsafe_code: bool,
    api: Option<ApiLevel>,
    /// Items of the analyzed code that call or use it.
    users: usize,
    /// No test reaches it through resolved calls, uses and names. `None` when the
    /// analyzed code has no tests, so the question doesn't apply.
    untested: Option<bool>,
}

impl Risk<'_> {
    fn key(&self) -> (bool, u8, u8, bool, usize) {
        let api = match self.api {
            Some(ApiLevel::Breaking) => 2,
            Some(ApiLevel::Unknown) => 1,
            _ => 0,
        };
        let users = match self.users {
            n if n >= VERY_MANY => 2,
            n if n >= MANY => 1,
            _ => 0,
        };
        (
            self.unsafe_code,
            api,
            users,
            self.untested == Some(true),
            self.users,
        )
    }

    /// Whether it is worth listing: a test gap alone is not, since in most
    /// projects that would list most changes.
    fn listed(&self) -> bool {
        self.unsafe_code
            || matches!(self.api, Some(ApiLevel::Breaking | ApiLevel::Unknown))
            || self.users >= MANY
    }
}

/// The items that call or use `id` (removed ones left out).
fn users(d: &Data, id: &str) -> Vec<String> {
    let callers = d.callers.get(id).into_iter().flatten();
    let users = d.used_by.get(id).into_iter().flatten().map(|(u, _)| u);
    let mut v: Vec<String> = callers
        .chain(users)
        .filter(|u| *u != id)
        .filter(|u| d.item(u).is_some_and(|it| it.status != Status::Removed))
        .cloned()
        .collect();
    v.sort();
    v.dedup();
    v
}

/// The items that test functions reach: what they call or use, or name
/// in their code, directly or through other items. Names count because
/// tests call mostly inside macros (`assert_eq!(calc(…), …)`), where calls
/// often stay unresolved but the path still names its item.
fn tested(d: &Data) -> HashSet<&str> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = d
        .items()
        .filter(|it| it.info.test)
        .map(|it| it.info.id.as_str())
        .collect();
    while let Some(cur) = queue.pop_front() {
        let Some(it) = d.item(cur) else { continue };
        let next = it
            .info
            .calls
            .iter()
            .map(|c| c.to.as_str())
            .chain(it.info.uses.iter().map(|u| u.to.as_str()))
            .chain(it.info.source_refs.values().filter_map(|r| r.id.as_deref()));
        for n in next {
            if d.item(n).is_some() && seen.insert(n) {
                queue.push_back(n);
            }
        }
    }
    seen
}

/// The changed items worth reviewing first, riskiest first.
pub fn ranked(d: &Data) -> Vec<Risk<'_>> {
    let has_tests = d.items().any(|it| it.info.test);
    let tested = tested(d);
    let mut list: Vec<Risk> = d
        .items()
        .filter(|it| changed(it) && it.info.kind != Kind::Mod && !it.info.test)
        .filter(|it| d.external(&it.info.id).is_none())
        .map(|it| {
            let live = it.status != Status::Removed;
            Risk {
                it,
                unsafe_code: it.unsafe_code.iter().any(|c| c.status != Status::Removed),
                api: it.api.as_ref().map(|a| a.level),
                users: if live { users(d, &it.info.id).len() } else { 0 },
                untested: (has_tests && live && it.info.kind.is_callable() && !it.info.test_only)
                    .then(|| !tested.contains(it.info.id.as_str())),
            }
        })
        .filter(Risk::listed)
        .collect();
    list.sort_by(|a, b| {
        b.key()
            .cmp(&a.key())
            .then_with(|| d.reading_order(a.it, b.it))
    });
    list
}

fn row_html(d: &Data, r: &Risk) -> String {
    let it = r.it;
    let mut why = Vec::new();
    why.push(crate::unsafety::tag(it));
    if matches!(r.api, Some(ApiLevel::Breaking | ApiLevel::Unknown)) {
        why.push(d.api_badge(it));
    }
    if r.users >= MANY {
        why.push(format!(
            r#"<span class="risk-chip" title="Items of the analyzed code that call or use it">{} callers and users</span>"#,
            r.users
        ));
    }
    if r.untested == Some(true) {
        why.push(
            r#"<span class="risk-chip" title="No test function calls, uses or names it, directly or through other items, as far as calls were resolved">no test reaches it</span>"#
                .to_string(),
        );
    }
    format!(
        r#"<li class="risk-row"><span class="kind">{}</span><a class="ref" data-id="{}" data-kind="{}" data-path="{}">{}</a> <span class="dim">in {}</span>{} {}</li>"#,
        it.info.kind.label(),
        esc(&it.info.id),
        it.info.kind.css(),
        esc(&it.info.id),
        crate::review::item_tag(&it.info.id, &d.display_name(it)),
        esc(&d.mod_label(d.mod_of(it))),
        crate::data::test_tag_of(it),
        why.concat(),
    )
}

/// The overview's "Review first" list. Empty unless at least two changes
/// stand out, since ranking one item says nothing.
pub fn overview_html(d: &Data) -> String {
    let list = ranked(d);
    if list.len() < 2 {
        return String::new();
    }
    let rows: Vec<String> = list.iter().map(|r| row_html(d, r)).collect();
    let (shown, rest) = rows.split_at(rows.len().min(SHOWN));
    let more = if rest.is_empty() {
        String::new()
    } else {
        format!(
            r#"<details class="risk-more"><summary>{} more</summary><ol class="risk-list" start="{}">{}</ol></details>"#,
            rest.len(),
            SHOWN + 1,
            rest.concat()
        )
    };
    format!(
        r#"<h2>Review first <span class="h-note">{} change{} ranked by risk</span> {}</h2><ol class="risk-list">{}</ol>{more}"#,
        list.len(),
        if list.len() == 1 { "" } else { "s" },
        crate::pages::help_html(
            "Changes that add or change unsafe code come first, then breaking API changes, then those the SemVer rules can't settle, then those that much of the analyzed code calls or uses (3 or more). Within that order, changes that no test reaches come first. Only resolved calls and uses count, and callers outside the crate are unknown. The sections below keep their module order."
        ),
        shown.concat(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::{ApiChange, Call, Output, UnsafeChange, UnsafeKind};

    fn item(id: &str, status: Status) -> Item {
        let mut it = Item::default();
        it.info.id = id.into();
        it.info.name = id.rsplit("::").next().unwrap().into();
        it.info.module = "c".into();
        it.info.kind = Kind::Fn;
        it.info.file = "src/lib.rs".into();
        it.status = status;
        it
    }

    fn data(items: Vec<Item>) -> Data {
        let mut root = item("c", Status::Unchanged);
        root.info.kind = Kind::Mod;
        let mut out = Output {
            meta: Default::default(),
            crates: vec!["c".into()],
            items: Default::default(),
            files: Vec::new(),
            externals: Default::default(),
            warnings: Vec::new(),
            semver_checks: None,
        };
        for it in items.into_iter().chain([root]) {
            out.items.insert(it.info.id.clone(), it);
        }
        Data::new(out)
    }

    fn calls(it: &mut Item, to: &str) {
        it.info.calls.insert(Call { to: to.into() });
    }

    #[test]
    fn unsafe_then_breaking_then_widely_used() {
        let mut uns = item("c::uns", Status::Changed);
        uns.unsafe_code.push(UnsafeChange {
            kind: UnsafeKind::Block,
            status: Status::Added,
            what: "unsafe { … }".into(),
            lines: Some((1, 1)),
            old_lines: None,
        });
        let mut brk = item("c::brk", Status::Changed);
        brk.api = Some(ApiChange {
            level: ApiLevel::Breaking,
            reasons: vec![],
            confirmed_by: vec![],
        });
        let hub = item("c::hub", Status::Changed);
        let lone = item("c::lone", Status::Changed);
        // Only removes unsafe code: not a risk by itself.
        let mut gone = item("c::gone", Status::Changed);
        gone.unsafe_code.push(UnsafeChange {
            kind: UnsafeKind::Block,
            status: Status::Removed,
            what: "unsafe { … }".into(),
            lines: None,
            old_lines: Some((1, 1)),
        });
        let mut callers: Vec<Item> = (0..3)
            .map(|i| {
                let mut c = item(&format!("c::caller{i}"), Status::Unchanged);
                calls(&mut c, "c::hub");
                c
            })
            .collect();
        // A test reaches `brk` through `caller0`.
        calls(&mut callers[0], "c::brk");
        let mut t = item("c::t", Status::Unchanged);
        t.info.test = true;
        calls(&mut t, "c::caller0");
        let mut all = vec![uns, brk, hub, lone, gone, t];
        all.extend(callers);
        let d = data(all);
        let r = ranked(&d);
        let ids: Vec<&str> = r.iter().map(|r| r.it.info.id.as_str()).collect();
        assert_eq!(ids, ["c::uns", "c::brk", "c::hub"]);
        assert_eq!(r[1].untested, Some(false));
        assert_eq!(r[0].untested, Some(true));
        assert_eq!(r[2].users, 3);
    }
}
