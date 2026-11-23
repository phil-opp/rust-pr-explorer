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
pub use highlight::{Links, code_html};
pub use markdown::{docs_html, first_paragraph, markdown_file_html};
pub use sig_diff::{note_html, sig_diff_html, structured_diff_html};

/// The start of a line in an inline diff: its old and new line numbers
/// (GitHub style) and its `+`/`-` sign. The numbers are CSS-generated
/// text, so copying code leaves them out; the "Line numbers" setting
/// hides them and shows the line's own sign instead.
pub fn gutter(old: Option<usize>, new: Option<usize>, sign: char) -> String {
    let no = |n: Option<usize>| n.map(|n| n.to_string()).unwrap_or_default();
    format!(
        r#"<span class="lno" data-o="{}" data-n="{}"></span><span class="sg" data-t="{}"></span>"#,
        no(old),
        no(new),
        if sign == ' ' {
            String::new()
        } else {
            sign.to_string()
        }
    )
}

/// The `style` attribute that makes the line number columns of a diff as
/// wide as its largest number.
pub fn gutter_style(max: usize) -> String {
    format!(
        r#"style="--lnw: {}ch""#,
        max.max(1).to_string().len().max(2)
    )
}

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
