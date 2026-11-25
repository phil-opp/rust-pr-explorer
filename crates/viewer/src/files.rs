//! Files the Rust analysis doesn't cover: `Cargo.toml` changes, Python
//! definitions, YAML keys, CSS selectors, Markdown sections, and plain line
//! diffs for the
//! rest. For analyzed Rust files, the code that belongs to no item: tests,
//! macro calls and `extern` blocks as parts, and the remaining lines.

use crate::code::{
    Links, Seg, code_html, esc, mark_changed_lines, markdown_diff_segs, markdown_file_html,
    marked_html, sig_diff_html,
};
use crate::css;
use crate::data::Data;
use crate::js;
use crate::yaml;
use model::{
    ChangedFile, DepKind, Kind, LockChange, ManifestChange, PartChange, SourceLine, Status,
};
use std::collections::{BTreeMap, BTreeSet};

/// Changes up to this many lines are shown directly, larger ones folded.
const SMALL: u32 = 12;

fn badge(status: Status) -> &'static str {
    match status {
        Status::Added => r#"<span class="st st-added badge">New</span>"#,
        Status::Removed => r#"<span class="st st-removed badge">Removed</span>"#,
        _ => "",
    }
}

fn stats(added: u32, removed: u32) -> String {
    format!(r#"<span class="d-add">+{added}</span> <span class="d-del">−{removed}</span>"#)
}

/// Hunks of a line diff. Rust, Python, JavaScript, YAML and CSS get
/// syntax colors; nothing
/// is linked.
pub fn hunks_html(d: &Data, lines: &[SourceLine], lang: &str) -> String {
    diff_html(d, lines, lang, &Diff::default())
}

/// What a file's diff adds to [`hunks_html`].
#[derive(Default)]
pub struct Diff<'a> {
    /// The file it is a diff of: its lines can be commented on (see
    /// `line_notes`).
    pub path: Option<&'a str>,
    /// The review controls of hunks: the hunk of the file that starts at
    /// the `N`th `@` line of the diff, by its key (`lines:N`), else the
    /// hunk of a box.
    pub hunk: Option<&'a dyn Fn(usize) -> Option<String>>,
    /// No "⋯ line N" before the first line: a box's diff, whose summary
    /// says where it starts.
    pub no_first_skip: bool,
}

/// [`hunks_html`] with the review controls and line comments of a file's
/// diff.
pub fn diff_html(d: &Data, lines: &[SourceLine], lang: &str, opts: &Diff) -> String {
    let unit = opts.path.map(crate::review::file_unit);
    let lnotes = unit
        .as_deref()
        .map(|u| crate::line_notes::ForUnit::of(d, u));
    let mut new_line = 0usize;
    let mut old_line = 0usize;
    let mut max_line = 0usize;
    // YAML block scalars span lines; each side keeps its own state.
    let (mut old_yaml, mut new_yaml) = (yaml::State::default(), yaml::State::default());
    // So do JavaScript comments and template literals.
    let (mut old_js, mut new_js) = (js::State::default(), js::State::default());
    // So do CSS comments.
    let (mut old_css, mut new_css) = (css::State::default(), css::State::default());
    let mut body = String::new();
    let mut codes: Vec<String> = Vec::with_capacity(lines.len());
    for l in lines {
        if l.0 == '@' {
            (old_yaml, new_yaml) = (yaml::State::default(), yaml::State::default());
            (old_js, new_js) = (js::State::default(), js::State::default());
            (old_css, new_css) = (css::State::default(), css::State::default());
        }
        codes.push(match lang {
            "rust" | "python" => code_html(d, &l.1, Links::None),
            "yaml" => match l.0 {
                '-' => yaml::line_html(&mut old_yaml, &l.1),
                '+' => yaml::line_html(&mut new_yaml, &l.1),
                _ => {
                    yaml::line_html(&mut old_yaml, &l.1);
                    yaml::line_html(&mut new_yaml, &l.1)
                }
            },
            "javascript" => match l.0 {
                '-' => js::line_html(&mut old_js, &l.1),
                '+' => js::line_html(&mut new_js, &l.1),
                _ => {
                    js::line_html(&mut old_js, &l.1);
                    js::line_html(&mut new_js, &l.1)
                }
            },
            "css" => match l.0 {
                '-' => css::line_html(&mut old_css, &l.1),
                '+' => css::line_html(&mut new_css, &l.1),
                _ => {
                    css::line_html(&mut old_css, &l.1);
                    css::line_html(&mut new_css, &l.1)
                }
            },
            _ => esc(&l.1),
        });
    }
    // Removed lines directly followed by added ones get their changed
    // words marked, compared as a block.
    let mut i = 0;
    while i < lines.len() {
        let dels = lines[i..].iter().take_while(|l| l.0 == '-').count();
        let adds = lines[i + dels..].iter().take_while(|l| l.0 == '+').count();
        if dels > 0
            && adds > 0
            && let Some((a, b)) =
                mark_changed_lines(&codes[i..i + dels], &codes[i + dels..i + dels + adds])
        {
            codes.splice(i..i + dels + adds, a.into_iter().chain(b));
        }
        i += (dels + adds).max(1);
    }
    // The `@` hunk the lines are in, and whether it is in a box.
    let mut at: Option<usize> = None;
    let mut boxed = false;
    for (l, code) in lines.iter().zip(codes) {
        if l.0 == '@' {
            let mut it = l.1.split(' ').filter_map(|n| n.parse::<usize>().ok());
            old_line = it.next().unwrap_or(1);
            new_line = it.next().unwrap_or(1);
            if std::mem::take(&mut boxed) {
                body.push_str("</span>");
            }
            let k = at.map_or(0, |k| k + 1);
            if !(opts.no_first_skip && at.is_none()) {
                body.push_str(&format!(
                    r#"<span class="ln skip">⋯ line {new_line}</span>"#
                ));
            }
            at = Some(k);
            if let Some(bar) = opts.hunk.and_then(|f| f(k)) {
                body.push_str(&format!(
                    r#"<span class="hunk"><span class="hunk-rv">{bar}</span>"#
                ));
                boxed = true;
            }
            continue;
        }
        let (class, no) = match l.0 {
            '+' => ("add", new_line),
            '-' => ("del", old_line),
            _ => ("", new_line),
        };
        let gutter = crate::code::gutter(
            (l.0 != '+').then_some(old_line),
            (l.0 != '-').then_some(new_line),
            l.0,
        );
        max_line = max_line.max(old_line).max(new_line);
        match l.0 {
            '+' => new_line += 1,
            '-' => old_line += 1,
            _ => {
                new_line += 1;
                old_line += 1;
            }
        }
        // Line comments: a removed line by its base number, any other by
        // the PR's.
        let side_old = l.0 == '-';
        let lc = lnotes.as_ref().map_or(String::new(), |n| {
            format!(
                r#" data-lc="{}"{}"#,
                crate::line_notes::key(side_old, no),
                if n.covers(side_old, no) {
                    " data-lc-on"
                } else {
                    ""
                }
            )
        });
        body.push_str(&format!(
            r#"<span class="ln {class}" data-t="{}" data-no="{no}"{lc}>{gutter}{}</span>"#,
            if l.0 == ' ' {
                String::new()
            } else {
                l.0.to_string()
            },
            if code.is_empty() {
                " ".to_string()
            } else {
                code
            }
        ));
        if let Some(n) = &lnotes {
            body.push_str(&n.boxes_at(d, &[(side_old, no)]));
        }
    }
    if boxed {
        body.push_str("</span>");
    }
    if let Some(n) = &lnotes {
        body.push_str(&n.stale_html(d));
    }
    let pre = format!(
        r#"<pre class="hunks" {}>{body}</pre>"#,
        crate::code::gutter_style(max_line)
    );
    match unit {
        Some(u) => format!(
            r#"<div class="src-code" data-item="{}">{pre}</div>"#,
            esc(&u)
        ),
        None => pre,
    }
}

