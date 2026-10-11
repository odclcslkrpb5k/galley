//! One check per comrak option `gfm_options()` sets (docs/plan.md §5): each shows the option
//! on or off as GitHub has it, so a changed comrak default or a dropped setting fails here.
//! Then GitHub behaviours found in reviews, and heading anchors against GitHub's own rendering
//! of every corpus file.

#[cfg(test)]
mod gfm {
    use std::path::Path;

    use gbk_markdown_core::model::{
        AlertKind, Container, Inlines, Leaf, LeafKind, LinkTarget, Marker, Style,
    };
    use gbk_markdown_core::{Document, load};

    fn doc(markdown: &str) -> Document {
        load(markdown.as_bytes())
    }

    fn texts(doc: &Document) -> Vec<String> {
        doc.leaves()
            .iter()
            .map(|leaf| match leaf.kind() {
                LeafKind::Paragraph(inlines)
                | LeafKind::Heading { inlines, .. }
                | LeafKind::DetailsSummary { inlines, .. } => inlines.text().to_owned(),
                LeafKind::Code { .. } => "<code>".to_owned(),
                LeafKind::TableRows { .. } => "<table>".to_owned(),
                LeafKind::ThematicBreak => "<hr>".to_owned(),
                LeafKind::AlertTitle(kind) => format!("<{}>", kind.title()),
            })
            .collect()
    }

    /// The containers of a leaf, innermost first.
    fn containers(doc: &Document, leaf: &Leaf) -> Vec<Container> {
        doc.containers(leaf)
            .map(|(_id, context)| *context.container())
            .collect()
    }

    /// The inlines of the only paragraph.
    fn paragraph(markdown: &str) -> Inlines {
        let doc = doc(markdown);
        assert_eq!(doc.leaves().len(), 1, "one leaf for {markdown:?}");
        let Some(LeafKind::Paragraph(inlines)) = doc.leaves().first().map(Leaf::kind) else {
            panic!("expected a paragraph for {markdown:?}");
        };
        inlines.clone()
    }

    fn style_of(inlines: &Inlines, text: &str) -> Style {
        let at = inlines.text().find(text).expect("text present");
        inlines
            .spans()
            .iter()
            .find(|span| span.range().start.index() <= at && at < span.range().end.index())
            .map_or(Style::PLAIN, gbk_markdown_core::model::Span::style)
    }

    // On.

    #[test]
    fn strikethrough_is_on() {
        let inlines = paragraph("a ~~gone~~ b");
        assert_eq!(inlines.text(), "a gone b");
        assert_eq!(style_of(&inlines, "gone"), Style::STRIKETHROUGH);
    }

    #[test]
    fn tables_are_on() {
        let doc = doc("| a | b |\n|:--|--:|\n| 1 | 2 |\n");
        let table = doc.tables().first().expect("table");
        assert_eq!(table.header().len(), 2);
        assert_eq!(table.rows().len(), 1);
    }

    #[test]
    fn autolinks_are_on() {
        let doc = doc("see www.example.com now");
        let link = doc.links().first().expect("an autolink");
        assert_eq!(
            link.target(),
            &LinkTarget::Absolute("http://www.example.com".to_owned())
        );
    }

    #[test]
    fn task_lists_are_on() {
        let doc = doc("- [x] done\n- [ ] open\n");
        let markers: Vec<Marker> = doc
            .leaves()
            .iter()
            .filter_map(|leaf| match containers(&doc, leaf).first() {
                Some(Container::ListItem { marker, .. }) => Some(*marker),
                _ => None,
            })
            .collect();
        assert_eq!(
            markers,
            vec![
                Marker::Task { checked: true },
                Marker::Task { checked: false }
            ]
        );
    }

    #[test]
    fn footnotes_are_on() {
        let doc = doc("text[^n]\n\n[^n]: the note\n");
        let footnote = doc.footnotes().first().expect("a footnote");
        assert_eq!(footnote.number(), 1);
        let Some(LeafKind::Paragraph(reference)) = doc.leaves().first().map(Leaf::kind) else {
            panic!("expected the referencing paragraph");
        };
        assert_eq!(reference.text(), "text1");
        assert_eq!(style_of(reference, "1"), Style::FOOTNOTE_REFERENCE);
        let last = doc.leaves().last().expect("definition leaf");
        assert!(matches!(
            containers(&doc, last).first(),
            Some(Container::Footnote(_))
        ));
    }

