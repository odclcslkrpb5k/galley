//! What can be copied, and the text each choice copies (docs/plan.md §4.5 "Copying"): a code
//! block (its button), and what the right-click menu offers at a point. Pure: the view finds
//! what is under the pointer and puts the chosen text on the clipboard.
//!
//! Text is copied as shown, not as written: no Markdown punctuation, images left out.

use std::collections::HashMap;
use std::ops::Range;

use galley_core::Document;
use galley_core::model::Inlines;
use galley_core::units::{CodeBlockId, TableId};

use crate::render::paint::{AttrKind, FontRole, LeafPaint, TextAttr};
use crate::render::table::RowPaint;
use crate::render::text::TextPoint;

/// A menu item: its label and the text it copies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Choice {
    pub(crate) label: &'static str,
    pub(crate) text: String,
}

pub(crate) const COPY_CODE: &str = "Copy code";
pub(crate) const COPY_LINK: &str = "Copy link address";
pub(crate) const COPY_IMAGE: &str = "Copy image address";
pub(crate) const COPY_PARAGRAPH: &str = "Copy paragraph";
pub(crate) const COPY_HEADING: &str = "Copy heading";
pub(crate) const COPY_ROW: &str = "Copy row";
pub(crate) const COPY_TABLE: &str = "Copy table";

/// What is under the pointer in a paint, as the view found it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Under {
    pub(crate) point: TextPoint,
    /// The address of the link there, ready to paste (a URL, or a path made absolute).
    pub(crate) link: Option<String>,
    /// The address of the image there.
    pub(crate) image: Option<String>,
}

/// The texts of the document's code blocks, by `CodeBlockId`: a code block's paints are pieces
/// of it, and a piece may be cut inside a very long line, so its text is kept whole.
pub(crate) fn code_texts(doc: &Document) -> Vec<String> {
    doc.code_blocks()
        .iter()
        .map(|block| block.text().to_owned())
        .collect()
}

/// The paints of each code block (a block's pieces are consecutive).
pub(crate) fn code_block_ranges(paints: &[LeafPaint]) -> HashMap<CodeBlockId, Range<usize>> {
    let mut ranges: HashMap<CodeBlockId, Range<usize>> = HashMap::new();
    for (index, paint) in paints.iter().enumerate() {
        if let Some(block) = paint.code_block {
            ranges
                .entry(block)
                .and_modify(|range| range.end = index + 1)
                .or_insert(index..index + 1);
        }
    }
    ranges
}

/// A code block's text, as its copy button copies it: without the final newline.
pub(crate) fn block_text(code_texts: &[String], block: CodeBlockId) -> Option<String> {
    let text = code_texts.get(block.index())?;
    Some(text.strip_suffix('\n').unwrap_or(text).to_owned())
}

/// What the menu offers at a point of paint `paint`, most specific first: the inline code or
/// code block, the link, the image, then the paragraph or heading, or the table row and table.
/// Choices with nothing to copy are left out.
pub(crate) fn choices(
    paints: &[LeafPaint],
    paint: usize,
    under: &Under,
    code_texts: &[String],
) -> Vec<Choice> {
    let Some(this) = paints.get(paint) else {
        return Vec::new();
    };
    let mut choices = Vec::new();
    let mut offer = |label: &'static str, text: Option<String>| {
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            choices.push(Choice { label, text });
        }
    };
    let at = under.point;
    let inline = at
        .byte
        .and_then(|byte| match (&this.table, at.row, at.cell) {
            (Some(table), Some(row), Some(cell)) => {
                let cell = table.rows.get(row)?.cells.get(cell)?;
                code_span(&cell.text, &cell.attrs, byte)
            }
            (None, _, _) if this.code_block.is_none() => code_span(&this.text, &this.attrs, byte),
            _ => None,
        });
    offer(
        COPY_CODE,
        inline.or_else(|| {
            this.code_block
                .and_then(|block| block_text(code_texts, block))
        }),
    );
    offer(COPY_LINK, under.link.clone());
    offer(COPY_IMAGE, under.image.clone());
    if let Some(table) = &this.table {
        offer(
            COPY_ROW,
            at.row.and_then(|row| table.rows.get(row)).map(row_text),
        );
        offer(COPY_TABLE, Some(table_text(paints, table.columns.table)));
    } else if this.code_block.is_none() {
        let label = match this.font {
            FontRole::Heading(_) => COPY_HEADING,
            FontRole::Body | FontRole::Code => COPY_PARAGRAPH,
        };
        offer(label, Some(leaf_text(paints, paint)));
    }
    choices
}

/// The inline code (or keyboard key, or math) at byte `byte` of `text`, if any.
fn code_span(text: &str, attrs: &[TextAttr], byte: usize) -> Option<String> {
    let attr = attrs.iter().find(|attr| {
        matches!(attr.kind, AttrKind::Code) && (attr.start..attr.end).contains(&byte)
    })?;
    text.get(attr.start..attr.end).map(shown)
}

/// The text of the leaf paint `paint` is (part of): all its pieces.
fn leaf_text(paints: &[LeafPaint], paint: usize) -> String {
    let source = paints.get(paint).map(|paint| paint.source);
    let same = |index: &usize| paints.get(*index).map(|paint| paint.source) == source;
    let first = (0..=paint).rev().take_while(same).last().unwrap_or(paint);
    let pieces: Vec<&str> = (first..paints.len())
        .take_while(same)
        .filter_map(|index| paints.get(index).map(|paint| paint.text.as_str()))
        .collect();
    // A piece is cut at a line break, which it drops.
    shown(&pieces.join("\n"))
}