fn manifest_html(entries: &[ManifestChange]) -> String {
    let mut by_section: BTreeMap<&str, Vec<&ManifestChange>> = BTreeMap::new();
    for e in entries {
        by_section.entry(&e.section).or_default().push(e);
    }
    let mut html = String::from(r#"<table class="manifest">"#);
    for (section, list) in by_section {
        html.push_str(&format!(
            r#"<tr><th colspan="3" class="mf-sec">[{}]</th></tr>"#,
            esc(section)
        ));
        for e in list {
            let (class, what) = match (&e.old, &e.new) {
                (None, Some(_)) => ("m-add", "added"),
                (Some(_), None) => ("m-del", "removed"),
                _ => ("m-chg", "changed"),
            };
            let old = e
                .old
                .as_deref()
                .map(|o| format!("<del>{}</del>", esc(o)))
                .unwrap_or_default();
            let new = e.new.as_deref().map(esc).unwrap_or_default();
            html.push_str(&format!(
                r#"<tr class="{class}" title="{what}"><td class="mf-name"><code>{}</code></td><td class="mf-old"><code>{old}</code></td><td class="mf-new"><code>{new}</code></td></tr>"#,
                esc(&e.name)
            ));
        }
    }
    html.push_str("</table>");
    html
}

/// "2 tests and 1 macro call": the parts counted by kind.
fn parts_summary(parts: &[PartChange]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for p in parts {
        match counts.iter_mut().find(|c| c.0 == p.kind) {
            Some(c) => c.1 += 1,
            None => counts.push((&p.kind, 1)),
        }
    }
    let list: Vec<String> = counts
        .iter()
        .map(|&(kind, n)| {
            let (one, many) = match kind {
                "fn" => ("function", "functions"),
                "test" => ("test", "tests"),
                "bench" => ("benchmark", "benchmarks"),
                "impl" => ("impl block", "impl blocks"),
                "extern" => ("extern block", "extern blocks"),
                "macro call" => ("macro call", "macro calls"),
                "macro" => ("macro", "macros"),
                "struct" => ("struct", "structs"),
                "enum" => ("enum", "enums"),
                "const" => ("constant", "constants"),
                "static" => ("static", "statics"),
                "type" => ("type alias", "type aliases"),
                "trait" => ("trait", "traits"),
                _ => ("other part", "other parts"),
            };
            format!("{n} {}", if n == 1 { one } else { many })
        })
        .collect();
    match list.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => list.join(""),
    }
}