    #[test]
    fn alerts_are_on() {
        let doc = doc("> [!WARNING]\n> careful\n");
        assert_eq!(texts(&doc), vec!["<Warning>", "careful"]);
    }

    #[test]
    fn dollar_and_code_math_are_on() {
        let inlines = paragraph("a $x_1 + y_2$ b $`z^2`$");
        assert_eq!(inlines.text(), "a x_1 + y_2 b z^2");
        assert_eq!(style_of(&inlines, "x_1"), Style::MATH);
        assert_eq!(style_of(&inlines, "z^2"), Style::MATH);
        let display = paragraph("$$\\frac{1}{3}$$");
        assert_eq!(style_of(&display, "frac"), Style::DISPLAY_MATH);
    }

    #[test]
    fn front_matter_is_on() {
        let doc = doc("---\ntitle: x\n---\n\n# Body\n");
        let block = doc.code_blocks().first().expect("front matter block");
        assert_eq!(block.info(), "yaml");
        assert_eq!(block.text(), "title: x");
    }

    // Off.

    #[test]
    fn superscript_subscript_and_underline_are_off() {
        assert_eq!(paragraph("^up^ x").text(), "^up^ x");
        // With strikethrough on, GFM reads ~a~ as strikethrough, not subscript.
        assert_eq!(style_of(&paragraph("~a~ x"), "a"), Style::STRIKETHROUGH);
        assert_eq!(style_of(&paragraph("__u__ x"), "u"), Style::STRONG);
    }

    #[test]
    fn wikilinks_description_lists_and_spoilers_are_off() {
        assert_eq!(paragraph("[[Page]] x").text(), "[[Page]] x");
        assert_eq!(paragraph("term\n: definition").text(), "term : definition");
        assert_eq!(paragraph("||secret|| x").text(), "||secret|| x");
    }

    #[test]
    fn highlight_and_insert_are_off() {
        assert_eq!(paragraph("==mark== x").text(), "==mark== x");
        assert_eq!(paragraph("++ins++ x").text(), "++ins++ x");
    }

    #[test]
    fn smart_punctuation_and_hard_breaks_are_off() {
        assert_eq!(paragraph("\"q\" -- ...").text(), "\"q\" -- ...");
        assert_eq!(paragraph("a\nb").text(), "a b");
    }

    #[test]
    fn relaxed_task_matching_is_off() {
        let doc = doc("- [y] not a task\n");
        let item = doc.leaves().first().expect("leaf");
        assert!(matches!(
            containers(&doc, item).first(),
            Some(Container::ListItem {
                marker: Marker::Bullet,
                ..
            })
        ));
    }

    #[test]
    fn relaxed_autolinks_are_off() {
        assert_eq!(doc("see example.com/path").links(), &[]);
    }

    #[test]
    fn task_items_in_tables_are_off() {
        let doc = doc("| a |\n|---|\n| [x] |\n");
        let cell = doc
            .tables()
            .first()
            .and_then(|table| table.rows().first())
            .and_then(|row| row.first())
            .expect("cell");
        assert_eq!(cell.text(), "[x]");
    }

    // GitHub behaviours found in the M1 reviews.

    #[test]
    fn unspaced_alert_markers_are_alerts() {
        assert_eq!(texts(&doc(">[!tip]\n>Use it.\n")), vec!["<Tip>", "Use it."]);
    }

    #[test]
    fn titled_alerts_are_quotes_starting_with_the_marker_line_as_written() {
        let doc = doc("> [!Tip] My **title**\n> body\n");
        assert_eq!(texts(&doc), vec!["[!Tip] My title body"]);
        let leaf = doc.leaves().first().expect("leaf");
        assert_eq!(containers(&doc, leaf), vec![Container::Quote]);
        let LeafKind::Paragraph(inlines) = leaf.kind() else {
            panic!("paragraph");
        };
        assert_eq!(style_of(inlines, "title"), Style::STRONG);
        // A title before a non-paragraph stands on its own line, first.
        assert_eq!(
            texts(&self::doc("> [!NOTE] Title\n> - item\n")),
            vec!["[!NOTE] Title", "item"]
        );
    }

