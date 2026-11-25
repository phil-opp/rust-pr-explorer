//! Undo for the canvas: every click, key press or drag that changes which
//! cards are open, their tabs, their places, the connectors, the scope or
//! the module frames can be taken back, with the Undo button or Ctrl+Z.

use super::{Canvas, Expansion, Group, render};
use crate::{App, dom, toast};

/// At most this many changes can be undone.
const LIMIT: usize = 50;

#[derive(Clone)]
pub struct Snapshot {
    groups: Vec<Group>,
    links: Vec<(String, String)>,
    scope: String,
    /// Whether module frames were drawn: laying the cards out by module
    /// turns them on, and undoing that turns them off again.
    frames: bool,
    /// What the "calls", "callers" and "used by" buttons had opened, so
    /// that they show as pressed again after an undo.
    expansions: Vec<Expansion>,
}

impl Snapshot {
    fn of(c: &Canvas) -> Snapshot {
        Snapshot {
            groups: c.groups.clone(),
            links: c.links.clone(),
            scope: c.scope.clone(),
            frames: c.frames,
            expansions: c.expansions.clone(),
        }
    }

    /// Whether `c` shows the same layout: the same cards with the same
    /// tabs in the same places, connectors, scope and frames. What a card shows
    /// inside (an open source view, unfolded lines) doesn't count.
    fn same_layout(&self, c: &Canvas) -> bool {
        self.links == c.links
            && self.scope == c.scope
            && self.frames == c.frames
            && self.groups.len() == c.groups.len()
            && self.groups.iter().zip(&c.groups).all(|(a, b)| {
                a.gid == b.gid
                    && a.tabs == b.tabs
                    && a.active == b.active
                    && a.x == b.x
                    && a.y == b.y
            })
    }
}

/// Remembers the layout before an event that may change it.
pub fn before_change(app: &mut App) {
    let c = &mut app.s.canvas;
    c.before = Some(Snapshot::of(c));
}

/// After the event: if it changed the layout, the layout before it can
/// be restored with [`undo`].
pub fn after_change(app: &mut App) {
    let c = &mut app.s.canvas;
    let Some(before) = c.before.take() else {
        return;
    };
    if before.same_layout(c) {
        return;
    }
    c.history.push(before);
    if c.history.len() > LIMIT {
        c.history.remove(0);
    }
    update_button(c);
}

/// Restores the layout before the last change.
pub fn undo(app: &mut App) {
    let c = &mut app.s.canvas;
    c.before = None;
    let Some(s) = c.history.pop() else {
        toast("Nothing to undo");
        return;
    };
    c.groups = s.groups;
    c.links = s.links;
    c.scope = s.scope;
    c.frames = s.frames;
    c.expansions = s.expansions;
    render(app);
    toast("Undone");
}

fn update_button(c: &Canvas) {
    if let Some(b) = dom::by_id("cv-undo") {
        if c.history.is_empty() {
            let _ = b.set_attribute("disabled", "");
        } else {
            let _ = b.remove_attribute("disabled");
        }
    }
}
