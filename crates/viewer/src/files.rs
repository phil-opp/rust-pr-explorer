//! Files the Rust analysis doesn't cover: `Cargo.toml` changes, Python
//! definitions, YAML keys, Markdown sections, and plain line diffs for the
//! rest. For analyzed Rust files, the code that belongs to no item: tests,
//! macro calls and `extern` blocks as parts, and the remaining lines.

use crate::code::{
    Links, Seg, code_html, esc, mark_changed_tokens, markdown_diff_segs, markdown_file_html,
    marked_html, sig_diff_html,
};
use crate::data::Data;
use crate::yaml;
use model::{ChangedFile, Kind, ManifestChange, PartChange, SourceLine, Status};
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

/// Hunks of a line diff. Rust, Python and YAML get syntax colors; nothing
/// is linked.
pub fn hunks_html(d: &Data, lines: &[SourceLine], lang: &str) -> String {
    let mut new_line = 0usize;
    let mut old_line = 0usize;
    // YAML block scalars span lines; each side keeps its own state.
    let (mut old_yaml, mut new_yaml) = (yaml::State::default(), yaml::State::default());
    let mut body = String::new();
    let mut codes: Vec<String> = Vec::with_capacity(lines.len());
    for l in lines {
        if l.0 == '@' {
            (old_yaml, new_yaml) = (yaml::State::default(), yaml::State::default());
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
            _ => esc(&l.1),
        });
    }
    // Removed lines directly followed by added ones pair up (the first
    // with the first, and so on) and get their changed words marked.
    let mut i = 0;
    while i < lines.len() {
        let dels = lines[i..].iter().take_while(|l| l.0 == '-').count();
        let adds = lines[i + dels..].iter().take_while(|l| l.0 == '+').count();
        for k in 0..dels.min(adds) {
            let (o, n) = (i + k, i + dels + k);
            if let Some((a, b)) = mark_changed_tokens(&codes[o], &codes[n]) {
                codes[o] = a;
                codes[n] = b;
            }
        }
        i += (dels + adds).max(1);
    }
    for (l, code) in lines.iter().zip(codes) {
        if l.0 == '@' {
            let mut it = l.1.split(' ').filter_map(|n| n.parse::<usize>().ok());
            old_line = it.next().unwrap_or(1);
            new_line = it.next().unwrap_or(1);
            body.push_str(&format!(
                r#"<span class="ln skip">⋯ line {new_line}</span>"#
            ));
            continue;
        }
        let (class, no) = match l.0 {
            '+' => ("add", new_line),
            '-' => ("del", old_line),
            _ => ("", new_line),
        };
        match l.0 {
            '+' => new_line += 1,
            '-' => old_line += 1,
            _ => {
                new_line += 1;
                old_line += 1;
            }
        }
        body.push_str(&format!(
            r#"<span class="ln {class}" data-t="{}" data-no="{no}">{}</span>"#,
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
    }
    format!(r#"<pre class="hunks">{body}</pre>"#)
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

/// A file's changed parts (see [`PartChange`]), one box each.
fn parts_html(d: &Data, list: &[PartChange], lang: &str) -> String {
    let no_refs = BTreeMap::new();
    let code = lang != "yaml";
    list.iter()
        .map(|c| {
            if c.kind == "module" {
                return r#"<div class="irow member part"><div class="ihead"><span class="kind">module</span><span class="title-name">top-level code</span></div><div class="mark m-chg"><div class="note">Statements outside functions and classes changed; see the line diff.</div></div></div>"#.to_string();
            }
            let sig = match &c.old_sig {
                Some(old) if code => format!(
                    r#"<pre class="sig sig-diff">{}</pre>"#,
                    sig_diff_html(d, old, &c.sig, &no_refs, &[])
                ),
                Some(old) => format!(
                    r#"<pre class="sig"><del>{}</del>
{}</pre>"#,
                    yaml::line_html(&mut yaml::State::default(), old),
                    yaml::line_html(&mut yaml::State::default(), &c.sig)
                ),
                None if code => {
                    format!(r#"<pre class="sig">{}</pre>"#, code_html(d, &c.sig, Links::None))
                }
                None => format!(r#"<pre class="sig">{}</pre>"#, yaml::line_html(&mut yaml::State::default(), &c.sig)),
            };
            let (sig_bar, sig_class) = match c.status {
                Status::Added | Status::Removed => ("", ""),
                _ if c.old_sig.is_some() => ("m-chg", ""),
                _ => ("", "m-ref"),
            };
            let mut segs = Vec::new();
            // A YAML key's line repeats its name, unless it changed.
            if code || c.old_sig.is_some() {
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
                        if lang == "python" { "Docstring" } else { "Doc comment" }
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
            let small = c.status == Status::Changed && added + removed <= 4;
            segs.push(Seg::new(
                bar,
                format!(
                    r#"<details class="src {}" {}><summary>{what} {} · line {}</summary>{}</details>"#,
                    if bar.is_empty() { "" } else { "src-chg" },
                    if small { "open" } else { "" },
                    stats(added, removed),
                    c.line,
                    hunks_html(d, &c.lines, lang)
                ),
            ));
            let status_class = match c.status {
                Status::Added => "added",
                Status::Removed => "removed",
                _ => "",
            };
            format!(
                r#"<div class="irow member part {status_class}"><div class="ihead"><span class="kind">{}</span><span class="title-name">{}</span>{}</div>{}</div>"#,
                esc(&c.kind),
                esc(&c.name),
                badge(c.status),
                marked_html(segs)
            )
        })
        .collect()
}

/// Markdown split at headings (outside code fences): (key, heading line,
/// text). The key is the heading path, made unique.
fn sections(md: &str) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> =
        vec![(String::new(), String::new(), String::new())];
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
            out.push((key, heading, String::new()));
            i += if setext.is_some() { 2 } else { 1 };
            continue;
        }
        let last = out.last_mut().unwrap();
        last.2.push_str(line);
        last.2.push('\n');
        i += 1;
    }
    if out[0].2.trim().is_empty() {
        out.remove(0);
    }
    out
}

/// A Markdown file compared section by section: changed sections show
/// their changes (paragraph and word level), unchanged ones are counted.
pub fn markdown_html(d: &Data, old: &str, new: &str) -> String {
    let (a, b) = (sections(old), sections(new));
    let old_by_key: BTreeMap<&str, &(String, String, String)> =
        a.iter().map(|s| (s.0.as_str(), s)).collect();
    let new_keys: BTreeSet<&str> = b.iter().map(|s| s.0.as_str()).collect();
    let heading = |h: &str, key: &str| {
        let title = h
            .lines()
            .next()
            .unwrap_or("")
            .trim_start_matches('#')
            .trim();
        let title = if title.is_empty() {
            "(before the first heading)"
        } else {
            title
        };
        format!(
            r#"<div class="md-h" title="{}">{}</div>"#,
            esc(key),
            esc(title)
        )
    };
    let mut segs: Vec<Seg> = Vec::new();
    let mut unchanged = 0;
    for s in &b {
        match old_by_key.get(s.0.as_str()) {
            Some(o) if o.1 == s.1 && o.2 == s.2 => unchanged += 1,
            Some(o) => {
                let head_changed = o.1 != s.1;
                segs.push(Seg::new(
                    if head_changed { "m-chg" } else { "" },
                    heading(&s.1, &s.0),
                ));
                segs.extend(markdown_diff_segs(d, &o.2, &s.2));
            }
            None => {
                let body = markdown_file_html(d, &s.2);
                segs.push(Seg::new(
                    "m-add",
                    format!(r#"{}<div class="docs">{body}</div>"#, heading(&s.1, &s.0)),
                ));
            }
        }
    }
    for o in a.iter().filter(|o| !new_keys.contains(o.0.as_str())) {
        segs.push(Seg::new(
            "m-del",
            format!(r#"<div class="md-removed">{}</div>"#, heading(&o.1, &o.0)),
        ));
    }
    let mut html = marked_html(segs);
    if unchanged > 0 {
        html.push_str(&format!(
            r#"<div class="collapsed-note">{unchanged} unchanged section{}</div>"#,
            if unchanged == 1 { "" } else { "s" }
        ));
    }
    html
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
                    r#"on the page of <a data-mod="{}"><code>{}</code></a>"#,
                    esc(&m.info.id),
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
        "python" | "yaml" | "rust" if !f.parts.is_empty() => parts_html(d, &f.parts, &f.lang),
        "markdown" => match (&f.old_text, &f.new_text) {
            (Some(o), Some(n)) => markdown_html(d, o, n),
            _ => String::new(),
        },
        // Listed under "Dependencies and package settings" already.
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
        format!(
            r#"<details class="src" {}><summary>{} {}</summary>{}</details>"#,
            if small { "open" } else { "" },
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
            hunks_html(d, &f.hunks, &f.lang)
        )
    };
    format!(
        r#"<div class="ientry file {}" data-file="{}"><div class="irow"><div class="ihead"><span class="title-name">{}</span><span class="kind">{}</span>{}{}</div><div class="iacts">{}{}</div></div>{outside}{structured}{lines}</div>"#,
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
    )
}

/// `Cargo.toml` changes, shown first in the overview.
pub fn manifests_html(d: &Data) -> String {
    let list: Vec<&ChangedFile> = d
        .out
        .files
        .iter()
        .filter(|f| !f.manifest.is_empty())
        .collect();
    if list.is_empty() {
        return String::new();
    }
    let cards: String = list
        .iter()
        .map(|f| {
            // The same review mark as the file's entry under "Other
            // changed files".
            format!(
                r#"<div class="ientry file"><div class="irow"><div class="ihead"><span class="title-name">{}</span><span class="kind">manifest</span></div><div class="iacts">{}</div></div>{}</div>"#,
                esc(&f.path),
                crate::review::control_html(&crate::review::file_unit(&f.path)),
                manifest_html(&f.manifest)
            )
        })
        .collect();
    format!(
        r#"<h2>Dependencies and package settings {}</h2>{cards}"#,
        crate::pages::help_html(
            "Changes in Cargo.toml: dependencies, features, workspace members, and the \
             package's version, edition and minimum Rust version."
        )
    )
}

/// Every changed file the Rust analysis doesn't cover.
pub fn other_files_html(d: &Data) -> String {
    // Rust files count as covered unless some of their changes are outside
    // every item.
    let others: Vec<&ChangedFile> = d
        .out
        .files
        .iter()
        .filter(|f| !f.analyzed || f.outside_items > 0 || !f.parts.is_empty())
        .collect();
    let covered = d
        .out
        .files
        .iter()
        .filter(|f| f.analyzed && f.outside_items == 0 && f.parts.is_empty())
        .count();
    let (generated, own): (Vec<&ChangedFile>, Vec<&ChangedFile>) =
        others.into_iter().partition(|f| f.generated.is_some());
    if own.is_empty() && generated.is_empty() {
        return String::new();
    }
    let mut html = format!(
        r#"<h2>Other changed files {}</h2>"#,
        crate::pages::help_html(&format!(
            "Files the Rust analysis doesn't cover{}. Python files are compared by function \
             and class, YAML files by key, Markdown files by section, and Rust code outside \
             items (tests, macro calls, extern blocks) one item at a time; other files line \
             by line.",
            if covered > 0 {
                format!(
                    " ({covered} changed Rust file{} are shown as items above)",
                    if covered == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            }
        ))
    );
    html.extend(own.iter().map(|f| file_card(d, f)));
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
    fn markdown_sections_follow_headings_outside_fences() {
        let md = "Intro.\n\n# Usage\n\nRun it.\n\n```sh\n# not a heading\n```\n\n## Flags\n\nSome.\n\nTitle\n=====\n\nText.\n";
        let s = sections(md);
        let keys: Vec<&str> = s.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(keys, ["", "Usage", "Usage › Flags", "Title"]);
        assert!(s[1].2.contains("# not a heading"));
    }
}