/// A file's changed parts (see [`PartChange`]), one box each; with its
/// hunk's review controls at the top if the file is reviewed hunk by hunk.
fn parts_html(d: &Data, path: &str, list: &[PartChange], lang: &str) -> String {
    let no_refs = BTreeMap::new();
    // Rust and Python headers are highlighted (and diffed) as code; YAML
    // and JavaScript ones with their own highlighters.
    let code = lang == "rust" || lang == "python";
    let plain = |text: &str| -> String {
        if lang == "javascript" {
            js::text_html(text)
        } else {
            yaml::line_html(&mut yaml::State::default(), text)
        }
    };
    list.iter()
        .enumerate()
        .map(|(i, c)| {
            if c.kind == "module" {
                return format!(
                    r#"<div class="irow member part"><div class="ihead"><span class="kind">module</span><span class="title-name">top-level code</span></div><div class="mark m-chg"><div class="note">Statements outside functions and classes changed; see the line diff{}.</div></div></div>"#,
                    match line_hunks_label(path) {
                        Some(l) => format!(" below <span class=\"rv-in\">({l})</span>"),
                        None => String::new(),
                    }
                );
            }
            let sig = match &c.old_sig {
                Some(old) if code => format!(
                    r#"<pre class="sig sig-diff">{}</pre>"#,
                    sig_diff_html(d, old, &c.sig, &no_refs, &[])
                ),
                Some(old) => format!(
                    r#"<pre class="sig"><del>{}</del>
{}</pre>"#,
                    plain(old),
                    plain(&c.sig)
                ),
                None if code => {
                    format!(r#"<pre class="sig">{}</pre>"#, code_html(d, &c.sig, Links::None))
                }
                None => format!(r#"<pre class="sig">{}</pre>"#, plain(&c.sig)),
            };
            let (sig_bar, sig_class) = match c.status {
                Status::Added | Status::Removed => ("", ""),
                _ if c.old_sig.is_some() => ("m-chg", ""),
                _ => ("", "m-ref"),
            };
            let mut segs = Vec::new();
            // A YAML key's line repeats its name, unless it changed.
            if lang != "yaml" || c.old_sig.is_some() {
                segs.push(Seg::new(
                    sig_bar,
                    format!(r#"<div class="sigbox {sig_class}">{sig}</div>"#),
                ));
            }
            if c.doc_changed {
                segs.push(Seg::new(
                    "m-chg",
                    format!(
                        r#"<div class="note">{} changed.</div>"#,
                        match lang {
                            "python" => "Docstring",
                            "javascript" => "JSDoc comment",
                            _ => "Doc comment",
                        }
                    ),
                ));
            }
            let added = c.lines.iter().filter(|l| l.0 == '+').count() as u32;
            let removed = c.lines.iter().filter(|l| l.0 == '-').count() as u32;
            let (what, bar) = match c.status {
                Status::Added | Status::Removed => ("Source", ""),
                _ if c.cosmetic => ("Formatting or comments only", ""),
                _ if c.body_changed => ("<b>Body changed</b>", "m-chg"),
                _ => ("Source", ""),
            };
            // Like Rust items' bodies: small changes start open.
            let small =
                c.status == Status::Changed && added + removed <= crate::data::SMALL_BODY as u32;
            segs.push(Seg::new(
                bar,
                format!(
                    r#"<details class="src {}" {}><summary>{what} {} · line {}</summary>{}</details>"#,
                    if bar.is_empty() { "" } else { "src-chg" },
                    if small { "open" } else { "" },
                    stats(added, removed),
                    c.line,
                    diff_html(
                        d,
                        &c.lines,
                        lang,
                        &Diff {
                            path: Some(path),
                            no_first_skip: true,
                            ..Default::default()
                        }
                    )
                ),
            ));
            let status_class = match c.status {
                Status::Added => "added",
                Status::Removed => "removed",
                _ => "",
            };
            let part = format!(
                r#"<div class="irow member part {status_class}"><div class="ihead"><span class="kind">{}</span><span class="title-name">{}</span>{}{}</div>{}{}</div>"#,
                esc(&c.kind),
                esc(&c.name),
                badge(c.status),
                crate::unsafety::part_tag(c),
                crate::unsafety::lines_html(&c.unsafe_code),
                marked_html(segs)
            );
            match hunk_box(d, path, &format!("part:{i}")) {
                Some((open, close)) => format!("{open}{part}{close}"),
                None => part,
            }
        })
        .collect()
}

/// A part of a Markdown file from one heading to the next.
#[derive(Debug)]
struct Section {
    /// The heading path (`Usage › Flags`), made unique.
    key: String,
    /// The heading's line (two for a setext heading), empty before the
    /// first heading.
    heading: String,
    text: String,
    /// Its first and last line in the file (1-based).
    first: usize,
    last: usize,
}

impl Section {
    fn title(&self) -> &str {
        let title = self
            .heading
            .lines()
            .next()
            .unwrap_or("")
            .trim_start_matches('#')
            .trim();
        if title.is_empty() {
            "(before the first heading)"
        } else {
            title
        }
    }
}

/// Markdown split at headings (outside code fences).
fn sections(md: &str) -> Vec<Section> {
    let section = |key: String, heading: String, first: usize| Section {
        key,
        heading,
        text: String::new(),
        first,
        last: 0,
    };
    let mut out: Vec<Section> = vec![section(String::new(), String::new(), 1)];
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut fence: Option<&str> = None;
    let lines: Vec<&str> = md.lines().collect();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        let indent = line.len() - t.len();
        match fence {
            Some(f) if t.starts_with(f) => fence = None,
            Some(_) => {}
            None if t.starts_with("```") => fence = Some("```"),
            None if t.starts_with("~~~") => fence = Some("~~~"),
            None => {}
        }
        let atx = (fence.is_none() && indent <= 3)
            .then(|| {
                let level = t.chars().take_while(|c| *c == '#').count();
                let rest = &t[level..];
                ((1..=6).contains(&level) && (rest.is_empty() || rest.starts_with(' ')))
                    .then(|| (level, rest.trim().trim_end_matches('#').trim().to_string()))
            })
            .flatten();
        // Setext: a text line underlined with `=` or `-`.
        let setext = (atx.is_none()
            && fence.is_none()
            && !t.is_empty()
            && indent <= 3
            && i + 1 < lines.len()
            // A one-line paragraph: the line before is blank.
            && (i == 0 || lines[i - 1].trim().is_empty()))
        .then(|| {
            let next = lines[i + 1].trim();
            if !next.is_empty() && next.chars().all(|c| c == '=') {
                Some(1)
            } else if next.len() >= 2 && next.chars().all(|c| c == '-') {
                Some(2)
            } else {
                None
            }
        })
        .flatten()
        .map(|level| (level, t.trim().to_string()));
        if let Some((level, title)) = atx.clone().or(setext.clone()) {
            while path.last().is_some_and(|(l, _)| *l >= level) {
                path.pop();
            }
            path.push((level, title));
            let mut key = path
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>()
                .join(" › ");
            let n = seen.entry(key.clone()).or_default();
            *n += 1;
            if *n > 1 {
                key = format!("{key} ({n})");
            }
            let heading = if setext.is_some() {
                format!("{line}\n{}", lines[i + 1])
            } else {
                line.to_string()
            };
            out.push(section(key, heading, i + 1));
            i += if setext.is_some() { 2 } else { 1 };
            continue;
        }
        let last = out.last_mut().unwrap();
        last.text.push_str(line);
        last.text.push('\n');
        i += 1;
    }
    let ends: Vec<usize> = out.iter().skip(1).map(|s| s.first - 1).collect();
    for (s, end) in out.iter_mut().zip(ends.into_iter().chain([lines.len()])) {
        s.last = end;
    }
    if out[0].text.trim().is_empty() {
        out.remove(0);
    }
    out
}

/// A changed section: its index in the base's sections and in the PR's.
type SectionChange = (Option<usize>, Option<usize>);

/// The sections of a Markdown file that changed, in the order the file's
/// view shows them: the PR's sections that are new or changed, each with
/// its base version (by key), then the removed ones.
fn section_changes(a: &[Section], b: &[Section]) -> (Vec<SectionChange>, usize) {
    let old_by_key: BTreeMap<&str, usize> = a
        .iter()
        .enumerate()
        .map(|(i, s)| (s.key.as_str(), i))
        .collect();
    let new_keys: BTreeSet<&str> = b.iter().map(|s| s.key.as_str()).collect();
    let mut out = Vec::new();
    let mut unchanged = 0;
    for (j, s) in b.iter().enumerate() {
        match old_by_key.get(s.key.as_str()) {
            Some(&i) if a[i].heading == s.heading && a[i].text == s.text => unchanged += 1,
            o => out.push((o.copied(), Some(j))),
        }
    }
    for (i, o) in a.iter().enumerate() {
        if !new_keys.contains(o.key.as_str()) {
            out.push((Some(i), None));
        }
    }
    (out, unchanged)
}

/// A Markdown file compared section by section: changed sections show
/// their changes (paragraph and word level), unchanged ones are counted.
/// With `path`, each changed section is a box with its hunk's review
/// controls, if the file is reviewed hunk by hunk.
pub fn markdown_html(d: &Data, old: &str, new: &str, path: Option<&str>) -> String {
    let (a, b) = (sections(old), sections(new));
    let heading = |s: &Section| {
        format!(
            r#"<div class="md-h" title="{}">{}</div>"#,
            esc(&s.key),
            esc(s.title())
        )
    };
    let (changes, unchanged) = section_changes(&a, &b);
    let mut html = String::new();
    let mut all: Vec<Seg> = Vec::new();
    for (o, n) in changes {
        let mut segs: Vec<Seg> = Vec::new();
        match (o.map(|i| &a[i]), n.map(|j| &b[j])) {
            (Some(o), Some(s)) => {
                let head_changed = o.heading != s.heading;
                segs.push(Seg::new(
                    if head_changed { "m-chg" } else { "" },
                    heading(s),
                ));
                segs.extend(markdown_diff_segs(d, &o.text, &s.text));
            }
            (None, Some(s)) => {
                let body = markdown_file_html(d, &s.text);
                segs.push(Seg::new(
                    "m-add",
                    format!(r#"{}<div class="docs">{body}</div>"#, heading(s)),
                ));
            }
            (Some(o), None) => segs.push(Seg::new(
                "m-del",
                format!(r#"<div class="md-removed">{}</div>"#, heading(o)),
            )),
            (None, None) => {}
        }
        let key = n.map_or_else(|| a[o.unwrap_or(0)].key.clone(), |j| b[j].key.clone());
        let side = if n.is_some() { "sec" } else { "sec-removed" };
        match path.and_then(|p| hunk_box(d, p, &format!("{side}:{key}"))) {
            Some((open, close)) => {
                html.push_str(&marked_html(std::mem::take(&mut all)));
                html.push_str(&open);
                html.push_str(&marked_html(segs));
                html.push_str(&close);
            }
            // Neighboring sections with the same marker share one bar.
            None => all.extend(segs),
        }
    }
    html.push_str(&marked_html(all));
    if unchanged > 0 {
        html.push_str(&format!(
            r#"<div class="collapsed-note">{unchanged} unchanged section{}</div>"#,
            if unchanged == 1 { "" } else { "s" }
        ));
    }
    html
}

/// Which of a file's hunks its line diff shows ("change 2 of 2",
/// "changes 2 and 3 of 3"), if it shows any: the changed lines that no box
/// holds. The line diff then starts open, so that they aren't missed.
fn line_hunks_label(path: &str) -> Option<String> {
    let all = crate::review::hunks_of(&crate::review::file_unit(path));
    let mine: Vec<String> = all
        .iter()
        .enumerate()
        .filter(|(_, h)| h.key.starts_with("lines:"))
        .map(|(i, _)| (i + 1).to_string())
        .collect();
    let (last, rest) = mine.split_last()?;
    Some(if rest.is_empty() {
        format!("change {last} of {}", all.len())
    } else {
        format!("changes {} and {last} of {}", rest.join(", "), all.len())
    })
}

/// The start and end of the box around a hunk of a file, if the file is
/// reviewed hunk by hunk and has the hunk `key`: its review controls and a
/// link to its lines on GitHub at the top.
fn hunk_box(d: &Data, path: &str, key: &str) -> Option<(String, String)> {
    let h = crate::review::file_hunk(path, key)?;
    Some((
        format!(
            r#"<div class="hunk fhunk"><div class="hunk-rv">{}{}</div>"#,
            crate::review::file_hunk_controls(path, &h),
            hunk_link(d, path, &h)
        ),
        "</div>".to_string(),
    ))
}

/// A link to a file's hunk on GitHub, for its review bar.
fn hunk_link(d: &Data, path: &str, h: &crate::review::Hunk) -> String {
    crate::post::github_link(
        d,
        crate::post::file_hunk_loc(&d.out.files, path, h),
        "gh-link",
        "GitHub ↗",
        "this change",
    )
}

/// A line of a diff with `@` lines: its tag, its numbers in the base and
/// in the PR (one of them is meaningless for `+` and `-` lines), its
/// text, and which `@` hunk it is in.
struct Row<'a> {
    tag: char,
    old: usize,
    new: usize,
    text: &'a str,
    hunk: usize,
}

fn rows(lines: &[SourceLine]) -> Vec<Row<'_>> {
    let (mut o, mut n) = (1, 1);
    let mut hunk = 0;
    let mut out = Vec::new();
    for l in lines {
        if l.0 == '@' {
            let mut it = l.1.split(' ').filter_map(|x| x.parse::<usize>().ok());
            o = it.next().unwrap_or(1);
            n = it.next().unwrap_or(1);
            hunk += usize::from(!out.is_empty());
            continue;
        }
        let tag = if matches!(l.0, '+' | '-') { l.0 } else { ' ' };
        out.push(Row {
            tag,
            old: o,
            new: n,
            text: &l.1,
            hunk,
        });
        o += usize::from(tag != '+');
        n += usize::from(tag != '-');
    }
    out
}

/// The hunks a changed file outside the Rust analysis is reviewed by, if
/// it has more than one: the boxes of its view (Markdown sections, Python
/// and JavaScript functions and classes, YAML keys, Rust code outside
/// items), each with the changed lines in it, and then the changed lines
/// of its line diff that no box holds, one hunk per hunk of that diff.
/// Their `key` says where the view shows them: `sec:KEY` and
/// `sec-removed:KEY` for a section, `part:N` for the file's N-th part and
/// `lines:N` for the N-th hunk of its line diff.
///
/// A box's id hashes what the box shows, not its line numbers, and a line
/// hunk's its changed lines, so a box that a later push leaves alone
/// keeps its mark. Added, removed, generated, binary and very large files
/// have no hunks.
pub fn review_hunks(f: &ChangedFile) -> Vec<crate::review::Hunk> {
    if f.status != Status::Changed || f.generated.is_some() || f.too_large || f.lang == "binary" {
        return Vec::new();
    }
    // The changed lines that a box can hold: for an analyzed Rust file all
    // of them (its line diff only has those outside items and parts).
    let diff = rows(&f.hunks);
    let all: Vec<(char, usize)> = if f.analyzed {
        let mut v: Vec<(char, usize)> = (f.removed_lines.iter().map(|&l| ('-', l as usize)))
            .chain(f.added_lines.iter().map(|&l| ('+', l as usize)))
            .collect();
        v.sort_by_key(|&(t, l)| (l, t == '+'));
        v
    } else {
        diff.iter()
            .filter(|r| r.tag != ' ')
            .map(|r| (r.tag, if r.tag == '-' { r.old } else { r.new }))
            .collect()
    };
    // Each box: key, name, what it shows, and the lines it spans on each
    // side.
    type Claim = BTreeSet<usize>;
    let mut boxes: Vec<(String, String, String, Claim, Claim)> = Vec::new();
    match (f.lang.as_str(), &f.old_text, &f.new_text) {
        ("markdown", Some(old), Some(new)) => {
            let (a, b) = (sections(old), sections(new));
            for (o, n) in section_changes(&a, &b).0 {
                let (o, n) = (o.map(|i| &a[i]), n.map(|j| &b[j]));
                let span = |s: Option<&Section>| -> Claim {
                    s.map(|s| (s.first..=s.last).collect()).unwrap_or_default()
                };
                let show = |s: Option<&Section>| {
                    s.map(|s| format!("{}\n{}", s.heading, s.text))
                        .unwrap_or_default()
                };
                let Some(s) = n.or(o) else { continue };
                let key = format!(
                    "{}:{}",
                    if n.is_some() { "sec" } else { "sec-removed" },
                    s.key
                );
                boxes.push((
                    key,
                    format!("section {}", s.title()),
                    format!("section\n{}\n{}\0{}", s.key, show(o), show(n)),
                    span(o),
                    span(n),
                ));
            }
        }
        ("python" | "javascript" | "yaml" | "rust", _, _) => {
            for (i, p) in f.parts.iter().enumerate() {
                if p.kind == "module" {
                    continue;
                }
                let lines = rows(&p.lines);
                let shown: String = lines
                    .iter()
                    .map(|r| format!("{}{}\n", r.tag, r.text))
                    .collect();
                boxes.push((
                    format!("part:{i}"),
                    format!("{} {}", p.kind, p.name),
                    format!(
                        "part\n{}\n{}\n{:?}\n{:?}\n{shown}",
                        p.kind, p.name, p.status, p.old_sig
                    ),
                    lines
                        .iter()
                        .filter(|r| r.tag != '+')
                        .map(|r| r.old)
                        .collect(),
                    lines
                        .iter()
                        .filter(|r| r.tag != '-')
                        .map(|r| r.new)
                        .collect(),
                ));
            }
        }
        _ => {}
    }
    let held = |claims: &(Claim, Claim), (tag, line): (char, usize)| {
        if tag == '-' {
            claims.0.contains(&line)
        } else {
            claims.1.contains(&line)
        }
    };
    let mut out: Vec<(String, crate::review::Hunk)> = Vec::new();
    let mut claimed: Vec<(Claim, Claim)> = Vec::new();
    for (key, name, text, old, new) in boxes {
        let claims = (old, new);
        let mine: Vec<(char, usize)> = all.iter().copied().filter(|&c| held(&claims, c)).collect();
        out.push((
            text,
            crate::review::Hunk {
                new: mine.iter().filter(|c| c.0 == '+').map(|c| c.1).collect(),
                old: mine.iter().filter(|c| c.0 == '-').map(|c| c.1).collect(),
                starts_added: mine.first().is_some_and(|c| c.0 == '+'),
                name,
                key,
                ..Default::default()
            },
        ));
        claimed.push(claims);
    }
    // The other changed lines of the line diff, by its hunks.
    let rest: Vec<&Row> = diff
        .iter()
        .filter(|r| r.tag != ' ')
        .filter(|r| {
            let c = (r.tag, if r.tag == '-' { r.old } else { r.new });
            !claimed.iter().any(|cl| held(cl, c))
        })
        .collect();
    for group in rest.chunk_by(|a, b| a.hunk == b.hunk) {
        let first = group[0];
        out.push((
            group
                .iter()
                .map(|r| format!("{}{}\n", r.tag, r.text))
                .collect(),
            crate::review::Hunk {
                new: group
                    .iter()
                    .filter(|r| r.tag == '+')
                    .map(|r| r.new)
                    .collect(),
                old: group
                    .iter()
                    .filter(|r| r.tag == '-')
                    .map(|r| r.old)
                    .collect(),
                starts_added: first.tag == '+',
                name: format!(
                    "line {}",
                    if first.tag == '+' {
                        first.new
                    } else {
                        first.old
                    }
                ),
                key: format!("lines:{}", first.hunk),
                ..Default::default()
            },
        ));
    }
    if out.len() < 2 {
        return Vec::new();
    }
    crate::review::with_ids(&crate::review::file_unit(&f.path), out)
}

fn file_card(d: &Data, f: &ChangedFile) -> String {
    // An analyzed file's items are on its module's page; what is left here
    // is code that belongs to no item.
    let module = f
        .analyzed
        .then(|| {
            d.items()
                .find(|it| it.info.kind == Kind::Mod && it.info.file == f.path)
        })
        .flatten();
    let outside = if f.analyzed {
        let mut what = Vec::new();
        if !f.parts.is_empty() {
            what.push(format!("{}, one box each", parts_summary(&f.parts)));
        }
        if f.outside_items > 0 {
            what.push(format!(
                "{} other changed line{}",
                f.outside_items,
                if f.outside_items == 1 { "" } else { "s" }
            ));
        }
        format!(
            r#"<div class="note">This file's items and docs are shown {}. Below is only the changed code that belongs to no item: {}.</div>"#,
            match module {
                Some(m) => format!(
                    r#"on the page of <a data-mod="{}"{}><code>{}</code></a>"#,
                    esc(&m.info.id),
                    crate::nav::module_href(&m.info.id),
                    esc(&d.mod_label(&m.info.id))
                ),
                None => "with the Rust items".to_string(),
            },
            what.join(", and "),
        )
    } else {
        String::new()
    };
    let structured = match f.lang.as_str() {
        "python" | "javascript" | "yaml" | "rust" if !f.parts.is_empty() => {
            parts_html(d, &f.path, &f.parts, &f.lang)
        }
        "css" if !f.styles.is_empty() => css::styles_html(&f.styles),
        // Compared, and no property changed.
        "css" if !f.hunks.is_empty() && f.status == Status::Changed => {
            r#"<div class="note">No property changed: only comments, formatting or the order of rules (which can matter where selectors of equal specificity overlap).</div>"#
                .to_string()
        }
        "markdown" => match (&f.old_text, &f.new_text) {
            (Some(o), Some(n)) => markdown_html(d, o, n, Some(&f.path)),
            _ => String::new(),
        },
        // Listed under "Dependencies and package settings" already.
        _ if !f.lock.is_empty() => {
            r#"<div class="note">The packages whose locked versions changed are listed at the top.</div>"#
                .to_string()
        }
        _ if !f.manifest.is_empty() => {
            r#"<div class="note">Dependency and package changes are listed at the top.</div>"#
                .to_string()
        }
        _ => String::new(),
    };
    let lines = if f.lang == "binary" {
        r#"<div class="note">Binary file, not shown.</div>"#.to_string()
    } else if f.too_large {
        r#"<div class="note">Too large to compare line by line.</div>"#.to_string()
    } else if f.hunks.is_empty() {
        String::new()
    } else {
        let small = if f.analyzed {
            f.outside_items <= SMALL
        } else {
            f.added + f.removed <= SMALL && structured.is_empty()
        };
        // Hunks to review in the line diff: open, and named in its summary.
        let to_review = line_hunks_label(&f.path);
        format!(
            r#"<details class="src" {}><summary>{} {}{}</summary>{}</details>"#,
            if small || to_review.is_some() {
                "open"
            } else {
                ""
            },
            if f.analyzed && !f.parts.is_empty() {
                "Other lines outside items"
            } else if f.analyzed {
                "Lines outside items"
            } else if structured.is_empty() {
                "Diff"
            } else {
                "Line diff"
            },
            if f.analyzed {
                // The hunks' context lines are not part of the count.
                format!(r#"<span class="dim">{}</span>"#, f.outside_items)
            } else {
                stats(f.added, f.removed)
            },
            to_review
                .map(|l| format!(r#" <span class="dim rv-in">· {l}</span>"#))
                .unwrap_or_default(),
            diff_html(
                d,
                &f.hunks,
                &f.lang,
                &Diff {
                    path: Some(&f.path),
                    hunk: Some(&|k| {
                        let h = crate::review::file_hunk(&f.path, &format!("lines:{k}"))?;
                        Some(format!(
                            "{}{}",
                            crate::review::file_hunk_controls(&f.path, &h),
                            hunk_link(d, &f.path, &h)
                        ))
                    }),
                    ..Default::default()
                }
            )
        )
    };
    format!(
        r#"<div class="ientry file {}" data-file="{}"><div class="irow"><div class="ihead"><span class="title-name">{}</span><span class="kind">{}</span>{}{}</div><div class="iacts">{}{}</div></div>{outside}{structured}{lines}{}</div>"#,
        match f.status {
            Status::Added => "whole whole-added",
            Status::Removed => "whole whole-removed",
            _ => "",
        },
        esc(&f.path),
        esc(&f.path),
        esc(&f.lang),
        badge(f.status),
        f.generated
            .as_deref()
            .map(|g| format!(r#"<span class="st st-unchanged">{}</span>"#, esc(g)))
            .unwrap_or_default(),
        stats(f.added, f.removed),
        crate::review::control_html(&crate::review::file_unit(&f.path)),
        crate::threads::file_html(d, &f.path),
    )
}

/// The numeric parts of a version (`1.2.3-rc.1` gives `[1, 2, 3]`) and
/// whether it is a pre-release.
fn version_parts(v: &str) -> (Vec<u64>, bool) {
    let core = v.split('+').next().unwrap_or(v);
    let (core, pre) = match core.split_once('-') {
        Some((c, _)) => (c, true),
        None => (core, false),
    };
    let parts = core
        .split('.')
        .map(|p| {
            let digits: String = p.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().unwrap_or(0)
        })
        .collect();
    (parts, pre)
}

/// What an update from `old` to `new` is: a downgrade, or which part of
/// the version changed. For Cargo, by its semver rules: a change of the
/// leftmost non-zero part is breaking (`0.3` to `0.4` is), others are
/// compatible.
fn bump(old: &str, new: &str, cargo: bool) -> (&'static str, &'static str) {
    let ((a, a_pre), (b, b_pre)) = (version_parts(old), version_parts(new));
    let n = a.len().max(b.len()).max(3);
    let at = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    let Some(first) = (0..n).find(|&i| at(&a, i) != at(&b, i)) else {
        return match (a_pre, b_pre) {
            (true, false) => ("", "release of the pre-release"),
            (false, true) => ("lk-major", "to a pre-release"),
            _ => ("", "pre-release changed"),
        };
    };
    if at(&b, first) < at(&a, first) {
        return ("lk-down", "downgrade");
    }
    if cargo {
        let leftmost = (0..n).find(|&i| at(&a, i) != 0).unwrap_or(n - 1).min(2);
        if first <= leftmost {
            ("lk-major", "breaking")
        } else {
            ("", "compatible")
        }
    } else {
        match first {
            0 => ("lk-major", "major"),
            1 => ("", "minor"),
            _ => ("", "patch"),
        }
    }
}

/// A link that opens in a new tab.
fn ext_link(url: &str, label: &str, title: &str) -> String {
    format!(
        r#"<a href="{}" target="_blank" rel="noopener" title="{}">{label}</a>"#,
        esc(url),
        esc(title)
    )
}

/// A `Cargo.lock` package's role: workspace crate, direct or transitive
/// dependency.
fn dep_tag(c: &LockChange) -> &'static str {
    match c.dep {
        Some(DepKind::Local) => {
            r#" <span class="lk-dep" title="A crate of the workspace, or a path dependency">workspace</span>"#
        }
        Some(DepKind::Direct) => {
            r#" <span class="lk-dep lk-direct" title="A dependency of a crate of the workspace">direct</span>"#
        }
        Some(DepKind::Transitive) => {
            r#" <span class="lk-dep" title="Only a dependency of other dependencies">transitive</span>"#
        }
        None => "",
    }
}

/// "required by `a`, `b` and 3 more", for packages that aren't direct
/// dependencies of an updated version (`was` for removed packages).
fn dependents_note(c: &LockChange, was: bool) -> String {
    if c.dependents.is_empty() {
        return String::new();
    }
    const SHOWN: usize = 3;
    let mut names: Vec<String> = c
        .dependents
        .iter()
        .take(SHOWN)
        .map(|d| format!("<code>{}</code>", esc(d)))
        .collect();
    if c.dependents.len() > SHOWN {
        names.push(format!("{} more", c.dependents.len() - SHOWN));
    }
    let list = match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    };
    let what = if was {
        "was required by"
    } else {
        "required by"
    };
    format!(r#"<div class="note">{what} {list}</div>"#)
}

/// What an added crate does at build time, and its description.
fn crate_notes(c: &LockChange) -> String {
    let Some(info) = &c.info else {
        return String::new();
    };
    let mut out = String::new();
    if let Some(d) = &info.description {
        out.push_str(&format!(r#"<div class="note lk-desc">{}</div>"#, esc(d)));
    }
    let runs: Vec<&str> = [
        (info.build_script, "a build script"),
        (info.proc_macro, "a procedural macro"),
    ]
    .iter()
    .filter(|(on, _)| *on)
    .map(|(_, what)| *what)
    .collect();
    if !runs.is_empty() {
        out.push_str(&format!(
            r#"<div class="note lk-warn">runs code at build time: {}</div>"#,
            runs.join(" and ")
        ));
    }
    out
}

/// Links to review a crates.io package's update (`from` the old version)
/// or new version: the published code's diff or source on diff.rs, its
/// changelog on docs.rs or else its repository, and crates.io.
fn crate_links(c: &LockChange, from: Option<&str>, to: &str) -> String {
    if !c.crates_io {
        return String::new();
    }
    let name = &c.name;
    let mut links = Vec::new();
    match from {
        Some(from) => links.push(ext_link(
            &format!("https://diff.rs/{name}/{from}/{to}"),
            "diff",
            &format!("The published code of {name} {from} and {to} compared, on diff.rs"),
        )),
        None => links.push(ext_link(
            &format!("https://diff.rs/browse/{name}/{to}"),
            "source",
            &format!("The published code of {name} {to}, on diff.rs"),
        )),
    }
    let info = c.info.clone().unwrap_or_default();
    match (&info.changelog, &info.repository) {
        (Some(file), _) => links.push(ext_link(
            &format!("https://docs.rs/crate/{name}/{to}/source/{file}"),
            "changelog",
            &format!("{file} of {name} {to}, on docs.rs"),
        )),
        (None, Some(repo)) => links.push(ext_link(repo, "repository", repo)),
        (None, None) => {}
    }
    if from.is_none() {
        links.push(ext_link(
            &format!("https://crates.io/crates/{name}/{to}"),
            "crates.io",
            &format!("{name} {to} on crates.io"),
        ));
    }
    format!(r#"<div class="note lk-links">{}</div>"#, links.join(" · "))
}

/// How many dependencies were added and removed, direct and transitive,
/// for a `Cargo.lock`.
fn lock_summary(list: &[LockChange]) -> String {
    let count = |added: bool, kind: DepKind| {
        list.iter()
            .filter(|c| c.dep == Some(kind) && (if added { &c.old } else { &c.new }).is_empty())
            .count()
    };
    let part = |added: bool| {
        let (d, t) = (
            count(added, DepKind::Direct),
            count(added, DepKind::Transitive),
        );
        let mut out = Vec::new();
        if d > 0 {
            out.push(format!("{d} direct"));
        }
        if t > 0 {
            let s = format!("{t} transitive");
            // New code nobody asked for by name.
            out.push(if added {
                format!(r#"<span class="lk-warn">{s}</span>"#)
            } else {
                s
            });
        }
        out.join(", ")
    };
    let (added, removed) = (part(true), part(false));
    let mut parts = Vec::new();
    if !added.is_empty() {
        parts.push(format!("New dependencies: {added}"));
    }
    if !removed.is_empty() {
        parts.push(format!("removed: {removed}"));
    }
    if parts.is_empty() {
        return String::new();
    }
    format!(
        r#"<div class="note lk-summary">{}</div>"#,
        parts.join(" · ")
    )
}

/// A lock file's changed packages, grouped: updated, added, removed, and
/// those whose version stayed but whose source or checksum changed.
fn lock_html(path: &str, list: &[LockChange]) -> String {
    let cargo = path.rsplit('/').next() == Some("Cargo.lock");
    let code = |v: &[String]| {
        v.iter()
            .map(|x| format!("<code>{}</code>", esc(x)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let source_note = |c: &LockChange| match (&c.old_source, &c.new_source) {
        (Some(o), Some(n)) => format!(
            r#"<div class="note lk-warn">source: <del>{}</del> → {}</div>"#,
            esc(o),
            esc(n)
        ),
        (None, Some(n)) if !c.old.is_empty() => {
            format!(r#"<div class="note lk-warn">now from {}</div>"#, esc(n))
        }
        (None, Some(n)) => format!(r#"<div class="note lk-warn">from {}</div>"#, esc(n)),
        (Some(o), None) => format!(
            r#"<div class="note">back to the default registry (was {})</div>"#,
            esc(o)
        ),
        (None, None) => String::new(),
    };
    let checksum = |c: &LockChange| {
        if c.checksum_changed {
            r#"<div class="note lk-warn">checksum changed for the same version: the published package differs</div>"#
        } else {
            ""
        }
    };
    let mut groups: [(&str, Vec<String>); 4] = [
        ("Updated", Vec::new()),
        ("Added", Vec::new()),
        ("Removed", Vec::new()),
        ("Same version, other source or checksum", Vec::new()),
    ];
    for c in list {
        // Versions on only one side; one of each is an update.
        let gone: Vec<String> = c
            .old
            .iter()
            .filter(|v| !c.new.contains(v))
            .cloned()
            .collect();
        let came: Vec<String> = c
            .new
            .iter()
            .filter(|v| !c.old.contains(v))
            .cloned()
            .collect();
        let name = format!(
            r#"<td class="mf-name"><code>{}</code>{}</td>"#,
            esc(&c.name),
            dep_tag(c)
        );
        let notes = format!("{}{}", source_note(c), checksum(c));
        let transitive = c.dep == Some(DepKind::Transitive);
        let (group, row) = if c.old.is_empty() {
            (
                1,
                format!(
                    r#"<tr class="m-add">{name}<td class="mf-old"></td><td class="mf-new">{}</td><td>{}{notes}{}{}</td></tr>"#,
                    code(&c.new),
                    crate_notes(c),
                    dependents_note(c, false),
                    c.new
                        .last()
                        .map(|v| crate_links(c, None, v))
                        .unwrap_or_default(),
                ),
            )
        } else if c.new.is_empty() {
            (
                2,
                format!(
                    r#"<tr class="m-del">{name}<td class="mf-old"><del>{}</del></td><td class="mf-new"></td><td>{notes}{}</td></tr>"#,
                    code(&c.old),
                    dependents_note(c, true),
                ),
            )
        } else if gone.is_empty() && came.is_empty() {
            (
                3,
                format!(
                    r#"<tr class="m-chg">{name}<td class="mf-old"></td><td class="mf-new">{}</td><td>{notes}</td></tr>"#,
                    code(&c.new)
                ),
            )
        } else {
            let (what, links) = match (gone.as_slice(), came.as_slice()) {
                ([o], [n]) => {
                    let (class, label) = bump(o, n, cargo);
                    (
                        format!(r#"<span class="lk-bump {class}">{label}</span>"#),
                        crate_links(c, Some(o), n),
                    )
                }
                // From several versions to one: compared with the newest.
                ([.., o], [n]) => {
                    let (class, label) = bump(o, n, cargo);
                    (
                        format!(
                            r#"<span class="lk-bump {class}">{label}</span><div class="note">from {} versions to one</div>"#,
                            gone.len()
                        ),
                        crate_links(c, Some(o), n),
                    )
                }
                ([], [.., n]) => (
                    r#"<span class="lk-bump">another version added</span>"#.to_string(),
                    crate_links(c, None, n),
                ),
                (_, []) => (
                    r#"<span class="lk-bump">a version dropped</span>"#.to_string(),
                    String::new(),
                ),
                _ => (String::new(), String::new()),
            };
            // Why a transitive dependency is there.
            let why = if transitive {
                dependents_note(c, false)
            } else {
                String::new()
            };
            (
                0,
                format!(
                    r#"<tr class="m-chg">{name}<td class="mf-old"><del>{}</del></td><td class="mf-new">{}</td><td>{what}{notes}{why}{links}</td></tr>"#,
                    code(&c.old),
                    code(&c.new)
                ),
            )
        };
        groups[group].1.push(row);
    }
    let counts: Vec<String> = groups
        .iter()
        .take(3)
        .filter(|g| !g.1.is_empty())
        .map(|g| format!("{} {}", g.1.len(), g.0.to_lowercase()))
        .collect();
    let mut table = String::from(r#"<table class="manifest lock">"#);
    for (title, rows) in &groups {
        if rows.is_empty() {
            continue;
        }
        table.push_str(&format!(
            r#"<tr><th colspan="4" class="mf-sec">{title}</th></tr>{}"#,
            rows.concat()
        ));
    }
    table.push_str("</table>");
    let summary = if cargo {
        lock_summary(list)
    } else {
        String::new()
    };
    // Long lists (a `cargo update`) are folded.
    if list.len() > 25 {
        format!(
            r#"{summary}<details class="lock-all"><summary>{} packages changed: {}</summary>{table}</details>"#,
            list.len(),
            counts.join(", ")
        )
    } else {
        format!("{summary}{table}")
    }
}

/// `Cargo.toml` and lock file changes, shown first in the overview.
pub fn manifests_html(d: &Data) -> String {
    let list: Vec<&ChangedFile> = d
        .out
        .files
        .iter()
        .filter(|f| {
            (!f.manifest.is_empty() || !f.lock.is_empty()) && crate::commit_filter::file_in(&f.path)
        })
        .collect();
    if list.is_empty() {
        return String::new();
    }
    let cards: String = list
        .iter()
        .map(|f| {
            // The same review mark as the file's entry under "Changed
            // files".
            let (kind, body) = if f.lock.is_empty() {
                ("manifest", manifest_html(&f.manifest))
            } else {
                ("lock file", lock_html(&f.path, &f.lock))
            };
            format!(
                r#"<div class="ientry file"><div class="irow"><div class="ihead"><span class="title-name">{}</span><span class="kind">{kind}</span></div><div class="iacts">{}</div></div>{body}</div>"#,
                esc(&f.path),
                crate::review::control_html(&crate::review::file_unit(&f.path)),
            )
        })
        .collect();
    format!(
        r#"<h2>Dependencies and package settings {}</h2>{cards}"#,
        crate::pages::help_html("Changes in Cargo.toml and lock files.")
    )
}

/// The groups of the overview's "Changed files", in their order.
const GROUPS: [&str; 8] = [
    "Rust code outside items",
    "Python",
    "JavaScript",
    "CSS",
    "YAML",
    "Docs",
    "Package files",
    "Other files",
];

/// The group of a changed file under "Changed files": its language or
/// kind.
fn file_group(f: &ChangedFile) -> &'static str {
    if f.analyzed || f.lang == "rust" {
        return GROUPS[0];
    }
    if !f.manifest.is_empty() || !f.lock.is_empty() {
        return "Package files";
    }
    match f.lang.as_str() {
        "python" => "Python",
        "javascript" => "JavaScript",
        "css" => "CSS",
        "yaml" => "YAML",
        "markdown" => "Docs",
        _ => "Other files",
    }
}

/// Where a file comes under "Changed files": generated files last, the
/// others by group. The review order follows it.
pub fn file_rank(f: &ChangedFile) -> (bool, usize) {
    let group = file_group(f);
    (
        f.generated.is_some(),
        GROUPS
            .iter()
            .position(|g| *g == group)
            .unwrap_or(GROUPS.len()),
    )
}

/// Every changed file that the item sections don't show in full, grouped
/// by language.
pub fn other_files_html(d: &Data) -> String {
    // Rust files count as covered unless some of their changes are outside
    // every item.
    let others: Vec<&ChangedFile> = d
        .out
        .files
        .iter()
        .filter(|f| !f.analyzed || f.outside_items > 0 || !f.parts.is_empty())
        .filter(|f| crate::commit_filter::file_in(&f.path))
        .collect();
    let (generated, mut own): (Vec<&ChangedFile>, Vec<&ChangedFile>) =
        others.into_iter().partition(|f| f.generated.is_some());
    own.sort_by_key(|f| file_rank(f));
    if own.is_empty() && generated.is_empty() {
        return String::new();
    }
    let mut html = format!(
        r#"<h2>Changed files {}</h2>"#,
        crate::pages::help_html(
            "Each changed file, by language, except Rust code that the sections above show as items."
        )
    );
    // Group headings only when there is more than one group and some group
    // has several files: otherwise each card's kind label says as much.
    let groups: BTreeSet<&str> = own.iter().map(|f| file_group(f)).collect();
    let mut last = None;
    for f in &own {
        let group = file_group(f);
        if groups.len() > 1 && own.len() > groups.len() && last != Some(group) {
            let n = own.iter().filter(|o| file_group(o) == group).count();
            html.push_str(&format!(
                r#"<h3 class="files-group">{group} <span class="h-note">{n}</span></h3>"#
            ));
        }
        last = Some(group);
        html.push_str(&file_card(d, f));
    }
    if !generated.is_empty() {
        html.push_str(&format!(
            r#"<details class="generated"><summary>{} generated or vendored file{} ({})</summary>{}</details>"#,
            generated.len(),
            if generated.len() == 1 { "" } else { "s" },
            generated
                .iter()
                .filter_map(|f| f.generated.as_deref())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(", "),
            generated.iter().map(|f| file_card(d, f)).collect::<String>()
        ));
    }
    html
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_bumps() {
        assert_eq!(bump("1.0.200", "1.0.210", true).1, "compatible");
        assert_eq!(bump("1.4.0", "2.0.0", true).1, "breaking");
        assert_eq!(bump("0.3.9", "0.4.0", true).1, "breaking");
        assert_eq!(bump("0.3.9", "0.3.10", true).1, "compatible");
        assert_eq!(bump("0.0.3", "0.0.4", true).1, "breaking");
        assert_eq!(bump("2.31.0", "2.32.0", false).1, "minor");
        assert_eq!(bump("1.2.0", "1.1.9", true).1, "downgrade");
        assert_eq!(
            bump("1.0.0-rc.1", "1.0.0", true).1,
            "release of the pre-release"
        );
    }

    /// A changed file with its line diff, as the analysis makes it.
    fn changed(path: &str, lang: &str, old: &str, new: &str) -> ChangedFile {
        let side = |t: &str| crate::since::Side {
            lines: t.lines().map(str::to_string).collect(),
            start: 1,
            nos: Vec::new(),
        };
        ChangedFile {
            path: path.into(),
            status: Status::Changed,
            lang: lang.into(),
            hunks: crate::since::interdiff(&side(old), &side(new)),
            ..Default::default()
        }
    }

    fn keys(list: &[crate::review::Hunk]) -> Vec<(&str, &str, &[usize], &[usize])> {
        list.iter()
            .map(|h| (h.key.as_str(), h.name.as_str(), &h.old[..], &h.new[..]))
            .collect()
    }

    #[test]
    fn markdown_hunks_are_sections() {
        let old = "Intro.\n\n# A\n\nalpha\n\n# B\n\nbeta\n";
        let new = "Intro.\n\n# A\n\nalpha2\n\n# B\n\nbeta2\n";
        let mut f = changed("README.md", "markdown", old, new);
        f.old_text = Some(old.into());
        f.new_text = Some(new.into());
        let h = review_hunks(&f);
        assert_eq!(
            keys(&h),
            [
                ("sec:A", "section A", &[5][..], &[5][..]),
                ("sec:B", "section B", &[9][..], &[9][..]),
            ]
        );
        // A later push changes the intro and moves both sections down:
        // they keep their ids, so their marks stay.
        let later = format!("Intro,\nlonger.\n{}", &new["Intro.\n".len()..]);
        let mut g = changed("README.md", "markdown", old, &later);
        g.old_text = Some(old.into());
        g.new_text = Some(later);
        let h2 = review_hunks(&g);
        assert_eq!(h2.len(), 3);
        assert_eq!(h2[1].id, h[0].id);
        assert_eq!(h2[2].id, h[1].id);
        assert_eq!(h2[2].new, [10]);
    }

    #[test]
    fn other_files_are_reviewed_by_the_hunks_of_their_diff() {
        let old: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let new = old
            .replace("line 3\n", "line three\n")
            .replace("line 25\n", "line 25\nline 25b\n");
        let f = changed("notes.txt", "text", &old, &new);
        let h = review_hunks(&f);
        assert_eq!(
            keys(&h),
            [
                ("lines:0", "line 3", &[3][..], &[3][..]),
                ("lines:1", "line 26", &[][..], &[26][..]),
            ]
        );
        assert!(h[0].id.starts_with("file:notes.txt#"));
        // One change: the file's own verdict is enough.
        let one = changed("notes.txt", "text", &old, &old.replace("line 3\n", "x\n"));
        assert!(review_hunks(&one).is_empty());
        // Added files have no hunks.
        let mut added = f.clone();
        added.status = Status::Added;
        assert!(review_hunks(&added).is_empty());
    }

    #[test]
    fn the_line_diff_names_its_hunks() {
        let old: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        let new = old
            .replace("line 3\n", "line three\n")
            .replace("line 25\n", "line 25\nline 25b\n");
        let f = changed("notes.txt", "text", &old, &new);
        crate::review::set_hunks_for_test(&f.path, review_hunks(&f));
        assert_eq!(
            line_hunks_label(&f.path).as_deref(),
            Some("changes 1 and 2 of 2")
        );
        let mut h = review_hunks(&f);
        h[0].key = "part:0".into();
        crate::review::set_hunks_for_test(&f.path, h);
        assert_eq!(line_hunks_label(&f.path).as_deref(), Some("change 2 of 2"));
        crate::review::set_hunks_for_test(&f.path, Vec::new());
        assert_eq!(line_hunks_label(&f.path), None);
    }

    #[test]
    fn boxes_hold_their_lines_and_the_rest_are_line_hunks() {
        // A YAML file whose `on` key is a box; the change at line 20 is in
        // no box.
        let old: String = (1..=30).map(|i| format!("k{i}: {i}\n")).collect();
        let new = old
            .replace("k2: 2\n", "k2: 3\n")
            .replace("k20: 20\n", "k20: x\n");
        let mut f = changed("ci.yml", "yaml", &old, &new);
        f.parts = vec![PartChange {
            name: "k2".into(),
            kind: "key".into(),
            status: Status::Changed,
            line: 2,
            lines: vec![
                SourceLine('@', "2 2".into()),
                SourceLine('-', "k2: 2".into()),
                SourceLine('+', "k2: 3".into()),
            ],
            ..Default::default()
        }];
        let h = review_hunks(&f);
        assert_eq!(
            keys(&h),
            [
                ("part:0", "key k2", &[2][..], &[2][..]),
                ("lines:1", "line 20", &[20][..], &[20][..]),
            ]
        );
    }

    #[test]
    fn markdown_sections_follow_headings_outside_fences() {
        let md = "Intro.\n\n# Usage\n\nRun it.\n\n```sh\n# not a heading\n```\n\n## Flags\n\nSome.\n\nTitle\n=====\n\nText.\n";
        let s = sections(md);
        let keys: Vec<&str> = s.iter().map(|x| x.key.as_str()).collect();
        assert_eq!(keys, ["", "Usage", "Usage › Flags", "Title"]);
        assert!(s[1].text.contains("# not a heading"));
        let spans: Vec<(usize, usize)> = s.iter().map(|x| (x.first, x.last)).collect();
        assert_eq!(spans, [(1, 2), (3, 10), (11, 14), (15, 18)]);
    }
}
