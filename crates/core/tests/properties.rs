//! Property tests (docs/plan.md §9): any input loads without panicking, and the document always
//! satisfies the model's invariants, which the renderer relies on without checking.

#[cfg(test)]
mod properties {
    use gbk_markdown_core::model::{
        Container, InlineObject, Inlines, LEAF_TEXT_BYTES, LeafKind, LinkTarget,
    };
    use gbk_markdown_core::{Document, load};
    use proptest::prelude::*;

    /// Checks every invariant; returns the first broken one.
    fn check(doc: &Document) -> Result<(), String> {
        check_contexts(doc)?;
        for (id, leaf) in doc.leaves_with_ids() {
            let at = |what: &str| format!("leaf {}: {what}", id.index());
            if let Some(context) = leaf.context() {
                let context = doc.context(context).ok_or_else(|| at("context id"))?;
                if context
                    .parent()
                    .is_some_and(|parent| doc.context(parent).is_none())
                {
                    return Err(at("parent context id"));
                }
            }
            for (context_id, context) in doc.containers(leaf) {
                // A leaf never comes before its containers' first leaf.
                let started = doc
                    .leaves_with_ids()
                    .take(id.index() + 1)
                    .any(|(leaf, _)| context.starts_at(leaf));
                if !started {
                    return Err(at(&format!(
                        "container {} has no first leaf at or before it",
                        context_id.index()
                    )));
                }
            }
            match leaf.kind() {
                LeafKind::Paragraph(inlines) | LeafKind::DetailsSummary { inlines, .. } => {
                    check_inlines(doc, inlines).map_err(|e| at(&e))?;
                    if matches!(leaf.kind(), LeafKind::Paragraph(_))
                        && inlines.text().len() > LEAF_TEXT_BYTES
                    {
                        return Err(at("paragraph leaf over the size cap"));
                    }
                }
                LeafKind::Heading {
                    inlines, anchor, ..
                } => {
                    check_inlines(doc, inlines).map_err(|e| at(&e))?;
                    let anchor = doc.anchor(*anchor).ok_or_else(|| at("anchor"))?;
                    if anchor.leaf() != id {
                        return Err(at("anchor points elsewhere"));
                    }
                }
                LeafKind::Code { block, lines } => {
                    let code = doc.code_block(*block).ok_or_else(|| at("code"))?;
                    if lines.start > lines.end || lines.end.index() > code.line_count() {
                        return Err(at("code lines out of range"));
                    }
                    if lines.start < lines.end && code.lines(lines).is_none() {
                        return Err(at("code lines do not slice the text"));
                    }
                }
                LeafKind::TableRows { table, rows, .. } => {
                    let table = doc.table(*table).ok_or_else(|| at("table"))?;
                    let rows = table.row_range(rows).ok_or_else(|| at("table rows"))?;
                    for cell in table.header().iter().chain(rows.iter().flatten()) {
                        check_inlines(doc, cell).map_err(|e| at(&e))?;
                    }
                }
                LeafKind::ThematicBreak | LeafKind::AlertTitle(_) => {}
            }
        }
        // Every anchor names a leaf, and every heading's anchor names that heading.
        for (index, anchor) in doc.anchors().iter().enumerate() {
            if doc.leaf(anchor.leaf()).is_none() {
                return Err(format!("anchor {index} has no leaf"));
            }
        }
        for (id, leaf) in doc.leaves_with_ids() {
            if let LeafKind::Heading { anchor, .. } = leaf.kind()
                && doc
                    .anchor(*anchor)
                    .map(gbk_markdown_core::model::Anchor::leaf)
                    != Some(id)
            {
                return Err(format!("heading {} has another's anchor", id.index()));
            }
        }
        for link in doc.links() {
            if let LinkTarget::Footnote(id) = link.target()
                && doc.footnote(*id).is_none()
            {
                return Err("footnote link id".to_owned());
            }
        }
        Ok(())
    }

    /// Contexts are created parent first, so a parent's id is always smaller: no cycles. Every
    /// list item and footnote has a first leaf, to carry its marker.
    fn check_contexts(doc: &Document) -> Result<(), String> {
        for (id, context) in doc.contexts_with_ids() {
            let index = id.index();
            if context
                .parent()
                .is_some_and(|parent| parent.index() >= index)
            {
                return Err(format!("context {index}'s parent is not older"));
            }
            if matches!(
                context.container(),
                Container::ListItem { .. } | Container::Footnote(_)
            ) && !doc
                .leaves_with_ids()
                .any(|(leaf, _)| context.starts_at(leaf))
            {
                return Err(format!("context {index} (item or footnote) has no leaf"));
            }
        }
        Ok(())
    }

    fn check_inlines(doc: &Document, inlines: &Inlines) -> Result<(), String> {
        let text = inlines.text();
        let mut previous_end = 0;
        for span in inlines.spans() {
            let (start, end) = (span.range().start.index(), span.range().end.index());
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
            if let Some(link) = span.link()
                && doc.link(link).is_none()
            {
                return Err("span link id".to_owned());
            }
            previous_end = end;
        }
        for (offset, object) in inlines.objects() {
            if !text
                .get(offset.index()..)
                .is_some_and(|rest| rest.starts_with(Inlines::OBJECT))
            {
                return Err("object offset not at U+FFFC".to_owned());
            }
            let InlineObject::Image(id) = object;
            if doc.image(*id).is_none() {
                return Err("image id".to_owned());
            }
        }
        if text.matches(Inlines::OBJECT).count() < inlines.objects().len() {
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
            Just("\r\n".to_owned()),
            Just("- ".to_owned()),
            Just("  - ".to_owned()),
            Just("1. ".to_owned()),
            Just("- [x] ".to_owned()),
            Just("> ".to_owned()),
            Just("> [!NOTE]\n".to_owned()),
            Just(">[!tip]\n".to_owned()),
            Just("> [!NOTE] Title\n".to_owned()),
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
            Just("<details open>".to_owned()),
            Just("<summary>s</summary>".to_owned()),
            Just("<summary>".to_owned()),
            Just("</summary>".to_owned()),
            Just("</details>".to_owned()),
            Just("<div align=\"center\">".to_owned()),
            Just("</div>".to_owned()),
            Just("<pre>".to_owned()),
            Just("</pre>".to_owned()),
            Just("<blockquote>".to_owned()),
            Just("</blockquote>".to_owned()),
            Just("<a href=\"x\">".to_owned()),
            Just("</a>".to_owned()),
            Just("<b>".to_owned()),
            Just("</b>".to_owned()),
            Just("<script>".to_owned()),
            Just("<video src=\"v.mp4\">".to_owned()),
            Just("<img src=\"x\">".to_owned()),
            Just("<br>".to_owned()),
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
            assert!(
                !doc.leaves().is_empty(),
                "{} has no leaves",
                input.display()
            );
            loaded += 1;
        }
        assert!(loaded >= 23, "found only {loaded} corpus inputs");
    }
}
