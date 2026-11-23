use super::docs_diff::{docs_diff_html, mark_changed_tokens, word_diff_html};
use super::markdown::doc_blocks;
use super::*;
use crate::data::Data;
use model::{DiffSpan, Ref, SigDiff};
use model::{Meta, Output};
use std::collections::BTreeMap;

fn empty() -> Data {
    Data::new(Output {
        meta: Meta {
            title: String::new(),
            base: String::new(),
            head: String::new(),
            url: None,
            resolution: String::new(),
            pr: None,
            project: None,
        },
        crates: vec!["c".into()],
        items: BTreeMap::new(),
        externals: BTreeMap::new(),
        files: Vec::new(),
        warnings: Vec::new(),
    })
}

#[test]
fn docs_never_contain_markup_from_the_source() {
    let d = empty();
    let html = docs_html(
        &d,
        "Hi <script>alert(1)</script> and <img src=x onerror=y>\n\n[x](javascript:alert(1))",
        &BTreeMap::new(),
    );
    assert!(!html.contains("<script"), "{html}");
    assert!(!html.contains("<img"), "{html}");
    assert!(!html.contains("javascript:"), "{html}");
    assert!(html.contains("&lt;script&gt;"), "{html}");
}

#[test]
fn intra_doc_links_and_code_blocks() {
    let d = empty();
    let mut refs = BTreeMap::new();
    refs.insert(
        "`Vec`".to_string(),
        Ref {
            id: None,
            path: "alloc::vec::Vec".into(),
        },
    );
    let html = docs_html(
        &d,
        "Uses [`Vec`] and [`Missing`].\n\n```\nlet x = 1;\n# hidden();\n```",
        &refs,
    );
    assert!(html.contains(r#"data-path="alloc::vec::Vec""#), "{html}");
    assert!(html.contains("<code>Missing</code>"), "{html}");
    assert!(!html.contains("[<code>Missing"), "{html}");
    assert!(!html.contains("hidden"), "{html}");
    assert!(html.contains(r#"<span class="kw">let</span>"#), "{html}");
}

#[test]
fn highlighting() {
    let d = empty();
    let html = code_html(
        &d,
        "fn f<'a>(c: char) -> &'a str { '\\n'; \"<b>\" } // x",
        Links::None,
    );
    assert!(html.contains(r#"<span class="kw">'a</span>"#), "{html}");
    assert!(html.contains(r#"<span class="str">'\n'</span>"#), "{html}");
    assert!(
        html.contains(r#"<span class="str">&quot;&lt;b&gt;&quot;</span>"#),
        "{html}"
    );
    assert!(html.contains(r#"<span class="cm">// x</span>"#), "{html}");
}

#[test]
fn signature_diff() {
    let d = empty();
    let html = sig_diff_html(
        &d,
        "pub fn calc(input: &str) -> f64",
        "pub fn calc(input: &str, env: &Env) -> Result<f64>",
        &BTreeMap::new(),
        &[],
    );
    assert!(
        html.contains(r#"<ins class="sd" data-g="2">, env: &amp;Env</ins>"#),
        "{html}"
    );
    // Unchanged text is not marked.
    assert!(html.starts_with(r#"<span class="kw">pub</span>"#), "{html}");
    // Both halves of a replacement share a group, for linked hover.
    let replaced = sig_diff_html(&d, "fn f(x: u8)", "fn f(x: u16)", &BTreeMap::new(), &[]);
    assert!(
        replaced
            .contains(r#"<del class="sd" data-g="2">u8</del><ins class="sd" data-g="2">u16</ins>"#),
        "{replaced}"
    );
}

#[test]
fn structured_signature_diff() {
    let d = empty();
    let diff = SigDiff {
        spans: vec![
            DiffSpan::Same {
                text: "pub enum E {\n    A,\n".into(),
            },
            DiffSpan::Removed {
                text: "    B,\n".into(),
                g: 1,
            },
            DiffSpan::Same {
                text: "    C(".into(),
            },
            DiffSpan::Wrap {
                before: "Option<".into(),
                inner: vec![DiffSpan::Same { text: "u8".into() }],
                after: ">".into(),
                unwrap: false,
                g: 2,
            },
            DiffSpan::Same {
                text: "),\n".into(),
            },
            DiffSpan::Added {
                text: "    D,\n".into(),
                g: 3,
            },
            DiffSpan::Same { text: "}".into() },
        ],
        notes: vec![model::DiffNote {
            text: "variant `B` removed".into(),
            groups: vec![1],
        }],
    };
    let html = structured_diff_html(&d, &diff, &BTreeMap::new(), &[]);
    assert!(html.contains(r#"<span class="wrap" data-g="2""#), "{html}");
    assert!(
        html.contains(r#"<ins class="edge">Option&lt;</ins>u8<ins class="edge">&gt;</ins>"#),
        "{html}"
    );
    assert!(
        html.contains(r#"<li data-gs="1">variant <code>B</code> removed</li>"#),
        "{html}"
    );
    assert!(!html.contains("sig-gutter"), "{html}");
}

#[test]
fn method_links_by_position_only() {
    let d = empty();
    let mut sites = BTreeMap::new();
    sites.insert(
        "7:10".to_string(),
        Ref {
            id: None,
            path: "c::A::get".into(),
        },
    );
    // Line 7: `a.get()` (name at column 10) is linked; `b.get()` is not.
    let html = code_html(
        &d,
        "    let a.get() + b.get();",
        Links::Source {
            line: 7,
            refs: &sites,
        },
    );
    assert_eq!(html.matches("c::A::get").count(), 1, "{html}");
    assert!(
        html.contains(r#".<span class="ref ext " data-path="c::A::get">get</span>"#),
        "{html}"
    );
}

#[test]
fn signature_links_only_at_recorded_offsets() {
    let d = empty();
    let mut refs = BTreeMap::new();
    refs.insert(
        "Op".to_string(),
        Ref {
            id: None,
            path: "c::Op".into(),
        },
    );
    // `Op(Op)`: only the type (offset 3) is a path.
    let html = code_html(
        &d,
        "Op(Op)",
        Links::Sig {
            refs: &refs,
            at: &[3],
            base: 0,
        },
    );
    assert_eq!(
        html,
        r#"Op(<span class="ref ext " data-path="c::Op">Op</span>)"#
    );
}

#[test]
fn module_containment() {
    use crate::data::within;
    assert!(within("a::b", "a"));
    assert!(within("a", "a"));
    assert!(!within("ab", "a"));
    assert!(!within("a", "a::b"));
}

#[test]
fn doc_blocks_keep_code_fences_whole() {
    let md =
        "Intro.\n\n```\nlet a = 1;\n\nlet b = 2;\n```\n\nSee [x].\n\n[x]: https://example.com\n";
    let (blocks, defs) = doc_blocks(md);
    assert_eq!(
        blocks,
        [
            "Intro.\n",
            "```\nlet a = 1;\n\nlet b = 2;\n```\n",
            "See [x].\n",
            "[x]: https://example.com\n"
        ]
    );
    assert_eq!(defs, "[x]: https://example.com\n");
}

#[test]
fn docs_diff_marks_blocks() {
    let d = empty();
    let html = docs_diff_html(
        &d,
        "Same.\n\nOld words.\n\nGone.",
        "Same.\n\nNew text.\n\nGone.\n\nAdded, see [x].\n\n[x]: https://example.com",
        &BTreeMap::new(),
    );
    assert!(
        html.starts_with(r#"<div class="docs"><div class="dblk"><p>Same.</p>"#),
        "{html}"
    );
    assert!(
        html.contains(
            r#"<div class="mark m-chg"><div class="docs"><div class="dblk"><p>New text.</p>"#
        ),
        "{html}"
    );
    assert!(html.contains(r#"<details class="dblk-old"><summary>previous text</summary><div class="mark m-del"><div class="docs"><div class="dblk"><p>Old words.</p>"#), "{html}");
    // The link definition applies to the block that uses it, and the
    // two added blocks share one marker.
    assert!(
        html.contains(r#"<div class="mark m-add"><div class="docs"><div class="dblk"><p>Added, see <a href="https://example.com""#),
        "{html}"
    );
    assert!(
        html.ends_with(r#"<div class="docs"><div class="dblk"><code>[x]: https://example.com</code></div></div></div>"#),
        "{html}"
    );
    assert_eq!(html.matches("mark m-add").count(), 1);
}

#[test]
fn word_diff_marks_changed_words() {
    let wd = |a: &str, b: &str| word_diff_html(a, b);
    assert_eq!(
        wd(
            "<p>Evaluates an expression.</p>",
            "<p>Evaluates an arithmetic expression.</p>"
        ),
        Some(r#"<p>Evaluates an <ins class="wd">arithmetic</ins> expression.</p>"#.into())
    );
    // A rewritten phrase is one removal and one insertion.
    assert_eq!(
        wd(
            "<p>Tokenizes <code>input</code> and creates a parser for it.</p>",
            "<p>Tokenizes <code>input</code> and creates a parser positioned at its first token.</p>"
        ),
        Some(
            r#"<p>Tokenizes <code>input</code> and creates a parser <del class="wd">for it</del><ins class="wd">positioned at its first token</ins>.</p>"#
                .into()
        )
    );
    // Mostly rewritten: block level instead.
    assert_eq!(wd("<p>One two three.</p>", "<p>Four five six.</p>"), None);
    // Code blocks are left to the block level.
    assert_eq!(wd("<pre>a b c</pre>", "<pre>a b d</pre>"), None);
    // Line breaks are spaces: rewrapping is no change.
    assert_eq!(
        wd("<p>a b\nc d</p>", "<p>a\nb c d e</p>"),
        Some("<p>a\nb c d <ins class=\"wd\">e</ins></p>".into())
    );
    // Changes in and around code spans; removed code stays code.
    assert_eq!(
        wd(
            "<p>re-exported with <code>pub use</code> appears here</p>",
            "<p>re-exported with <code>pub use</code>s appears here</p>"
        ),
        Some(
            r#"<p>re-exported with <code>pub use</code><ins class="wd">s</ins> appears here</p>"#
                .into()
        )
    );
    assert_eq!(
        wd("<p>x <code>pub use</code> y z w</p>", "<p>x y z w</p>"),
        Some(r#"<p>x <del class="wd"><code>pub use</code></del>y z w</p>"#.into())
    );
    // A changed link target or emphasis marks the word it applies to.
    assert_eq!(
        wd(
            r#"<p>See <a href="a.html">x</a> now.</p>"#,
            r#"<p>See <a href="b.html">x</a> now.</p>"#
        ),
        Some(r#"<p>See <del class="wd">x</del><a href="b.html"><ins class="wd">x</ins></a> now.</p>"#.into())
    );
    assert_eq!(
        wd(
            "<p>A <em>b</em> c d e.</p>",
            "<p>A <strong>b</strong> c d e.</p>"
        ),
        Some(r#"<p>A <del class="wd"><em>b</em></del><ins class="wd"><strong>b</strong></ins> c d e.</p>"#.into())
    );
    // Two list items merged: the new structure, with the word after the
    // removed break marked.
    assert_eq!(
        wd(
            "<ul>\n<li>a b c</li>\n<li>d e f</li>\n</ul>",
            "<ul>\n<li>a b c d e f</li>\n</ul>"
        ),
        Some(
            "<ul>\n<li>a b c\n<del class=\"wd\">d</del> <ins class=\"wd\">d</ins> e f</li>\n</ul>"
                .into()
        )
    );
}

#[test]
fn word_diff_renders_as_ins_and_del() {
    let d = empty();
    let html = docs_diff_html(
        &d,
        "Evaluates an expression.",
        "Evaluates a parsed expression.",
        &BTreeMap::new(),
    );
    assert_eq!(
        html,
        r#"<div class="mark m-chg"><div class="docs"><div class="dblk"><p>Evaluates <del class="wd">an</del><ins class="wd">a parsed</ins> expression.</p>
</div></div></div>"#
    );
}

#[test]
fn source_diff_follows_the_settings() {
    use model::{Item, SourceLine, Status};
    let d = empty();
    let mut it = Item {
        status: Status::Changed,
        ..Default::default()
    };
    it.changes.body = true;
    it.info.kind = model::Kind::Fn;
    it.info.line = 10;
    it.old_line = Some(4);
    it.source = vec![
        SourceLine(' ', "fn f() {".into()),
        SourceLine('-', "  a();".into()),
        SourceLine('+', "    a();".into()),
        SourceLine('-', "    b();".into()),
        SourceLine('+', "    c();".into()),
        SourceLine(' ', "}".into()),
    ];
    let html = d.source_html(&it, true, false);
    assert!(
        html.contains(">+2</span> <span class=\"d-del\">−2<"),
        "{html}"
    );
    crate::settings::set(crate::settings::Settings {
        ignore_ws: true,
        split: true,
        ..Default::default()
    });
    let html = d.source_html(&it, true, false);
    crate::settings::set(Default::default());
    // Only `b()` → `c()` counts, shown in one row: old line 6, new line 12.
    assert!(
        html.contains(">+1</span> <span class=\"d-del\">−1<"),
        "{html}"
    );
    assert!(
        html.contains(
            r#"<span class="no">6</span><span class="ln del wl" data-t="-">    <span class="wc">b</span>();"#
        ),
        "{html}"
    );
    assert!(
        html.contains(r#"<span class="no">12</span><span class="ln add" data-t="+">"#),
        "{html}"
    );
}

#[test]
fn private_fields_are_expandable_from_outside() {
    use model::{Item, Status};
    let mut out = empty().out;
    let mut it = Item {
        status: Status::Unchanged,
        ..Default::default()
    };
    it.info.id = "c::m::S".into();
    it.info.kind = model::Kind::Struct;
    it.info.module = "c::m".into();
    it.info.sig = "pub struct S {\n    secret: u8,\n}".into();
    it.info.sig_public = Some("pub struct S {\n    /* private fields */\n}".into());
    out.items.insert(it.info.id.clone(), it);
    let d = Data::new(out);
    let it = d.item("c::m::S").unwrap();
    let outside = d.sig_html(it, "c");
    assert!(outside.contains("private fields */"), "{outside}");
    assert!(
        outside.contains(r#"<details class="private-def">"#),
        "{outside}"
    );
    assert!(outside.contains("secret"), "{outside}");
    // Inside the module everything is shown already.
    let inside = d.sig_html(it, "c::m");
    assert!(!inside.contains("private-def"), "{inside}");
}

#[test]
fn neighboring_markers_are_joined() {
    let seg = |bar, html: &str| Seg::new(bar, html.to_string());
    assert_eq!(
        marked_html([
            seg("m-add", "a"),
            seg("m-add", "b"),
            seg("", ""),
            seg("m-add", "c"),
            seg("", "d"),
            seg("m-chg", "e"),
            seg("m-del", "f"),
        ]),
        r#"<div class="mark m-add">abc</div>d<div class="mark m-chg">e</div><div class="mark m-del">f</div>"#
    );
}

#[test]
fn an_added_item_has_no_part_markers() {
    use model::{Item, SourceLine, Status};
    let mut out = empty().out;
    let mut it = Item {
        status: Status::Added,
        ..Default::default()
    };
    it.info.id = "c::f".into();
    it.info.kind = model::Kind::Fn;
    it.info.module = "c".into();
    it.info.sig = "pub fn f()".into();
    it.info.docs = "Does things.\n\nIn detail.".into();
    it.source = vec![SourceLine('+', "pub fn f() {}".into())];
    out.items.insert(it.info.id.clone(), it);
    let d = Data::new(out);
    let it = d.item("c::f").unwrap();
    let html = d.item_body_html(it, "c", true, d.source_seg(it, false, true));
    // Marked as a whole (by the card or row), not part by part.
    assert_eq!(html.matches("class=\"mark").count(), 0, "{html}");
}

#[test]
fn small_body_changes_are_shown_directly() {
    use model::{Item, SourceLine, Status};
    let d = empty();
    let mut it = Item {
        status: Status::Changed,
        ..Default::default()
    };
    it.changes.body = true;
    it.info.kind = model::Kind::Fn;
    it.info.line = 1;
    it.old_line = Some(1);
    let mut lines: Vec<SourceLine> = (0..12).map(|i| SourceLine(' ', format!("l{i};"))).collect();
    lines.insert(6, SourceLine('-', "old();".into()));
    lines.insert(7, SourceLine('+', "new();".into()));
    it.source = lines.clone();
    let html = d.source_html(&it, false, true);
    // The hunk with two lines of context comes first, outside the
    // collapsed whole source; skipped lines are marked.
    let (hunk, whole) = html.split_once("<details").unwrap();
    // The changed line pair has its changed word marked.
    assert!(
        hunk.contains(r#"<span class="wc">old</span>();"#)
            && hunk.contains(r#"<span class="wc">new</span>();"#),
        "{hunk}"
    );
    assert!(hunk.contains("l4;") && hunk.contains("l7;"), "{hunk}");
    assert!(!hunk.contains("l2;") && !hunk.contains("l9;"), "{hunk}");
    assert_eq!(hunk.matches("⋯").count(), 2, "{hunk}");
    assert!(whole.contains("l0;"), "{whole}");
    // Five changed lines or more: collapsed as before.
    for k in 0..3 {
        lines.insert(1, SourceLine('+', format!("more{k}();")));
    }
    it.source = lines;
    let html = d.source_html(&it, false, true);
    assert!(
        html.starts_with("<div class=\"mark m-chg\"><details"),
        "{html}"
    );
}

#[test]
fn impl_headers_name_the_self_type() {
    use crate::data::impl_parts;
    let some = |a: &str, b: &str| (Some(a.to_string()), Some(b.to_string()));
    assert_eq!(
        impl_parts("impl<T> Log for Arc<T>\nwhere\n    T: ?Sized + Log,"),
        some("Log", "Arc<T>")
    );
    assert_eq!(impl_parts("impl<T> Log for &'_ T"), some("Log", "&'_ T"));
    assert_eq!(
        impl_parts("impl PartialEq<LevelFilter> for Level"),
        some("PartialEq<LevelFilter>", "Level")
    );
    assert_eq!(
        impl_parts("unsafe impl Sync for AtomicUsize"),
        some("Sync", "AtomicUsize")
    );
    assert_eq!(
        impl_parts("impl<'a, K: Source, V> fmt::Debug for (K, V)"),
        some("Debug", "(K, V)")
    );
    assert_eq!(
        impl_parts("impl<'v> Inner<'v>"),
        (None, Some("Inner<'v>".to_string()))
    );
}

#[test]
fn review_rendering_cases() {
    let d = empty();
    // Brackets that aren't links stay.
    let html = docs_html(&d, "Index arr[0] and a [u8] slice.", &BTreeMap::new());
    assert!(html.contains("arr[0]") && html.contains("[u8]"), "{html}");
    // Markdown files keep `# ` lines of their code blocks.
    let html = markdown_file_html(&d, "```\n# curl https://x | sh\nrun\n```\n");
    assert!(html.contains("# curl"), "{html}");
    assert!(
        crate::data::impl_parts("impl<F: Fn() -> u8> Tr for X<F>")
            .1
            .as_deref()
            == Some("X<F>")
    );
    assert!(!web_url("javascript://x/%0Aalert(1)") && web_url("https://docs.rs/x"));
}

#[test]
fn changed_code_lines_mark_changed_tokens() {
    let d = empty();
    let (a, b) = (
        code_html(&d, "let x = foo(1, 2);", Links::None),
        code_html(&d, "let x = foo(1, 3);", Links::None),
    );
    let (ma, mb) = mark_changed_tokens(&a, &b).unwrap();
    assert!(ma.contains(r#"<span class="wc">2</span>"#), "{ma}");
    assert!(mb.contains(r#"<span class="wc">3</span>"#), "{mb}");
    assert_eq!(ma.matches("wc").count(), 1, "{ma}");
    // Words of a run share one mark; tags are never inside a mark.
    let (_, mb) = mark_changed_tokens("<p>a b c d e</p>", "<p>a x y d e</p>").unwrap();
    assert_eq!(mb, r#"<p>a <span class="wc">x y</span> d e</p>"#);
    let (_, mb) = mark_changed_tokens("a <b>b</b> c d", "a x <b>y</b> c d").unwrap();
    assert_eq!(
        mb,
        r#"a <span class="wc">x</span> <b><span class="wc">y</span></b> c d"#
    );
    // A different line: no marks.
    assert_eq!(mark_changed_tokens("one two three", "four five six"), None);
}

#[test]
fn ampersand_before_multibyte_text_does_not_panic() {
    // The 12th byte after `&` falls inside `⋯`.
    let (old, new) = mark_changed_tokens("a &amp; x ⋯ y", "a &amp;  x⋯⋯ z").unwrap();
    assert!(
        old.contains("&amp;") && new.contains("&amp;"),
        "{old} {new}"
    );
    assert!(word_diff_html("&a b c ⋯ d", "&a b c ⋯ e").is_some());
}
