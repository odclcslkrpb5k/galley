//! Spike content: the document model's leaves as plain text, one paragraph each.
//!
//! Stands in for the renderer until M2 (docs/plan.md §10).

use gbk_markdown_core::Document;
use gbk_markdown_core::model::{LeafKind, Notice};

pub(crate) fn leaf_texts(doc: &Document) -> Vec<String> {
    let notices = doc.notices().iter().map(|notice| {
        match notice {
            Notice::InvalidUtf8Replaced => {
                "⚠ This file is not valid UTF-8; unreadable bytes show as �."
            }
            Notice::HtmlTooDeep => "⚠ Some HTML was nested too deeply to show.",
        }
        .to_owned()
    });
    let leaves = doc.leaves().iter().map(|leaf| match leaf.kind() {
        LeafKind::Paragraph(inlines)
        | LeafKind::Heading { inlines, .. }
        | LeafKind::DetailsSummary { inlines, .. } => inlines.text().to_owned(),
        LeafKind::Code { block, lines } => doc
            .code_block(*block)
            .and_then(|code| code.lines(lines))
            .unwrap_or_default()
            .to_owned(),
        LeafKind::TableRows {
            table,
            rows,
            with_header,
        } => {
            let Some(table) = doc.table(*table) else {
                return String::new();
            };
            let header = with_header.then(|| table.header());
            header
                .into_iter()
                .chain(
                    table
                        .row_range(rows)
                        .unwrap_or_default()
                        .iter()
                        .map(Vec::as_slice),
                )
                .map(|cells| {
                    cells
                        .iter()
                        .map(gbk_markdown_core::model::Inlines::text)
                        .collect::<Vec<_>>()
                        .join(" │ ")
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        LeafKind::ThematicBreak => "────────".to_owned(),
        LeafKind::AlertTitle(kind) => kind.title().to_owned(),
    });
    notices.chain(leaves).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_become_plain_text() {
        let doc = gbk_markdown_core::load(b"# A\n\none\ntwo\n\n| x | y |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(leaf_texts(&doc), vec!["A", "one two", "x │ y\n1 │ 2"]);
    }

    #[test]
    fn notices_come_first() {
        let doc = gbk_markdown_core::load(b"a\xFFb");
        let texts = leaf_texts(&doc);
        assert!(texts.first().is_some_and(|text| text.starts_with('⚠')));
        assert_eq!(texts.get(1).map(String::as_str), Some("a\u{FFFD}b"));
    }
}
