//! What each leaf looks like, as plain data (docs/plan.md §4.5).
//!
//! A `LeafPaint` is the text to lay out with its attributes, its font, and the box around it:
//! indent, gaps, background padding, quote and alert bars, list markers. It is built from the
//! document on the loader thread (no GTK here), is `Send`, and is what both the height worker
//! and the widget lay out, so the two cannot disagree about a leaf.
//!
//! Lengths are in em (multiples of the body font size): the paint does not know the screen's
//! resolution, which is applied when it is laid out (`geometry`).
//!
//! No paint's text is longer than `LEAF_TEXT_BYTES`, whatever the document holds: laying out one
//! huge leaf would stall a frame, so `split` cuts such paints into several. Paints therefore do
//! not map one to one onto the document's leaves; each records its `source` leaf.

use gbk_markdown_core::Document;
use gbk_markdown_core::model::{
    AlertKind, Align, Container, Delimiter, HeadingLevel, InlineObject, Inlines, LEAF_TEXT_BYTES,
    Leaf, LeafKind, Marker, Style,
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
pub(super) const CODE_PADDING: f64 = 0.6;
/// A paint with more text than this is cut at a line break if it has one: code and table rows
/// then lay out in pieces of about a millisecond each (a 200-line code chunk took 10-48 ms).
/// A cut between lines is invisible; a cut inside a paragraph is not, so prose is only cut at
/// `LEAF_TEXT_BYTES` (16 KiB of prose lays out in a few ms). A run this long with no space or
/// line break is cut too: Pango wraps such a run slowly (16 KiB took ~200 ms), and it wraps
/// at characters anyway, so the cut barely shows.
const SPLIT_AT_LINE_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LeafPaint {
    /// The index of the document leaf this paints, or part of.
    pub(crate) source: usize,
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

impl LeafPaint {
    /// Unstyled text in a font, with no box around it.
    pub(crate) fn plain(text: &str, font: FontRole, attrs: Vec<TextAttr>) -> Self {
        Self {
            source: 0,
            text: text.to_owned(),
            attrs,
            font,
            align: Align::Start,
            indent_em: 0.0,
            gap_above_em: 0.0,
            gap_below_em: 0.0,
            pad_above_em: 0.0,
            pad_below_em: 0.0,
            background: None,
            decorations: Vec::new(),
        }
    }
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
    /// A vertical bar `x_em` from the left margin, the full height of the leaf, except that it
    /// starts below the gap above the container's first leaf and ends above the gap below its
    /// last (so adjacent quotes stay apart).
    Bar {
        x_em: f64,
        color: BarColor,
        first: bool,
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

/// The paints of every leaf, in order.
pub(crate) fn paint_document(doc: &Document) -> Vec<LeafPaint> {
    let leaves: Vec<_> = doc.leaves_with_ids().collect();
    leaves
        .iter()
        .enumerate()
        .flat_map(|(index, &(id, leaf))| {
            // The containers the next leaf is in: any other ends at this leaf.
            let next: Vec<ContextId> = leaves
                .get(index + 1)
                .map(|&(_id, next)| doc.containers(next).map(|(id, _)| id).collect())
                .unwrap_or_default();
            split(paint_leaf(doc, index, id, leaf, &next))
        })
        .collect()
}

fn paint_leaf(
    doc: &Document,
    source: usize,
    id: LeafId,
    leaf: &Leaf,
    next: &[ContextId],
) -> LeafPaint {
    let mut paint = LeafPaint {
        source,
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
                    first: starts_here,
                    last,
                });
                paint.indent_em += QUOTE_INDENT;
            }
            Container::Alert(kind) => {
                paint.decorations.push(Decoration::Bar {
                    x_em: paint.indent_em,
                    color: BarColor::Alert(*kind),
                    first: starts_here,
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
    // Where runs of source text start, in the source and in the paint text: a placeholder
    // replaces each image's one object character, so positions after it move.
    let mut runs: Vec<(usize, usize)> = vec![(0, paint.text.len())];
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
        copied = at + Inlines::OBJECT.len_utf8();
        runs.push((copied, paint.text.len()));
    }
    paint
        .text
        .push_str(source.get(copied..).unwrap_or_default());
    let map = |position: usize| {
        let (from, to) = runs
            .iter()
            .rev()
            .find(|&&(from, _)| from <= position)
            .copied()
            .unwrap_or_default();
        to + (position - from)
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

/// Cuts a paint whose text is too long into pieces that each lay out quickly (see
/// `SPLIT_AT_LINE_BYTES` and `cuts`). The pieces stack into the original box: gaps, padding,
/// markers and bar ends stay with the first or last piece.
fn split(paint: LeafPaint) -> Vec<LeafPaint> {
    let cuts = cuts(&paint.text);
    if cuts.is_empty() {
        return vec![paint];
    }
    let count = cuts.len() + 1;
    let mut pieces = Vec::with_capacity(count);
    let mut from = 0;
    for (index, (to, next)) in cuts
        .into_iter()
        .chain([(paint.text.len(), paint.text.len())])
        .enumerate()
    {
        let (first, last) = (index == 0, index + 1 == count);
        let decorations = paint
            .decorations
            .iter()
            .filter_map(|decoration| match decoration {
                Decoration::Bar {
                    x_em,
                    color,
                    first: starts,
                    last: ends,
                } => Some(Decoration::Bar {
                    x_em: *x_em,
                    color: *color,
                    first: first && *starts,
                    last: last && *ends,
                }),
                Decoration::Marker { .. } | Decoration::Rule => first.then(|| decoration.clone()),
                Decoration::HeadingRule => last.then(|| decoration.clone()),
            })
            .collect();
        pieces.push(LeafPaint {
            source: paint.source,
            text: paint.text.get(from..to).unwrap_or_default().to_owned(),
            attrs: paint
                .attrs
                .iter()
                .filter(|attr| attr.start < to && attr.end > from)
                .map(|attr| TextAttr {
                    start: attr.start.clamp(from, to) - from,
                    end: attr.end.clamp(from, to) - from,
                    kind: attr.kind,
                })
                .collect(),
            font: paint.font,
            align: paint.align,
            indent_em: paint.indent_em,
            gap_above_em: if first { paint.gap_above_em } else { 0.0 },
            gap_below_em: if last { paint.gap_below_em } else { 0.0 },
            pad_above_em: if first { paint.pad_above_em } else { 0.0 },
            pad_below_em: if last { paint.pad_below_em } else { 0.0 },
            background: paint.background,
            decorations,
        });
        from = next;
    }
    pieces
}

/// Where to cut `text`: (end of a piece, start of the next). In order of preference: at a line
/// break in the first 2 KiB; before (or 2 KiB into) an unbreakable run; at the first line
/// break; after a space once over `LEAF_TEXT_BYTES`; else at a character boundary. A cut at a
/// line break drops the newline, since the next piece starts on a new line anyway.
fn cuts(text: &str) -> Vec<(usize, usize)> {
    let mut cuts = Vec::new();
    let mut start = 0;
    loop {
        let rest = text.get(start..).unwrap_or_default();
        if rest.len() <= SPLIT_AT_LINE_BYTES {
            return cuts;
        }
        let near = rest
            .get(..rest.floor_char_boundary(SPLIT_AT_LINE_BYTES))
            .unwrap_or_default();
        let far = rest
            .get(..rest.floor_char_boundary(LEAF_TEXT_BYTES))
            .unwrap_or_default();
        let cut = if let Some(newline) = near.rfind('\n') {
            (start + newline, start + newline + 1)
        } else if let Some(run) = unbreakable(far) {
            let at = if run == 0 { near.len() } else { run };
            (start + at, start + at)
        } else if let Some(newline) = far.find('\n') {
            (start + newline, start + newline + 1)
        } else if rest.len() > LEAF_TEXT_BYTES {
            // No unbreakable run in `far`, so it has a space in its last 2 KiB.
            let at = far.rfind(' ').map_or(far.len(), |space| space + 1);
            (start + at, start + at)
        } else {
            return cuts;
        };
        cuts.push(cut);
        start = cut.1;
    }
}

/// Where the first run longer than `SPLIT_AT_LINE_BYTES` with no space or line break starts.
fn unbreakable(text: &str) -> Option<usize> {
    let mut start = 0;
    for (index, _break) in text.match_indices([' ', '\n']).chain([(text.len(), "")]) {
        if index - start > SPLIT_AT_LINE_BYTES {
            return Some(start);
        }
        start = index + 1;
    }
    None
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

    fn bar_ends(paint: &LeafPaint) -> Vec<(bool, bool)> {
        paint
            .decorations
            .iter()
            .filter_map(|decoration| match decoration {
                Decoration::Bar { first, last, .. } => Some((*first, *last)),
                Decoration::Marker { .. } | Decoration::Rule | Decoration::HeadingRule => None,
            })
            .collect()
    }

    #[test]
    fn bars_know_their_containers_first_and_last_leaves() {
        let paints = paints("> one\n>\n> two\n>\n> three\n");
        let ends: Vec<_> = paints.iter().map(bar_ends).collect();
        assert_eq!(
            ends,
            vec![
                vec![(true, false)],
                vec![(false, false)],
                vec![(false, true)]
            ]
        );
    }

    #[test]
    fn long_code_splits_between_lines_into_quick_pieces() {
        // One 199-line chunk in the model, 31 bytes a line: about 6 KB, in a quote.
        let code = (0..199)
            .map(|i| format!("{i:030}\n"))
            .collect::<Vec<_>>()
            .concat();
        let quoted = code
            .lines()
            .map(|line| format!("> {line}\n"))
            .collect::<Vec<_>>()
            .concat();
        let paints = paints(&format!("> ```\n{quoted}> ```\n"));
        assert_eq!(paints.len(), 4);
        assert!(
            paints
                .iter()
                .all(|paint| paint.text.len() <= SPLIT_AT_LINE_BYTES)
        );
        assert!(paints.iter().all(|paint| paint.source == 0));
        let joined: Vec<&str> = paints.iter().map(|paint| paint.text.as_str()).collect();
        assert_eq!(joined.join("\n"), code.trim_end());
        let first = paints.first().expect("first");
        let last = paints.last().expect("last");
        assert!((first.pad_above_em - CODE_PADDING).abs() < f64::EPSILON);
        assert!((last.pad_below_em - CODE_PADDING).abs() < f64::EPSILON);
        assert!((last.gap_below_em - PARAGRAPH_GAP).abs() < f64::EPSILON);
        assert!(
            paints
                .iter()
                .skip(1)
                .all(|paint| paint.pad_above_em == 0.0 && paint.gap_above_em == 0.0)
        );
        assert!(
            paints
                .iter()
                .rev()
                .skip(1)
                .all(|paint| paint.pad_below_em == 0.0 && paint.gap_below_em == 0.0)
        );
        assert_eq!(bar_ends(first), vec![(true, false)]);
        assert_eq!(bar_ends(last), vec![(false, true)]);
        assert!(
            paints
                .iter()
                .all(|paint| paint.background == Some(Fill::Code))
        );
    }

    #[test]
    fn prose_splits_only_past_the_leaf_cap_and_unbreakable_runs_at_2_kib() {
        let prose = "word ".repeat(LEAF_TEXT_BYTES / 5 - 10);
        assert_eq!(paints(&prose).len(), 1);
        let huge = "y".repeat(3 * LEAF_TEXT_BYTES + 5);
        let pieces = paints(&format!("```\n{huge}\n```\n"));
        assert_eq!(pieces.len(), 25);
        assert!(
            pieces
                .iter()
                .all(|paint| paint.text.len() <= SPLIT_AT_LINE_BYTES)
        );
        let joined: String = pieces.iter().map(|paint| paint.text.as_str()).collect();
        assert_eq!(joined, huge);
    }

    #[test]
    fn a_long_heading_keeps_its_rule_and_attributes_on_the_right_pieces() {
        let words = "word ".repeat(LEAF_TEXT_BYTES / 5 * 2);
        let heading = format!("# {} [link](x)\n", words.trim_end());
        // 32 KiB: one cut after the last space within 16 KiB leaves exactly 16 KiB.
        let pieces = paints(&heading);
        assert_eq!(pieces.len(), 2);
        assert!(
            pieces
                .iter()
                .all(|paint| paint.text.len() <= LEAF_TEXT_BYTES)
        );
        // Cut after a space, so no word is broken.
        assert!(
            pieces
                .iter()
                .rev()
                .skip(1)
                .all(|paint| paint.text.ends_with(' '))
        );
        let last = pieces.last().expect("last");
        assert_eq!(last.decorations, vec![Decoration::HeadingRule]);
        assert!(
            pieces
                .iter()
                .rev()
                .skip(1)
                .all(|paint| paint.decorations.is_empty())
        );
        for paint in &pieces {
            for attr in &paint.attrs {
                assert!(attr.start < attr.end && attr.end <= paint.text.len());
                assert!(
                    paint.text.is_char_boundary(attr.start)
                        && paint.text.is_char_boundary(attr.end)
                );
            }
        }
        assert!(last.attrs.iter().any(|attr| attr.kind == AttrKind::Link));
    }

    #[test]
    fn cuts_prefer_line_breaks_and_stay_on_character_boundaries() {
        assert_eq!(cuts("short"), Vec::new());
        // A line break in the first 2 KiB: cut there, dropping it; then an unbreakable run.
        let text = format!("{}\n{}", "a".repeat(1000), "b".repeat(3000));
        assert_eq!(cuts(&text), vec![(1000, 1001), (3049, 3049)]);
        // Prose with its first line break past 2 KiB: cut at that line break.
        let text = format!("{}\n{}", "ab ".repeat(1700), "c ".repeat(500));
        assert_eq!(cuts(&text), vec![(5100, 5101)]);
        // Prose then an unbreakable run: cut just before the run, then 2 KiB into it.
        let text = format!("{}{}", "ab ".repeat(10), "z".repeat(3000));
        assert_eq!(cuts(&text), vec![(30, 30), (2078, 2078)]);
        // Prose over the cap: after the last space within it.
        let text = "abc ".repeat(LEAF_TEXT_BYTES / 2);
        assert_eq!(cuts(&text), vec![(LEAF_TEXT_BYTES, LEAF_TEXT_BYTES)]);
        // Multi-byte text with no spaces: cuts land on character boundaries.
        let text = "é".repeat(LEAF_TEXT_BYTES);
        let found = cuts(&text);
        assert_eq!(found.len(), 15);
        assert!(
            found
                .iter()
                .all(|&(end, next)| end == next && text.is_char_boundary(end))
        );
        assert_eq!(unbreakable("a b"), None);
        assert_eq!(unbreakable(&format!("a\n{}", "x".repeat(2049))), Some(2));
        assert_eq!(unbreakable(&"x".repeat(2048)), None);
    }

    #[test]
    fn paints_record_their_source_leaf() {
        let paints = paints("one\n\ntwo\n\nthree\n");
        let sources: Vec<usize> = paints.iter().map(|paint| paint.source).collect();
        assert_eq!(sources, vec![0, 1, 2]);
    }

    #[test]
    fn tables_are_rows_of_cells_with_a_bold_header() {
        let short = paints("| A | B |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n");
        let table = short.first().expect("table");
        assert_eq!(table.text, "A  │  B\n1  │  2\n3  │  4");
        assert!(table.attrs.contains(&TextAttr {
            start: 0,
            end: "A  │  B".len(),
            kind: AttrKind::Bold
        }));
        assert!((table.gap_below_em - PARAGRAPH_GAP).abs() < f64::EPSILON);
        let rows = (0..60)
            .map(|i| format!("| {i} |\n"))
            .collect::<Vec<_>>()
            .concat();
        let long = paints(&format!("| N |\n|---|\n{rows}"));
        let gaps: Vec<f64> = long.iter().map(|paint| paint.gap_below_em).collect();
        assert_eq!(gaps, vec![0.0, PARAGRAPH_GAP]);
    }

    #[test]
    fn details_alerts_and_footnotes_indent_their_content() {
        let paints = paints(
            "<details><summary>More</summary>\n\nbody\n\n</details>\n\n> [!TIP]\n> tip\n\nx[^n]\n\n[^n]: note\n",
        );
        let summary = paints.first().expect("summary");
        assert_eq!(summary.text, "More");
        assert_eq!(
            markers(summary),
            vec![(DETAILS_INDENT - MARKER_GAP, "▾".to_owned())]
        );
        assert!((summary.gap_below_em - PARAGRAPH_GAP / 2.0).abs() < f64::EPSILON);
        assert!((summary.indent_em - DETAILS_INDENT).abs() < f64::EPSILON);
        let body = paints.get(1).expect("details body");
        assert_eq!(body.text, "body");
        assert!((body.indent_em - DETAILS_INDENT).abs() < f64::EPSILON);
        let tip = paints.get(3).expect("alert body");
        assert_eq!(tip.text, "tip");
        assert!((tip.indent_em - QUOTE_INDENT).abs() < f64::EPSILON);
        let note = paints.last().expect("footnote");
        assert_eq!(note.text, "note");
        assert!((note.indent_em - LIST_INDENT).abs() < f64::EPSILON);
        assert_eq!(
            markers(note),
            vec![(LIST_INDENT - MARKER_GAP, "1.".to_owned())]
        );
    }

    #[test]
    fn attributes_ending_or_starting_at_a_cut_stay_on_their_own_side() {
        let text = format!("{}\n{}", "a".repeat(2000), "b".repeat(2000));
        let attrs = vec![
            TextAttr {
                start: 0,
                end: 2001,
                kind: AttrKind::Italic,
            },
            TextAttr {
                start: 2000,
                end: 4001,
                kind: AttrKind::Bold,
            },
        ];
        let pieces = split(LeafPaint::plain(&text, FontRole::Body, attrs));
        let kinds: Vec<Vec<(usize, usize, AttrKind)>> = pieces
            .iter()
            .map(|paint| {
                paint
                    .attrs
                    .iter()
                    .map(|attr| (attr.start, attr.end, attr.kind))
                    .collect()
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                vec![(0, 2000, AttrKind::Italic)],
                vec![(0, 2000, AttrKind::Bold)]
            ]
        );
    }
}
