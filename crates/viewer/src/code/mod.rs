//! Rust syntax highlighting with cross links, and doc comment rendering.

mod docs_diff;
mod highlight;
mod markdown;
mod sig_diff;
#[cfg(test)]
mod tests;

pub use docs_diff::{
    Seg, docs_compare_html, docs_diff_segs, mark_changed_tokens, markdown_diff_segs, marked_html,
};
pub use highlight::{Links, code_html, ref_tag};
pub use markdown::{docs_html, first_paragraph, markdown_file_html};
pub use sig_diff::{note_html, sig_diff_html, structured_diff_html};

/// Whether a URL may be used as a link: only `http(s)`.
pub fn web_url(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}