    #[test]
    fn a_footnote_label_defined_twice_keeps_its_first_definition() {
        let doc = doc("x[^a]\n\n[^a]: one\n\n[^a]: two\n");
        assert_eq!(texts(&doc), vec!["x1", "one"]);
        assert_eq!(doc.footnotes().len(), 1);
    }

    #[test]
    fn markdown_and_html_styles_interleave_like_a_browser() {
        let inlines = paragraph("*a <b>b* c</b>");
        assert_eq!(style_of(&inlines, "c"), Style::STRONG);
        let inlines = paragraph("<b>x *y</b> z*");
        assert_eq!(style_of(&inlines, "z"), Style::EMPHASIS);
        let inlines = paragraph("**<i>a** b</i>");
        assert_eq!(style_of(&inlines, "b"), Style::EMPHASIS);
    }

    #[test]
    fn closing_tags_inside_paragraphs_close_block_html() {
        let doc = doc("<details>\n<summary>More</summary>\n\nhidden text</details>\n\n# after\n");
        let after = doc.leaves().last().expect("heading");
        assert_eq!(containers(&doc, after), Vec::new());
    }

    #[test]
    fn list_items_close_html_opened_inside_them() {
        let doc = doc("- a\n\n  <details>\n\n  hidden\n- b\n\nafter\n");
        let after = doc.leaves().last().expect("after");
        assert_eq!(containers(&doc, after), Vec::new());
    }

    #[test]
    fn an_item_with_only_a_comment_keeps_its_marker() {
        assert_eq!(texts(&doc("- <!-- c -->\n- b\n")), vec!["", "b"]);
    }

    #[test]
    fn crlf_line_endings_do_not_reach_the_model() {
        let doc = doc("```\r\na\r\nb\r\n```\r\n");
        assert_eq!(
            doc.code_blocks()
                .first()
                .map(gbk_markdown_core::model::CodeBlock::text),
            Some("a\nb")
        );
    }

    #[test]
    fn filtered_html_tags_show_as_text_as_on_github() {
        assert_eq!(
            paragraph("text <style>p{}</style> b").text(),
            "text <style>p{}</style> b"
        );
    }

    #[test]
    fn alerts_are_titled_with_their_kind() {
        let doc = doc("> [!CAUTION]\n> x\n");
        assert!(matches!(
            doc.leaves().first().map(Leaf::kind),
            Some(LeafKind::AlertTitle(AlertKind::Caution))
        ));
    }

    // Heading anchors, against GitHub's own rendering of every corpus file.

    /// GitHub's heading ids in a rendering: `id="user-content-<slug>" class="anchor"` on the
    /// permalink after each heading. Raw `<a id>` anchors written in the Markdown carry the same
    /// prefix but no `class="anchor"`.
    fn github_heading_ids(html: &str) -> Vec<String> {
        html.split("<a id=\"user-content-")
            .skip(1)
            .filter_map(|rest| rest.split_once("\" class=\"anchor\""))
            .map(|(id, _rest)| id.to_owned())
            .filter(|id| !id.contains('"'))
            .collect()
    }

    #[test]
    fn heading_anchors_match_github_across_the_corpus() {
        let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/corpus");
        let mut checked = 0;
        let mut differences = Vec::new();
        for entry in std::fs::read_dir(corpus).expect("corpus") {
            let dir = entry.expect("entry").path();
            let markdown = std::fs::read(dir.join("input.md")).expect("input");
            let github = std::fs::read_to_string(dir.join("github.html")).expect("rendering");
            let expected = github_heading_ids(&github);
            let doc = load(&markdown);
            let actual: Vec<String> = doc
                .anchors()
                .iter()
                .map(|anchor| anchor.slug().to_owned())
                .collect();
            if expected != actual {
                let at = expected
                    .iter()
                    .zip(&actual)
                    .position(|(e, a)| e != a)
                    .unwrap_or_else(|| expected.len().min(actual.len()));
                differences.push(format!(
                    "{}: {} GitHub ids, {} ours; first difference at {at}: {:?} vs {:?}",
                    dir.display(),
                    expected.len(),
                    actual.len(),
                    expected.get(at),
                    actual.get(at)
                ));
            }
            checked += expected.len();
        }
        assert!(checked >= 300, "only {checked} GitHub heading ids found");
        assert!(differences.is_empty(), "{}", differences.join("\n"));
    }
}
