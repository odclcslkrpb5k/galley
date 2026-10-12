//! What each leaf looks like, as plain data (docs/plan.md §4.5).
//!
//! A `LeafPaint` is the text to lay out with its attributes, its font, and the box around it:
//! indent, gaps, background padding, quote and alert bars, list markers. It is built from the
//! document on the loader thread (no GTK here), is `Send`, and is what both the height worker
//! and the widget lay out, so the two cannot disagree about a leaf.
//!
//! Lengths are in em (multiples of the body font size): the paint does not know the screen's
//! resolution, which is applied when it is laid out.

use gbk_markdown_core::Document;
use gbk_markdown_core::model::{
    AlertKind, Align, Container, Delimiter, HeadingLevel, InlineObject, Inlines, Leaf, LeafKind,
    Marker, Style,
};
use gbk_markdown_core::units::{ContextId, LeafId};

/// Indent per container level, in em.
const QUOTE_INDENT: f64 = 1.0;
const LIST_INDENT: f64 = 1.75;
const DETAILS_INDENT: f64 = 1.0;
/// Gap between a marker and its item's text, in em.
const MARKER_GAP: f64 = 0.4;
/// Space after a paragraph, in em; less inside a tight list.
const PARAGRAPH_GAP: f64 = 0.75;
const TIGHT_GAP: f64 = 0.15;
/// Padding inside a code block's background, in em.
const CODE_PADDING: f64 = 0.6;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LeafPaint {
    pub(crate) text: String,
    pub(crate) attrs: Vec<TextAttr>,
    pub(crate) font: FontRole,
    pub(crate) align: Align,
    /// Distance of the text from the left margin.
    pub(crate) indent_em: f64,
    /// Space above and below the leaf's box, outside any background.
    pub(crate) gap_above_em: f64,
    pub(crate) gap_below_em: f64,
    /// Space between the background's edge and the text, above and below.
    pub(crate) pad_above_em: f64,
    pub(crate) pad_below_em: f64,
    pub(crate) background: Option<Fill>,
    pub(crate) decorations: Vec<Decoration>,
}

