//! One check per comrak option `gfm_options()` sets (docs/plan.md §5): each shows the option
//! on or off as GitHub has it, so a changed comrak default or a dropped setting fails here.
//! Plus heading anchors against the ids GitHub gave the synthetic corpus file.

#[cfg(test)]
mod gfm {
    use std::path::Path;

    use gbk_markdown_core::model::{
        AlertKind, Container, Inlines, LeafKind, LinkTarget, Marker, Style,
    };
    use gbk_markdown_core::{Document, load};

    fn doc(markdown: &str) -> Document {
        load(markdown.as_bytes())
    }

    /// The inlines of the only paragraph.
    fn paragraph(markdown: &str) -> Inlines {
        let doc = doc(markdown);
        assert_eq!(doc.leaves.len(), 1, "one leaf for {markdown:?}");
        match doc.leaves.into_iter().next().map(|leaf| leaf.kind) {
            Some(LeafKind::Paragraph(inlines)) => inlines,
            other => panic!("expected a paragraph for {markdown:?}, got {other:?}"),
        }
    }

    fn style_of(inlines: &Inlines, text: &str) -> Style {
        let at = inlines.text.find(text).expect("text present");
        inlines
            .spans
            .iter()
            .find(|span| span.range.start.index() <= at && at < span.range.end.index())
            .map_or(Style::PLAIN, |span| span.style)
    }

    // On.

    #[test]
    fn strikethrough_is_on() {
        let inlines = paragraph("a ~~gone~~ b");
        assert_eq!(inlines.text, "a gone b");
        assert_eq!(style_of(&inlines, "gone"), Style::STRIKETHROUGH);
    }

    #[test]
    fn tables_are_on() {
        let doc = doc("| a | b |\n|:--|--:|\n| 1 | 2 |\n");
        assert_eq!(doc.tables.len(), 1);
        let table = doc.tables.first().expect("table");
        assert_eq!(table.header.len(), 2);
        assert_eq!(table.rows.len(), 1);
    }

    #[test]
    fn autolinks_are_on() {
        let doc = doc("see www.example.com now");
        let link = doc.links.first().expect("an autolink");
        assert_eq!(
            link.target,
            LinkTarget::Absolute("http://www.example.com".to_owned())
        );
    }

