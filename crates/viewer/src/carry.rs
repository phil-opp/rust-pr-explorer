//! Taking review marks to another page. Each explorer comment's page is
//! served from its own address, and browsers keep local storage per
//! address, so a later comment's page starts without the marks made on an
//! earlier one. Marks travel as text instead: in the review that **Finish
//! review…** posts (the action reads them back into the next page), or
//! copied on one page and pasted on another.
//!
//! In a review, the marks are readable lines (see `review::review_lines`)
//! in a code block, in a collapsed `<details>` at the end of the review's
//! text. Copied marks also hold the reviewed versions and the comments, as
//! [`model::MARKS_MARKER`] and JSON in base64, which survives being pasted
//! through chats.

use crate::code::esc;
use crate::review;
use model::{MARKS_MARKER, REVIEW_MARKS_HEADER, marks_in, review_marks_in};

/// The longest marks that go into a posted review. GitHub takes up to
/// 65536 characters for the review's text, which also holds the summary
/// and the comments that fit on no line.
const REVIEW_LIMIT: usize = 40_000;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn unbase64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes().filter(|&c| c != b'=') {
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// This PR's marks as text to copy, `None` without marks.
pub fn copy_text() -> Option<String> {
    let json = review::export(review::Export::Copy)?;
    Some(format!("{MARKS_MARKER}{}", base64(json.as_bytes())))
}

/// The marks for a posted review, empty without marks or if they are too
/// long. A stale mark loaded from a review can't show what changed since
/// then; the page of the changes since the last explorer comment does.
pub fn for_review() -> String {
    let Some(lines) = review::export(review::Export::Review) else {
        return String::new();
    };
    if lines.len() > REVIEW_LIMIT {
        return String::new();
    }
    // A fence longer than any run of backticks in the lines.
    let longest = lines.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!(
        "<details><summary>Review marks (Rust PR Explorer)</summary>\n\n\
         What this review covered, so that the explorer's page for later commits can show it.\n\n\
         {fence}text\n{REVIEW_MARKS_HEADER}\n{lines}\n{fence}\n\n</details>"
    )
}

/// Loads marks from a text: copied from another page, or a posted
/// review's. Says what happened.
pub fn import_text(text: &str) -> Result<String, String> {
    let added = if let Some(lines) = review_marks_in(text) {
        review::import_review(&lines)?
    } else {
        let data = marks_in(text).ok_or("No review marks found in the text.")?;
        let json = unbase64(&data)
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or("The marks in the text are damaged.")?;
        review::import(&json)?
    };
    Ok(match added {
        0 => "No new marks: this page already has them.".to_string(),
        1 => "Loaded 1 mark.".to_string(),
        n => format!("Loaded {n} marks."),
    })
}

/// Loads the marks saved in a review (see `model::SavedMarks`).
pub fn import_saved(data: &str) -> Result<String, String> {
    import_text(&format!("{REVIEW_MARKS_HEADER}\n{data}"))
}

/// When a review was posted, as the browser shows dates.
fn date(iso: &str) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(iso));
    if d.get_time().is_nan() {
        return iso.to_string();
    }
    d.to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED)
        .as_string()
        .unwrap_or_else(|| iso.to_string())
}

/// The dialog of **Marks…**.
pub fn dialog_html() -> String {
    let saved = review::saved_marks();
    let from_reviews = if saved.is_empty() {
        String::new()
    } else {
        let rows: String = saved
            .iter()
            .enumerate()
            .map(|(i, m)| {
                format!(
                    r#"<li><button data-rv-load="{i}">Load</button> <b>{}</b>, <a href="{}" target="_blank" rel="noopener">review of {}</a></li>"#,
                    esc(&m.login),
                    esc(&m.url),
                    esc(&date(&m.at))
                )
            })
            .collect();
        format!(
            r#"<h4>From a posted review</h4>
<p class="sub">A review posted with <b>Finish review…</b> carries the marks it was posted with. The latest of each reviewer:</p>
<ul class="rv-saved">{rows}</ul>"#
        )
    };
    let has = review::has_marks();
    format!(
        r#"<div class="rv-finish">
<p class="sub">Each explorer comment's page keeps its own marks: the browser can't share them between the pages of different comments. Marks are added to this page's, where it has none of its own for the same item.</p>
{from_reviews}
<h4>Copy to another page</h4>
<p class="sub">Copies your marks of this PR as text, to paste on the page of a later explorer comment.</p>
<div class="rv-row"><button id="rv-copy-marks"{copy_off}>Copy marks</button></div>
<textarea id="rv-out" class="hidden" rows="3" readonly></textarea>
<h4>Paste from another page</h4>
<textarea id="rv-paste" rows="3" placeholder="Paste the copied marks here"></textarea>
<div class="rv-row"><button id="rv-paste-load" class="primary">Load marks</button></div>
<h4>Clear</h4>
<div class="rv-row"><button id="rv-clear"{copy_off}>Clear marks</button></div>
</div>"#,
        copy_off = if has { "" } else { " disabled" },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for s in ["", "a", "ab", "abc", "abcd", "{\"ü\":\"--> x\"}"] {
            let enc = base64(s.as_bytes());
            assert_eq!(enc.len() % 4, 0);
            assert_eq!(unbase64(&enc).unwrap(), s.as_bytes(), "{s}");
        }
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(unbase64("TW!u"), None);
    }

    #[test]
    fn review_marks_are_found_again() {
        let block = "<details><summary>Review marks (Rust PR Explorer)</summary>\n\n````text\nrust-pr-explorer review marks v1\nmark 1 a::f\nmark 2 file:x```y\n````\n\n</details>";
        assert_eq!(
            review_marks_in(&format!("Looks good.\n\n{block}")).unwrap(),
            "mark 1 a::f\nmark 2 file:x```y"
        );
    }

    #[test]
    fn marks_are_found_in_a_copy() {
        let body = format!(
            "Looks good.\n\n<!-- {MARKS_MARKER}{} -->",
            base64(b"{\"v\":1}")
        );
        let data = marks_in(&body).unwrap();
        assert_eq!(unbase64(&data).unwrap(), b"{\"v\":1}");
        // Line breaks a paste added are left out.
        assert_eq!(marks_in(&format!("{MARKS_MARKER}TW\nFu")).unwrap(), "TWFu");
        assert_eq!(marks_in("no marks"), None);
    }
}