/// An attribute over bytes `start..end` of the paint's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextAttr {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) kind: AttrKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttrKind {
    Bold,
    Italic,
    Strikethrough,
    Underline,
    /// Inline code, keyboard keys and math: monospace on a faint background.
    Code,
    Link,
    Subscript,
    Superscript,
    Highlight,
    /// Secondary text: image placeholders.
    Muted,
    /// An alert's title, in its colour.
    AlertTitle(AlertKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FontRole {
    Body,
    Heading(HeadingLevel),
    Code,
}

impl FontRole {
    /// Size relative to the body font, as GitHub's stylesheet has it.
    pub(crate) fn scale(self) -> f64 {
        match self {
            Self::Body => 1.0,
            Self::Code => 0.85,
            Self::Heading(level) => match level {
                HeadingLevel::H1 => 2.0,
                HeadingLevel::H2 => 1.5,
                HeadingLevel::H3 => 1.25,
                HeadingLevel::H4 => 1.0,
                HeadingLevel::H5 => 0.875,
                HeadingLevel::H6 => 0.85,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fill {
    Code,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Decoration {
    /// A vertical bar `x_em` from the left margin, the full height of the leaf, or down to
    /// its text's end when this is the container's last leaf (so adjacent quotes stay apart).
    Bar {
        x_em: f64,
        color: BarColor,
        last: bool,
    },
    /// A list marker or footnote number, right-aligned to end at `x_em`.
    Marker { x_em: f64, text: String },
    /// A horizontal rule across the leaf (thematic break).
    Rule,
    /// The line under an h1 or h2, as on GitHub.
    HeadingRule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BarColor {
    Quote,
    Alert(AlertKind),
}

/// The paint of every leaf, in order.
pub(crate) fn paint_document(doc: &Document) -> Vec<LeafPaint> {
    let leaves: Vec<_> = doc.leaves_with_ids().collect();
    leaves
        .iter()
        .enumerate()
        .map(|(index, &(id, leaf))| {
            // The containers the next leaf is in: any other ends at this leaf.
            let next: Vec<ContextId> = leaves
                .get(index + 1)
                .map(|&(_id, next)| doc.containers(next).map(|(id, _)| id).collect())
                .unwrap_or_default();
            paint_leaf(doc, id, leaf, &next)
        })
        .collect()
}

fn paint_leaf(doc: &Document, id: LeafId, leaf: &Leaf, next: &[ContextId]) -> LeafPaint {
    let mut paint = LeafPaint {
        text: String::new(),
        attrs: Vec::new(),
        font: FontRole::Body,
        align: leaf.align(),
        indent_em: 0.0,
        gap_above_em: 0.0,
        gap_below_em: PARAGRAPH_GAP,
        pad_above_em: 0.0,
        pad_below_em: 0.0,
        background: None,
        decorations: Vec::new(),
    };
    let tight = containers(&mut paint, doc, id, leaf, next);
    match leaf.kind() {
        LeafKind::Paragraph(inlines) => {
            push_inlines(&mut paint, doc, inlines);
            if tight {
                paint.gap_below_em = TIGHT_GAP;
            }
        }
        LeafKind::Heading { level, inlines, .. } => {
            push_inlines(&mut paint, doc, inlines);
            paint.font = FontRole::Heading(*level);
            paint.attrs.push(whole(&paint.text, AttrKind::Bold));
            paint.gap_above_em = 1.0;
            paint.gap_below_em = 0.5;
            if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                paint.decorations.push(Decoration::HeadingRule);
            }
        }
        LeafKind::Code { block, lines } => {
            let code = doc.code_block(*block);
            code.and_then(|code| code.lines(lines))
                .unwrap_or_default()
                .clone_into(&mut paint.text);
            paint.font = FontRole::Code;
            paint.background = Some(Fill::Code);
            let first = lines.start.index() == 0;
            let last = code.is_none_or(|code| lines.end.index() >= code.line_count());
            paint.pad_above_em = if first { CODE_PADDING } else { 0.0 };
            paint.pad_below_em = if last { CODE_PADDING } else { 0.0 };
            paint.gap_below_em = if last { PARAGRAPH_GAP } else { 0.0 };
        }
        LeafKind::TableRows {
            table,
            rows,
            with_header,
        } => {
            // M2 shows tables as aligned text rows; M3 draws them as tables.
            if let Some(table) = doc.table(*table) {
                if *with_header {
                    push_row(&mut paint, doc, table.header());
                    let header_end = paint.text.len();
                    paint.attrs.push(TextAttr {
                        start: 0,
                        end: header_end,
                        kind: AttrKind::Bold,
                    });
                }
                for row in table.row_range(rows).unwrap_or_default() {
                    if !paint.text.is_empty() {
                        paint.text.push('\n');
                    }
                    push_row(&mut paint, doc, row);
                }
                let last = rows.end.index() >= table.rows().len();
                paint.gap_below_em = if last { PARAGRAPH_GAP } else { 0.0 };
            }
        }
        LeafKind::ThematicBreak => {
            paint.decorations.push(Decoration::Rule);
            paint.gap_above_em = 0.5;
        }
        LeafKind::AlertTitle(kind) => {
            kind.title().clone_into(&mut paint.text);
            paint
                .attrs
                .push(whole(&paint.text, AttrKind::AlertTitle(*kind)));
            paint.attrs.push(whole(&paint.text, AttrKind::Bold));
            paint.gap_below_em = TIGHT_GAP;
        }
        LeafKind::DetailsSummary { inlines, .. } => {
            push_inlines(&mut paint, doc, inlines);
            paint.decorations.push(Decoration::Marker {
                x_em: paint.indent_em - MARKER_GAP,
                text: "▾".to_owned(),
            });
            paint.gap_below_em = PARAGRAPH_GAP / 2.0;
        }
    }
    paint
}

/// Applies the leaf's containers, outermost first: indent, bars, markers. Returns whether the
/// leaf sits directly in an item of a tight list.
fn containers(
    paint: &mut LeafPaint,
    doc: &Document,
    id: LeafId,
    leaf: &Leaf,
    next: &[ContextId],
) -> bool {
    let mut stack: Vec<_> = doc.containers(leaf).collect();
    let tight = matches!(
        stack.first().map(|(_id, context)| context.container()),
        Some(Container::ListItem { tight: true, .. })
    );
    stack.reverse();
    let mut list_depth = 0;
    for (context_id, context) in stack {
        let starts_here = context.starts_at(id);
        let last = !next.contains(&context_id);
        match context.container() {
            Container::Quote => {
                paint.decorations.push(Decoration::Bar {
                    x_em: paint.indent_em,
                    color: BarColor::Quote,
                    last,
                });
                paint.indent_em += QUOTE_INDENT;
            }
            Container::Alert(kind) => {
                paint.decorations.push(Decoration::Bar {
                    x_em: paint.indent_em,
                    color: BarColor::Alert(*kind),
                    last,
                });
                paint.indent_em += QUOTE_INDENT;
            }
            Container::ListItem { marker, .. } => {
                paint.indent_em += LIST_INDENT;
                if starts_here {
                    paint.decorations.push(Decoration::Marker {
                        x_em: paint.indent_em - MARKER_GAP,
                        text: marker_text(*marker, list_depth),
                    });
                }
                list_depth += 1;
            }
            Container::Footnote(footnote) => {
                paint.indent_em += LIST_INDENT;
                if starts_here && let Some(footnote) = doc.footnote(*footnote) {
                    paint.decorations.push(Decoration::Marker {
                        x_em: paint.indent_em - MARKER_GAP,
                        text: format!("{}.", footnote.number()),
                    });
                }
            }
            Container::Details { .. } => paint.indent_em += DETAILS_INDENT,
        }
    }
    tight
}

fn marker_text(marker: Marker, depth: usize) -> String {
    match marker {
        Marker::Bullet => match depth % 3 {
            0 => "•",
            1 => "◦",
            _ => "▪",
        }
        .to_owned(),
        Marker::Ordered { number, delimiter } => match delimiter {
            Delimiter::Period => format!("{number}."),
            Delimiter::Paren => format!("{number})"),
        },
        Marker::Task { checked } => if checked { "☑" } else { "☐" }.to_owned(),
    }
}

fn whole(text: &str, kind: AttrKind) -> TextAttr {
    TextAttr {
        start: 0,
        end: text.len(),
        kind,
    }
}

fn push_row(paint: &mut LeafPaint, doc: &Document, cells: &[Inlines]) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            paint.text.push_str("  │  ");
        }
        push_inlines(paint, doc, cell);
    }
}

/// Appends styled text. Images become "[alt]" placeholders until M4 draws them.
fn push_inlines(paint: &mut LeafPaint, doc: &Document, inlines: &Inlines) {
    let source = inlines.text();
    let base = paint.text.len();
    // Byte position in the paint text for each position in the source text.
    let mut shifts: Vec<(usize, isize)> = Vec::new();
    let mut copied = 0;
    for (offset, object) in inlines.objects() {
        let at = offset.index();
        paint
            .text
            .push_str(source.get(copied..at).unwrap_or_default());
        let InlineObject::Image(image) = object;
        let alt = doc.image(*image).map_or("", |image| image.alt());
        let placeholder = format!("[{}]", if alt.is_empty() { "image" } else { alt });
        let start = paint.text.len();
        paint.text.push_str(&placeholder);
        paint.attrs.push(TextAttr {
            start,
            end: paint.text.len(),
            kind: AttrKind::Muted,
        });
        let object_len = Inlines::OBJECT.len_utf8();
        shifts.push((
            at + object_len,
            isize::try_from(placeholder.len()).unwrap_or(isize::MAX)
                - isize::try_from(object_len).unwrap_or(0),
        ));
        copied = at + object_len;
    }
    paint
        .text
        .push_str(source.get(copied..).unwrap_or_default());
    let map = |position: usize| {
        let shift: isize = shifts
            .iter()
            .filter(|&&(from, _)| from <= position)
            .map(|&(_, shift)| shift)
            .sum();
        base + position.saturating_add_signed(shift)
    };
    for span in inlines.spans() {
        let range = span.range();
        let (start, end) = (map(range.start.index()), map(range.end.index()));
        let style = span.style();
        for (bit, kind) in [
            (Style::STRONG, AttrKind::Bold),
            (Style::EMPHASIS, AttrKind::Italic),
            (Style::STRIKETHROUGH, AttrKind::Strikethrough),
            (Style::UNDERLINE, AttrKind::Underline),
            (Style::CODE, AttrKind::Code),
            (Style::KEYBOARD, AttrKind::Code),
            (Style::MATH, AttrKind::Code),
            (Style::DISPLAY_MATH, AttrKind::Code),
            (Style::SUBSCRIPT, AttrKind::Subscript),
            (Style::SUPERSCRIPT, AttrKind::Superscript),
            (Style::FOOTNOTE_REFERENCE, AttrKind::Superscript),
            (Style::HIGHLIGHT, AttrKind::Highlight),
        ] {
            if style.contains(bit) {
                paint.attrs.push(TextAttr { start, end, kind });
            }
        }
        if span.link().is_some() {
            paint.attrs.push(TextAttr {
                start,
                end,
                kind: AttrKind::Link,
            });
        }
    }
}

/// Lengths of a laid-out leaf, in whole logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Metrics {
    /// The body font's em, in logical pixels.
    pub(crate) em_px: f64,
}

impl Metrics {
    pub(crate) fn px(self, em: f64) -> i64 {
        crate::units::round_px(em * self.em_px)
    }

    /// A leaf's full height given its text's laid-out height.
    pub(crate) fn leaf_height(self, paint: &LeafPaint, text_height: i64) -> i64 {
        self.px(paint.gap_above_em)
            + self.px(paint.pad_above_em)
            + text_height
            + self.px(paint.pad_below_em)
            + self.px(paint.gap_below_em)
    }

    /// Space between a background's left or right edge and the text.
    pub(crate) fn horizontal_padding(self, paint: &LeafPaint) -> i64 {
        if paint.background.is_some() {
            self.px(CODE_PADDING)
        } else {
            0
        }
    }

    /// The width text is wrapped to, given the view's text width.
    pub(crate) fn text_width(self, paint: &LeafPaint, view_text_width: i64) -> i64 {
        (view_text_width - self.px(paint.indent_em) - 2 * self.horizontal_padding(paint)).max(1)
    }

    /// A leaf's height estimated without layout: characters over an average width per line.
    pub(crate) fn estimate(self, paint: &LeafPaint, view_text_width: i64) -> i64 {
        let scale = paint.font.scale();
        let line = crate::units::round_px(self.em_px * scale * 1.4);
        let char_px = self.em_px * scale * 0.5;
        let width = crate::units::count_to_f64(
            usize::try_from(self.text_width(paint, view_text_width)).unwrap_or(1),
        );
        let per_line = (width / char_px).max(1.0);
        let lines: i64 = paint
            .text
            .split('\n')
            .map(|line| {
                let chars = crate::units::count_to_f64(line.chars().count());
                crate::units::round_px((chars / per_line).ceil().max(1.0))
            })
            .sum();
        self.leaf_height(paint, lines.max(1) * line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paints(markdown: &str) -> Vec<LeafPaint> {
        paint_document(&gbk_markdown_core::load(markdown.as_bytes()))
    }

    fn markers(paint: &LeafPaint) -> Vec<(f64, String)> {
        paint
            .decorations
            .iter()
            .filter_map(|decoration| match decoration {
                Decoration::Marker { x_em, text } => Some((*x_em, text.clone())),
                Decoration::Bar { .. } | Decoration::Rule | Decoration::HeadingRule => None,
            })
            .collect()
    }

    #[test]
    fn list_markers_number_and_nest() {
        let paints = paints("3. a\n4. b\n   - c\n     - d\n- [x] e\n");
        let all: Vec<Vec<String>> = paints
            .iter()
            .map(|paint| markers(paint).into_iter().map(|(_, text)| text).collect())
            .collect();
        assert_eq!(
            all,
            vec![
                vec!["3.".to_owned()],
                vec!["4.".to_owned()],
                vec!["◦".to_owned()],
                vec!["▪".to_owned()],
                vec!["☑".to_owned()],
            ]
        );
        let indents: Vec<f64> = paints.iter().map(|paint| paint.indent_em).collect();
        assert_eq!(indents, vec![1.75, 1.75, 3.5, 5.25, 1.75]);
        assert!(paints.iter().all(|paint| paint.gap_below_em == TIGHT_GAP));
    }

    #[test]
    fn a_marker_shows_only_on_an_items_first_leaf() {
        let paints = paints("- first\n\n  second\n");
        assert_eq!(markers(paints.first().expect("first")).len(), 1);
        assert_eq!(markers(paints.get(1).expect("second")).len(), 0);
        assert!(
            paints
                .iter()
                .all(|paint| paint.gap_below_em == PARAGRAPH_GAP)
        );
    }

    #[test]
    fn quotes_and_alerts_draw_bars_at_their_depth() {
        let paints = paints("> > nested\n\n> [!WARNING]\n> careful\n");
        let bars = |paint: &LeafPaint| -> Vec<(f64, BarColor)> {
            paint
                .decorations
                .iter()
                .filter_map(|decoration| match decoration {
                    Decoration::Bar { x_em, color, .. } => Some((*x_em, *color)),
                    Decoration::Marker { .. } | Decoration::Rule | Decoration::HeadingRule => None,
                })
                .collect()
        };
        assert_eq!(
            bars(paints.first().expect("nested")),
            vec![(0.0, BarColor::Quote), (1.0, BarColor::Quote)]
        );
        let alert = paints.get(1).expect("alert title");
        assert_eq!(alert.text, "Warning");
        assert_eq!(
            bars(alert),
            vec![(0.0, BarColor::Alert(AlertKind::Warning))]
        );
        let last = |paint: &LeafPaint| -> Vec<bool> {
            paint
                .decorations
                .iter()
                .filter_map(|decoration| match decoration {
                    Decoration::Bar { last, .. } => Some(*last),
                    Decoration::Marker { .. } | Decoration::Rule | Decoration::HeadingRule => None,
                })
                .collect()
        };
        // The nested quote ends at its only leaf; the alert title is not its alert's last.
        assert_eq!(last(paints.first().expect("nested")), vec![true, true]);
        assert_eq!(last(alert), vec![false]);
        assert_eq!(last(paints.get(2).expect("alert body")), vec![true]);
    }

    #[test]
    fn code_chunks_pad_and_gap_only_at_the_block_edges() {
        let code = (0..450)
            .map(|i| format!("line {i}\n"))
            .collect::<Vec<_>>()
            .concat();
        let paints = paints(&format!("```rust\n{code}```\n"));
        let edges: Vec<(f64, f64, f64)> = paints
            .iter()
            .map(|paint| (paint.pad_above_em, paint.pad_below_em, paint.gap_below_em))
            .collect();
        assert_eq!(
            edges,
            vec![
                (CODE_PADDING, 0.0, 0.0),
                (0.0, 0.0, 0.0),
                (0.0, CODE_PADDING, PARAGRAPH_GAP)
            ]
        );
        assert!(paints.iter().all(|paint| paint.font == FontRole::Code));
        assert!(
            paints
                .first()
                .is_some_and(|paint| paint.text.starts_with("line 0\n"))
        );
        assert!(
            paints
                .first()
                .is_some_and(|paint| !paint.text.ends_with('\n'))
        );
    }

    #[test]
    fn images_become_placeholders_and_attributes_follow_them() {
        let paints = paints("a ![logo](x.png) **bold** ![](y.png) [link](z)");
        let paint = paints.first().expect("paragraph");
        assert_eq!(paint.text, "a [logo] bold [image] link");
        let at = |kind: AttrKind| -> Vec<&str> {
            paint
                .attrs
                .iter()
                .filter(|attr| attr.kind == kind)
                .map(|attr| paint.text.get(attr.start..attr.end).expect("in text"))
                .collect()
        };
        assert_eq!(at(AttrKind::Bold), vec!["bold"]);
        assert_eq!(at(AttrKind::Muted), vec!["[logo]", "[image]"]);
        assert_eq!(at(AttrKind::Link), vec!["link"]);
    }

    #[test]
    fn headings_are_bold_scaled_and_ruled() {
        let paints = paints("# One\n\n### Three\n");
        let one = paints.first().expect("h1");
        assert_eq!(one.font, FontRole::Heading(HeadingLevel::H1));
        assert_eq!(one.decorations, vec![Decoration::HeadingRule]);
        assert!(one.attrs.contains(&TextAttr {
            start: 0,
            end: 3,
            kind: AttrKind::Bold
        }));
        let three = paints.get(1).expect("h3");
        assert_eq!(three.decorations, Vec::new());
        assert!(FontRole::Heading(HeadingLevel::H1).scale() > three.font.scale());
    }

    #[test]
    fn heights_add_up_and_estimates_grow_with_text() {
        let metrics = Metrics { em_px: 16.0 };
        let paints = paints("short\n\n```\ncode\n```\n");
        let short = paints.first().expect("paragraph");
        assert_eq!(metrics.leaf_height(short, 20), 20 + 12);
        let code = paints.get(1).expect("code");
        assert_eq!(metrics.leaf_height(code, 20), 10 + 20 + 10 + 12);
        assert_eq!(metrics.text_width(code, 600), 600 - 20);
        let long = LeafPaint {
            text: "word ".repeat(400),
            ..short.clone()
        };
        assert!(metrics.estimate(&long, 600) > 5 * metrics.estimate(short, 600));
    }
}