    #[test]
    fn task_lists_are_on() {
        let doc = doc("- [x] done\n- [ ] open\n");
        let markers: Vec<Marker> = doc
            .leaves
            .iter()
            .filter_map(|leaf| match leaf.containers.last() {
                Some(Container::ListItem(item)) => Some(item.marker),
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
        let footnote = doc.footnotes.first().expect("a footnote");
        assert_eq!(footnote.number, 1);
        let reference = match doc.leaves.first().map(|leaf| &leaf.kind) {
            Some(LeafKind::Paragraph(inlines)) => inlines.clone(),
            other => panic!("expected the referencing paragraph, got {other:?}"),
        };
        assert_eq!(reference.text, "text1");
        assert_eq!(style_of(&reference, "1"), Style::FOOTNOTE_REFERENCE);
        let last = doc.leaves.last().expect("definition leaf");
        assert!(matches!(
            last.containers.first(),
            Some(Container::Footnote {
                show_number: true,
                ..
            })
        ));
    }

    #[test]
    fn alerts_are_on_and_custom_titles_are_quotes() {
        let doc = doc("> [!WARNING]\n> careful\n\n> [!NOTE] Mine\n> body\n");
        let kinds: Vec<&LeafKind> = doc.leaves.iter().map(|leaf| &leaf.kind).collect();
        assert!(matches!(
            kinds.first(),
            Some(LeafKind::AlertTitle(AlertKind::Warning))
        ));
        // GitHub renders a titled alert as a plain quote whose text starts with the marker.
        let titled = doc.leaves.last().expect("titled leaf");
        assert_eq!(titled.containers, vec![Container::Quote]);
        let LeafKind::Paragraph(inlines) = &titled.kind else {
            panic!("expected a paragraph, got {:?}", titled.kind);
        };
        assert_eq!(inlines.text, "[!NOTE] Mine body");
    }

    #[test]
    fn dollar_and_code_math_are_on() {
        let inlines = paragraph("a $x_1 + y_2$ b $`z^2`$");
        assert_eq!(inlines.text, "a x_1 + y_2 b z^2");
        assert_eq!(style_of(&inlines, "x_1"), Style::MATH);
        assert_eq!(style_of(&inlines, "z^2"), Style::MATH);
    }

    #[test]
    fn front_matter_is_on() {
        let doc = doc("---\ntitle: x\n---\n\n# Body\n");
        let block = doc.code_blocks.first().expect("front matter block");
        assert_eq!(block.info, "yaml");
        assert_eq!(block.text, "title: x");
    }

    // Off.

    #[test]
    fn superscript_subscript_and_underline_are_off() {
        assert_eq!(paragraph("^up^ x").text, "^up^ x");
        // With strikethrough on, GFM reads ~a~ as strikethrough, not subscript.
        assert_eq!(style_of(&paragraph("~a~ x"), "a"), Style::STRIKETHROUGH);
        assert_eq!(style_of(&paragraph("__u__ x"), "u"), Style::STRONG);
    }

    #[test]
    fn wikilinks_description_lists_and_spoilers_are_off() {
        assert_eq!(paragraph("[[Page]] x").text, "[[Page]] x");
        assert_eq!(paragraph("term\n: definition").text, "term : definition");
        assert_eq!(paragraph("||secret|| x").text, "||secret|| x");
    }

    #[test]
    fn highlight_and_insert_are_off() {
        assert_eq!(paragraph("==mark== x").text, "==mark== x");
        assert_eq!(paragraph("++ins++ x").text, "++ins++ x");
    }

    #[test]
    fn smart_punctuation_and_hard_breaks_are_off() {
        assert_eq!(paragraph("\"q\" -- ...").text, "\"q\" -- ...");
        assert_eq!(paragraph("a\nb").text, "a b");
    }

    #[test]
    fn relaxed_task_matching_is_off() {
        let doc = doc("- [y] not a task\n");
        let item = doc.leaves.first().expect("leaf");
        assert!(matches!(
            item.containers.last(),
            Some(Container::ListItem(item)) if item.marker == Marker::Bullet
        ));
    }

    #[test]
    fn relaxed_autolinks_are_off() {
        assert_eq!(doc("see example.com/path").links, Vec::new());
    }

    #[test]
    fn task_items_in_tables_are_off() {
        let doc = doc("| a |\n|---|\n| [x] |\n");
        let cell = doc
            .tables
            .first()
            .and_then(|table| table.rows.first())
            .and_then(|row| row.first())
            .expect("cell");
        assert_eq!(cell.text, "[x]");
    }

    // Heading anchors, against GitHub's own rendering of the synthetic corpus file.

    #[test]
    fn heading_anchors_match_github() {
        let corpus =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/corpus/_synthetic-features");
        let markdown = std::fs::read(corpus.join("input.md")).expect("input");
        let github = std::fs::read_to_string(corpus.join("github.html")).expect("rendering");
        // GitHub writes each heading's anchor as id="user-content-<slug>" (footnote anchors
        // use the same prefix and are left out).
        let expected: Vec<String> = github
            .split("id=\"user-content-")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .filter(|id| !id.starts_with("fn"))
            .map(str::to_owned)
            .collect();
        let doc = load(&markdown);
        let actual: Vec<String> = doc
            .anchors
            .iter()
            .map(|anchor| anchor.slug.clone())
            .collect();
        assert!(
            expected.len() >= 20,
            "found {} GitHub heading ids",
            expected.len()
        );
        assert_eq!(actual, expected);
    }
}
