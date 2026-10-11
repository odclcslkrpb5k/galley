//! Property tests (docs/plan.md §9): any input loads without panicking, and the document always
//! satisfies the model's invariants, which the renderer relies on without checking.

#[cfg(test)]
mod properties {
    use gbk_markdown_core::model::{Container, InlineObject, Inlines, LeafKind, LinkTarget};
    use gbk_markdown_core::{Document, load};
    use proptest::prelude::*;

    /// Checks every invariant; returns the first broken one.
    fn check(doc: &Document) -> Result<(), String> {
        for (index, leaf) in doc.leaves.iter().enumerate() {
            let at = |what: &str| format!("leaf {index}: {what}");
            match &leaf.kind {
                LeafKind::Paragraph(inlines) | LeafKind::DetailsSummary { inlines, .. } => {
                    check_inlines(doc, inlines).map_err(|e| at(&e))?;
                }
                LeafKind::Heading {
                    inlines, anchor, ..
                } => {
                    check_inlines(doc, inlines).map_err(|e| at(&e))?;
                    let anchor = doc
                        .anchors
                        .get(anchor.index())
                        .ok_or_else(|| at("anchor"))?;
                    if anchor.leaf.index() != index {
                        return Err(at("anchor points elsewhere"));
                    }
                }
                LeafKind::Code { block, lines } => {
                    let code = doc
                        .code_blocks
                        .get(block.index())
                        .ok_or_else(|| at("code"))?;
                    if lines.start > lines.end || lines.end.index() > code.line_starts.len() {
                        return Err(at("code lines out of range"));
                    }
                    for start in &code.line_starts {
                        if !code.text.is_char_boundary(start.index()) {
                            return Err(at("line start off a char boundary"));
                        }
                    }
                }
                LeafKind::TableRows { table, rows, .. } => {
                    let table = doc.tables.get(table.index()).ok_or_else(|| at("table"))?;
                    if rows.start > rows.end || rows.end.index() > table.rows.len() {
                        return Err(at("table rows out of range"));
                    }
                    for cell in table.header.iter().chain(table.rows.iter().flatten()) {
                        check_inlines(doc, cell).map_err(|e| at(&e))?;
                    }
                }
                LeafKind::ThematicBreak | LeafKind::AlertTitle(_) => {}
            }
            for container in &leaf.containers {
                if let Container::Footnote { footnote, .. } = container
                    && doc.footnotes.get(footnote.index()).is_none()
                {
                    return Err(at("footnote container id"));
                }
            }
        }
        for (index, anchor) in doc.anchors.iter().enumerate() {
            match doc.leaves.get(anchor.leaf.index()).map(|leaf| &leaf.kind) {
                Some(LeafKind::Heading { anchor: id, .. }) if id.index() == index => {}
                _ => return Err(format!("anchor {index} is not its heading's")),
            }
        }
        for (index, footnote) in doc.footnotes.iter().enumerate() {
            if footnote.leaves.start > footnote.leaves.end
                || footnote.leaves.end.index() > doc.leaves.len()
            {
                return Err(format!("footnote {index} leaves out of range"));
            }
        }
        for link in &doc.links {
            if let LinkTarget::Footnote(id) = link.target
                && doc.footnotes.get(id.index()).is_none()
            {
                return Err("footnote link id".to_owned());
            }
        }
        Ok(())
    }

    fn check_inlines(doc: &Document, inlines: &Inlines) -> Result<(), String> {
        let text = &inlines.text;
        let mut previous_end = 0;
        for span in &inlines.spans {
            let (start, end) = (span.range.start.index(), span.range.end.index());
            if start >= end {
                return Err(format!("empty or reversed span {start}..{end}"));
            }
            if start < previous_end {
                return Err("spans overlap or are unsorted".to_owned());
            }
            if end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                return Err(format!(
                    "span {start}..{end} not on char boundaries of {text:?}"
                ));
            }
            if let Some(link) = span.link
                && doc.links.get(link.index()).is_none()
            {
                return Err("span link id".to_owned());
            }
            previous_end = end;
        }
        for (offset, object) in &inlines.objects {
            if !text
                .get(offset.index()..)
                .is_some_and(|rest| rest.starts_with(Inlines::OBJECT))
            {
                return Err("object offset not at U+FFFC".to_owned());
            }
            match object {
                InlineObject::Image(id) => {
                    if doc.images.get(id.index()).is_none() {
                        return Err("image id".to_owned());
                    }
                }
            }
        }
        let markers = text.matches(Inlines::OBJECT).count();
        if markers < inlines.objects.len() {
            return Err("fewer U+FFFC than objects".to_owned());
        }
        Ok(())
    }

    /// Text built from Markdown's special characters, words, and some HTML.
    fn markdown_like() -> impl Strategy<Value = String> {
        let piece = prop_oneof![
            Just("# ".to_owned()),
            Just("## ".to_owned()),
            Just("\n".to_owned()),
            Just("\n\n".to_owned()),
            Just("- ".to_owned()),
            Just("1. ".to_owned()),
            Just("- [x] ".to_owned()),
            Just("> ".to_owned()),
            Just("> [!NOTE]\n".to_owned()),
            Just("```\n".to_owned()),
            Just("    ".to_owned()),
            Just("| a | b |\n|---|:-:|\n".to_owned()),
            Just("*".to_owned()),
            Just("**".to_owned()),
            Just("_".to_owned()),
            Just("~~".to_owned()),
            Just("`".to_owned()),
            Just("$".to_owned()),
            Just("[x](y)".to_owned()),
            Just("![a](b.png)".to_owned()),
            Just("[^1]".to_owned()),
            Just("[^1]: note\n".to_owned()),
            Just("<details>".to_owned()),
            Just("<summary>s</summary>".to_owned()),
            Just("</details>".to_owned()),
            Just("<div align=\"center\">".to_owned()),
            Just("</div>".to_owned()),
            Just("<pre>".to_owned()),
            Just("</pre>".to_owned()),
            Just("<blockquote>".to_owned()),
            Just("<b>".to_owned()),
            Just("</b>".to_owned()),
            Just("<img src=\"x\">".to_owned()),
            Just("<!-- c -->".to_owned()),
            Just("---\n".to_owned()),
            Just("https://example.com ".to_owned()),
            "[a-zé漢😀 ]{1,8}",
        ];
        prop::collection::vec(piece, 0..60).prop_map(|pieces| pieces.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn arbitrary_bytes_load(bytes in prop::collection::vec(any::<u8>(), 0..2000)) {
            let doc = load(&bytes);
            prop_assert_eq!(check(&doc), Ok(()));
        }

        #[test]
        fn markdown_like_text_loads(text in markdown_like()) {
            let doc = load(text.as_bytes());
            prop_assert_eq!(check(&doc), Ok(()));
        }
    }

    #[test]
    fn the_corpus_loads() {
        let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/corpus");
        let mut loaded = 0;
        for entry in std::fs::read_dir(corpus).expect("corpus") {
            let input = entry.expect("entry").path().join("input.md");
            let bytes = std::fs::read(&input).expect("input");
            let doc = load(&bytes);
            assert_eq!(check(&doc), Ok(()), "{}", input.display());
            assert!(!doc.leaves.is_empty(), "{} has no leaves", input.display());
            loaded += 1;
        }
        assert!(loaded >= 23, "found only {loaded} corpus inputs");
    }
}
