//! Drafts of the texts a reviewer is writing: a comment in its editor and
//! the summary in the "Finish review" dialog. A dialog closes on Escape and
//! on a click next to it, and until its text is saved it lives only in the
//! dialog. So every change is kept in the browser's storage, by PR and by
//! what the text is for, and put back when the dialog opens again.

use crate::dom;
use wasm_bindgen::JsCast;

const KEY: &str = "rust-semantic-review.draft";

/// The draft of the summary in the "Finish review" dialog.
pub const SUMMARY: &str = "summary";

thread_local! {
    /// The text the open editor had before its draft was put into it, for
    /// "Discard draft".
    static SAVED: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn storage() -> Option<web_sys::Storage> {
    dom::window().local_storage().ok().flatten()
}

/// The storage key of a draft of this PR.
fn key(field: &str) -> String {
    format!("{KEY}:{}:{field}", crate::review::key())
}

pub fn get(field: &str) -> Option<String> {
    storage()?.get_item(&key(field)).ok().flatten()
}

/// Keeps `text` as the draft of `field`; an empty text removes it.
pub fn set(field: &str, text: &str) {
    let Some(s) = storage() else {
        return;
    };
    if text.trim().is_empty() {
        let _ = s.remove_item(&key(field));
    } else {
        // A full storage loses the draft, as before there were drafts.
        let _ = s.set_item(&key(field), text);
    }
}

pub fn remove(field: &str) {
    set(field, "");
}

/// Removes every draft of this PR (with its marks, see `review::clear`).
pub fn clear() {
    let Some(s) = storage() else {
        return;
    };
    let prefix = key("");
    let keys: Vec<String> = (0..s.length().unwrap_or(0))
        .filter_map(|i| s.key(i).ok().flatten())
        .filter(|k| k.starts_with(&prefix))
        .collect();
    for k in keys {
        let _ = s.remove_item(&k);
    }
}

/// What the open comment editor's draft is kept under: `data-rv-draft` if
/// the editor names one (a comment on lines that isn't saved yet has no id
/// of its own), else its unit or comment (`data-rv-for`).
pub fn editor_field() -> Option<String> {
    let e = dom::query("#modal:not(.hidden) .rv-editor")?;
    dom::data(&e, "rv-draft").or_else(|| dom::data(&e, "rv-for").map(|id| format!("note:{id}")))
}

fn textarea(id: &str) -> Option<web_sys::HtmlTextAreaElement> {
    dom::by_id(id)?.dyn_into().ok()
}

/// After a comment editor opened: puts its draft into it, if it has one
/// that differs from the saved comment, and says so, with a way back.
pub fn restore_editor() {
    let (Some(field), Some(t)) = (editor_field(), textarea("rv-text")) else {
        return;
    };
    let Some(draft) = get(&field).filter(|d| d.trim() != t.value().trim()) else {
        return;
    };
    SAVED.with(|s| *s.borrow_mut() = t.value());
    t.set_value(&draft);
    let _ = t.insert_adjacent_html(
        "beforebegin",
        r#"<p class="note rv-draft">This is your unsaved draft from before. <button data-rv-draft-discard>Discard draft</button></p>"#,
    );
}

/// "Discard draft": back to the saved comment.
pub fn discard_editor() {
    if let Some(field) = editor_field() {
        remove(&field);
    }
    if let Some(t) = textarea("rv-text") {
        t.set_value(&SAVED.with(|s| s.borrow().clone()));
        let _ = t.focus();
    }
    if let Some(note) = dom::query(".rv-draft") {
        note.remove();
    }
}

/// After the "Finish review" dialog opened: puts the summary's draft back.
pub fn restore_summary() {
    if let (Some(draft), Some(t)) = (get(SUMMARY), textarea("rv-body")) {
        t.set_value(&draft);
    }
}

/// A text field of a dialog changed: keeps its text as a draft.
pub fn on_input(el: &web_sys::Element) {
    let Some(t) = el.dyn_ref::<web_sys::HtmlTextAreaElement>() else {
        return;
    };
    match el.id().as_str() {
        "rv-body" => set(SUMMARY, &t.value()),
        "rv-text" => {
            if let Some(field) = editor_field() {
                set(&field, &t.value());
            }
        }
        _ => {}
    }
}