/// A table row as a line of tab-separated cells (as a spreadsheet pastes it).
fn row_text(row: &RowPaint) -> String {
    row.cells
        .iter()
        .map(|cell| shown(&cell.text).replace('\n', " "))
        .collect::<Vec<_>>()
        .join("\t")
}

/// A whole table, header first, a row a line.
fn table_text(paints: &[LeafPaint], table: TableId) -> String {
    paints
        .iter()
        .filter_map(|paint| paint.table.as_ref())
        .filter(|piece| piece.columns.table == table)
        .flat_map(|piece| piece.rows.iter().map(row_text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Text as copied: images (their object characters) left out, and no space around it.
fn shown(text: &str) -> String {
    text.replace(Inlines::OBJECT, "").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::paint::paint_document;

    fn load(markdown: &str) -> (Vec<LeafPaint>, Vec<String>) {
        let doc = galley_core::load(markdown.as_bytes());
        (paint_document(&doc), code_texts(&doc))
    }

    fn labels(choices: &[Choice]) -> Vec<&str> {
        choices.iter().map(|choice| choice.label).collect()
    }

    fn text_of<'a>(choices: &'a [Choice], label: &str) -> Option<&'a str> {
        choices
            .iter()
            .find(|choice| choice.label == label)
            .map(|choice| choice.text.as_str())
    }

    #[test]
    fn a_code_block_copies_whole_without_its_final_newline() {
        let (paints, code) = load("```sh\necho one\necho two\n```\n");
        let choices = choices(&paints, 0, &Under::default(), &code);
        assert_eq!(labels(&choices), [COPY_CODE]);
        assert_eq!(text_of(&choices, COPY_CODE), Some("echo one\necho two"));
        let ranges = code_block_ranges(&paints);
        assert_eq!(ranges.values().collect::<Vec<_>>(), [&(0..1)]);
        // A block with no language is a code block too.
        let (paints, code) = load("    indented\n");
        assert_eq!(
            text_of(&choices_at(&paints, &code, 0), COPY_CODE),
            Some("indented")
        );
    }

    fn choices_at(paints: &[LeafPaint], code: &[String], paint: usize) -> Vec<Choice> {
        choices(paints, paint, &Under::default(), code)
    }

    #[test]
    fn inline_code_under_the_pointer_comes_first() {
        let (paints, code) = load("Run `make install` now.\n");
        let text = &paints.first().expect("a paint").text;
        let byte = text.find("install");
        let under = Under {
            point: TextPoint {
                byte,
                ..TextPoint::default()
            },
            ..Under::default()
        };
        let choices = choices(&paints, 0, &under, &code);
        assert_eq!(labels(&choices), [COPY_CODE, COPY_PARAGRAPH]);
        assert_eq!(text_of(&choices, COPY_CODE), Some("make install"));
        assert_eq!(
            text_of(&choices, COPY_PARAGRAPH),
            Some("Run make install now.")
        );
        // Off the code: the paragraph only.
        let off = Under {
            point: TextPoint {
                byte: text.find("now"),
                ..TextPoint::default()
            },
            ..Under::default()
        };
        assert_eq!(
            labels(&super::choices(&paints, 0, &off, &code)),
            [COPY_PARAGRAPH]
        );
    }

    #[test]
    fn links_images_and_headings() {
        let (paints, code) = load("# The *title*\n\n![logo](logo.png)\n");
        assert_eq!(
            choices_at(&paints, &code, 0),
            [Choice {
                label: COPY_HEADING,
                text: "The title".to_owned()
            }]
        );
        // An image alone: no text to copy, only its address.
        let under = Under {
            image: Some("/docs/logo.png".to_owned()),
            link: Some("https://example.com/".to_owned()),
            ..Under::default()
        };
        assert_eq!(
            labels(&choices(&paints, 1, &under, &code)),
            [COPY_LINK, COPY_IMAGE]
        );
    }

    #[test]
    fn tables_copy_as_tab_separated_lines() {
        let (paints, code) = load("| a | b |\n|---|---|\n| 1 | `x y` |\n| 2 | ![i](i.png) z |\n");
        let row = |row, cell, byte| Under {
            point: TextPoint {
                row: Some(row),
                cell: Some(cell),
                byte,
            },
            ..Under::default()
        };
        let choices = choices(&paints, 0, &row(1, 1, Some(1)), &code);
        assert_eq!(labels(&choices), [COPY_CODE, COPY_ROW, COPY_TABLE]);
        assert_eq!(text_of(&choices, COPY_CODE), Some("x y"));
        assert_eq!(text_of(&choices, COPY_ROW), Some("1\tx y"));
        assert_eq!(text_of(&choices, COPY_TABLE), Some("a\tb\n1\tx y\n2\tz"));
        let header = super::choices(&paints, 0, &row(0, 0, None), &code);
        assert_eq!(text_of(&header, COPY_ROW), Some("a\tb"));
    }

    #[test]
    fn a_list_item_copies_without_its_marker() {
        let (paints, code) = load("- first item\n- second\n");
        assert_eq!(
            text_of(&choices_at(&paints, &code, 1), COPY_PARAGRAPH),
            Some("second")
        );
    }
}
