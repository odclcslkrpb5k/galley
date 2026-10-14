//! What each leaf looks like, as plain data (docs/plan.md §4.5).
//!
//! A `LeafPaint` is the text to lay out with its attributes and links (or a piece of a table, its
//! cells each such text), its font, and the box around it: indent, gaps, background padding,
//! quote and alert bars, list markers, a summary's disclosure, the `<details>` it is inside. It
//! is built from the document on the loader thread (no GTK here), is `Send`, and is what both
//! the height worker and the widget lay out, so the two cannot disagree about a leaf. Links are
//! resolved here, to a leaf, a URL or a relative path, so clicking needs no document.
//!
//! Lengths are in em (multiples of the body font size): the paint does not know the screen's
//! resolution, which is applied when it is laid out (`geometry`).
//!
//! No paint's text is longer than `LEAF_TEXT_BYTES`, whatever the document holds: laying out one
//! huge leaf would stall a frame, so `split` cuts such paints into several. Paints therefore do
//! not map one to one onto the document's leaves; each records its `source` leaf.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use galley_core::Document;
use galley_core::model::{
    AlertKind, Align, Container, Delimiter, HeadingLevel, InlineObject, Inlines, LEAF_TEXT_BYTES,
    Leaf, LeafKind, LinkTarget, Marker, Style,
};
use galley_core::units::{
    ContextId, DetailsId, FootnoteId, ImageId, LeafId, LinkId, RowNo, TableId,
};

use super::highlight::{self, CodeSpan, TokenClass};
use super::image::{ImageInfo, ImageSource};
use super::table::{CellPaint, ColumnSample, RowPaint, TablePaint};

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
/// Most text in one table cell: a cell is laid out whole, and no README needs more. Longer
/// cells are cut short with an ellipsis (a listed deviation, plan §5).
const CELL_TEXT_BYTES: usize = 2 * 1024;

/// Most rows and text a table's column widths are measured from (`table::ColumnSample`), and
/// most text of one sampled cell.
const SAMPLE_BYTES: usize = 8 * 1024;
const SAMPLE_CELL_BYTES: usize = 256;
/// Most cells measured for a sample, and laid out in one table piece: each is a layout, and a
/// piece is laid out whole in one prefetch slice (a 100-column table would otherwise be
/// thousands of layouts at once).
const SAMPLE_CELLS: usize = 128;
const PIECE_CELLS: usize = 64;
const SAMPLE_ROWS: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LeafPaint {
    /// The index of the document leaf this paints, or part of.
    pub(crate) source: usize,
    pub(crate) text: String,
    pub(crate) attrs: Vec<TextAttr>,
    pub(crate) links: Vec<LinkSpan>,
    /// Set for a piece of a table, whose text is in its cells (`text` is empty).
    pub(crate) table: Option<TablePaint>,
    /// Set for code in a named language: where its text is in its block, for highlighting.
    pub(crate) code: Option<CodeSpan>,
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
    /// The images in it (its text's, or its cells'), in order: the view loads them when the
    /// paint is drawn.
    pub(crate) images: Vec<ImageId>,
    /// The `<details>` this is inside (by id), shown only while all of them are open; a
    /// summary is not inside its own.
    pub(crate) details: Vec<DetailsId>,
    /// The `<details>` whose summary this is: clicking it opens or closes them.
    pub(crate) summary_of: Option<DetailsId>,
}

impl LeafPaint {
    /// Whether anything in it is a link: its text, a table cell's, or a marker.
    pub(crate) fn has_links(&self) -> bool {
        !self.links.is_empty()
            || self.table.as_ref().is_some_and(|table| {
                table
                    .rows
                    .iter()
                    .any(|row| row.cells.iter().any(|cell| !cell.links.is_empty()))
            })
            || self
                .decorations
                .iter()
                .any(|decoration| matches!(decoration, Decoration::Marker { link: Some(_), .. }))
    }

    /// Unstyled text in a font, with no box around it.
    pub(crate) fn plain(text: &str, font: FontRole, attrs: Vec<TextAttr>) -> Self {
        Self {
            source: 0,
            text: text.to_owned(),
            attrs,
            links: Vec::new(),
            table: None,
            code: None,
            images: Vec::new(),
            font,
            align: Align::Start,
            indent_em: 0.0,
            gap_above_em: 0.0,
            gap_below_em: 0.0,
            pad_above_em: 0.0,
            pad_below_em: 0.0,
            background: None,
            decorations: Vec::new(),
            details: Vec::new(),
            summary_of: None,
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

/// A link over bytes `start..end` of a paint's (or cell's) text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) target: Target,
}

/// Where a link goes, as far as the document alone can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A leaf of this document: a heading or `<a id>`, a footnote, or back to the reference.
    Leaf(LeafId),
    /// A `#fragment` that names nothing in this document.
    Missing(String),
    /// A URL, opened by the system.
    Uri(String),
    /// A path relative to the document (or, starting with `/`, to its repository), and the
    /// fragment after it.
    Relative {
        path: String,
        fragment: Option<String>,
    },
}

impl AttrKind {
    /// The image, for an `Image` attribute.
    pub(crate) fn image(self) -> Option<ImageId> {
        match self {
            Self::Image(image) => Some(image),
            Self::Bold
            | Self::Italic
            | Self::Strikethrough
            | Self::Underline
            | Self::Code
            | Self::Link
            | Self::Subscript
            | Self::Superscript
            | Self::Highlight
            | Self::AlertTitle(_)
            | Self::Token(_) => None,
        }
    }
}

impl Target {
    /// A one-word description, for the probe's report: `leaf:3`, `uri:https://...`.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Leaf(leaf) => format!("leaf:{}", leaf.index()),
            Self::Missing(name) => format!("missing:{name}"),
            Self::Uri(uri) => format!("uri:{uri}"),
            Self::Relative { path, fragment } => match fragment {
                Some(fragment) => format!("relative:{path}#{fragment}"),
                None => format!("relative:{path}"),
            },
        }
    }
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
    /// An image, at its object character.
    Image(ImageId),
    /// An alert's title, in its colour.
    AlertTitle(AlertKind),
    /// A token of highlighted code, in its colour.
    Token(TokenClass),
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
    /// A list marker or footnote number, right-aligned to end at `x_em`; a footnote's number
    /// links back to where it is referenced.
    Marker {
        x_em: f64,
        text: String,
        link: Option<Target>,
    },
    /// A `<details>` summary's marker, right-aligned to end at `x_em`: which way it points
    /// depends on whether the details are open, which is the view's state, not the paint's.
    Disclosure { x_em: f64 },
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

/// The first paint of document leaf `leaf` (paints are in leaf order).
pub(crate) fn first_paint_of(paints: &[LeafPaint], leaf: usize) -> Option<usize> {
    let at = paints.partition_point(|paint| paint.source < leaf);
    paints
        .get(at)
        .is_some_and(|paint| paint.source == leaf)
        .then_some(at)
}

/// Each anchor's name (a heading's slug, an `<a id>`) and the leaf it names. The first of a
/// name wins, as in a browser. Looked up with `model::anchor_name`, like `Document::anchor_leaf`.
pub(crate) fn anchor_leaves(doc: &Document) -> HashMap<String, LeafId> {
    let mut anchors = HashMap::new();
    for anchor in doc.anchors() {
        anchors
            .entry(anchor.slug().to_owned())
            .or_insert(anchor.leaf());
    }
    anchors
}

/// The largest `width` or `height` attribute taken, in pixels.
const MAX_ATTRIBUTE_PX: u32 = 16_384;

/// The document's images: alt text, size attributes, and where each comes from (only http,
/// https and relative paths are ever loaded).
pub(crate) fn image_infos(doc: &Document) -> Vec<ImageInfo> {
    doc.images()
        .iter()
        .map(|image| {
            // Clamped: a width of four billion would overflow Pango's line heights.
            let clamp = |side: Option<u32>| side.map(|side| side.min(MAX_ATTRIBUTE_PX));
            let (width, height) = image.size();
            ImageInfo {
                alt: image.alt().to_owned(),
                width: clamp(width),
                height: clamp(height),
                source: image.source().and_then(|source| match source {
                    LinkTarget::Absolute(url) => Some(ImageSource::Url(url.clone())),
                    LinkTarget::Relative { path, .. } if !path.is_empty() => {
                        Some(ImageSource::Path(path.clone()))
                    }
                    LinkTarget::Relative { .. }
                    | LinkTarget::Fragment(_)
                    | LinkTarget::Footnote(_) => None,
                }),
            }
        })
        .collect()
}

/// Whether each `<details>` (by id) starts open: those written `<details open>`.
pub(crate) fn details_open(doc: &Document) -> Vec<bool> {
    let mut open = Vec::new();
    for (_id, context) in doc.contexts_with_ids() {
        if let Container::Details {
            id,
            open: starts_open,
        } = context.container()
        {
            let index = id.index();
            if open.len() <= index {
                open.resize(index + 1, false);
            }
            if let Some(slot) = open.get_mut(index) {
                *slot = *starts_open;
            }
        }
    }
    open
}

/// The paints of every leaf, in order.
pub(crate) fn paint_document(doc: &Document) -> Vec<LeafPaint> {
    let leaves: Vec<_> = doc.leaves_with_ids().collect();
    let mut painter = Painter {
        doc,
        references: first_references(doc),
        footnotes: doc
            .contexts_with_ids()
            .filter_map(|(_id, context)| match context.container() {
                Container::Footnote(footnote) => Some((*footnote, context.first_leaf()?)),
                Container::Quote
                | Container::Alert(_)
                | Container::ListItem { .. }
                | Container::Details { .. } => None,
            })
            .collect(),
        samples: HashMap::new(),
    };
    let mut paints = Vec::with_capacity(leaves.len());
    for (index, &(id, leaf)) in leaves.iter().enumerate() {
        // The containers the next leaf is in: any other ends at this leaf.
        let next: Vec<ContextId> = leaves
            .get(index + 1)
            .map(|&(_id, next)| doc.containers(next).map(|(id, _)| id).collect())
            .unwrap_or_default();
        paints.extend(split(painter.paint_leaf(index, id, leaf, &next)));
    }
    paints
}

/// The leaf of each footnote's first reference, where its number links back to.
fn first_references(doc: &Document) -> HashMap<FootnoteId, LeafId> {
    let mut references = HashMap::new();
    for (id, leaf) in doc.leaves_with_ids() {
        let mut note = |inlines: &Inlines| {
            for span in inlines.spans() {
                if let Some(LinkTarget::Footnote(footnote)) = span
                    .link()
                    .and_then(|link| doc.link(link))
                    .map(galley_core::model::Link::target)
                {
                    references.entry(*footnote).or_insert(id);
                }
            }
        };
        match leaf.kind() {
            LeafKind::Paragraph(inlines)
            | LeafKind::Heading { inlines, .. }
            | LeafKind::DetailsSummary { inlines, .. } => note(inlines),
            LeafKind::TableRows { table, rows, .. } => {
                for row in doc
                    .table(*table)
                    .and_then(|t| t.row_range(rows))
                    .unwrap_or_default()
                {
                    row.iter().for_each(&mut note);
                }
            }
            LeafKind::Code { .. } | LeafKind::ThematicBreak | LeafKind::AlertTitle(_) => {}
        }
    }
    references
}

struct Painter<'d> {
    doc: &'d Document,
    references: HashMap<FootnoteId, LeafId>,
    /// Each footnote's first leaf, where a reference to it goes.
    footnotes: HashMap<FootnoteId, LeafId>,
    /// Each table's column sample, shared by its pieces.
    samples: HashMap<TableId, Arc<ColumnSample>>,
}

/// Styled text being built: a paint's or a table cell's.
struct Styled<'a> {
    text: &'a mut String,
    attrs: &'a mut Vec<TextAttr>,
    links: &'a mut Vec<LinkSpan>,
}

impl Painter<'_> {
    fn paint_leaf(
        &mut self,
        source: usize,
        id: LeafId,
        leaf: &Leaf,
        next: &[ContextId],
    ) -> LeafPaint {
        let mut paint = LeafPaint {
            source,
            text: String::new(),
            attrs: Vec::new(),
            links: Vec::new(),
            table: None,
            code: None,
            images: Vec::new(),
            font: FontRole::Body,
            align: leaf.align(),
            indent_em: 0.0,
            gap_above_em: 0.0,
            gap_below_em: PARAGRAPH_GAP,
            pad_above_em: 0.0,
            pad_below_em: 0.0,
            background: None,
            decorations: Vec::new(),
            details: Vec::new(),
            summary_of: None,
        };
        let tight = self.containers(&mut paint, id, leaf, next);
        self.fill(&mut paint, leaf, tight);
        paint.images = images_in(&paint);
        paint
    }

    /// Fills in what the leaf holds: its text, or its table rows.
    fn fill(&mut self, paint: &mut LeafPaint, leaf: &Leaf, tight: bool) {
        let doc = self.doc;
        match leaf.kind() {
            LeafKind::Paragraph(inlines) => {
                self.push_inlines(&mut paint.styled(), inlines);
                if tight {
                    paint.gap_below_em = TIGHT_GAP;
                }
                // A formula on a line of its own (`$$...$$`) is centred, as GitHub shows it.
                if is_display_math(inlines) {
                    paint.align = Align::Center;
                }
            }
            LeafKind::Heading { level, inlines, .. } => {
                self.push_inlines(&mut paint.styled(), inlines);
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
                paint.code = code
                    .filter(|code| highlight::language(code.info()).is_some())
                    .and_then(|code| code.line_start(lines.start))
                    .map(|start| CodeSpan {
                        block: *block,
                        start: start.index(),
                    });
                let first = lines.start.index() == 0;
                let last = code.is_none_or(|code| lines.end.index() >= code.line_count());
                paint.gap_below_em = if last { PARAGRAPH_GAP } else { 0.0 };
                if code.is_some_and(|code| code.info() == "math") {
                    // GitHub shows a ```math block as a formula: centred, not boxed.
                    paint.align = Align::Center;
                } else {
                    paint.background = Some(Fill::Code);
                    paint.pad_above_em = if first { CODE_PADDING } else { 0.0 };
                    paint.pad_below_em = if last { CODE_PADDING } else { 0.0 };
                }
            }
            LeafKind::TableRows {
                table,
                rows,
                with_header,
            } => {
                let last = doc
                    .table(*table)
                    .is_none_or(|t| rows.end.index() >= t.rows().len());
                paint.gap_below_em = if last { PARAGRAPH_GAP } else { 0.0 };
                paint.table = self.table_rows(*table, rows, *with_header, last);
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
            LeafKind::DetailsSummary { details, inlines } => {
                paint.summary_of = Some(*details);
                self.push_inlines(&mut paint.styled(), inlines);
                paint.decorations.push(Decoration::Disclosure {
                    x_em: paint.indent_em - MARKER_GAP,
                });
                paint.gap_below_em = PARAGRAPH_GAP / 2.0;
            }
        }
    }

    /// Applies the leaf's containers, outermost first: indent, bars, markers. Returns whether
    /// the leaf sits directly in an item of a tight list.
    fn containers(
        &self,
        paint: &mut LeafPaint,
        id: LeafId,
        leaf: &Leaf,
        next: &[ContextId],
    ) -> bool {
        let doc = self.doc;
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
                            link: None,
                        });
                    }
                    list_depth += 1;
                }
                Container::Footnote(footnote) => {
                    paint.indent_em += LIST_INDENT;
                    if starts_here && let Some(note) = doc.footnote(*footnote) {
                        paint.decorations.push(Decoration::Marker {
                            x_em: paint.indent_em - MARKER_GAP,
                            text: format!("{}.", note.number()),
                            link: self.references.get(footnote).copied().map(Target::Leaf),
                        });
                    }
                }
                Container::Details { id: details, .. } => {
                    paint.indent_em += DETAILS_INDENT;
                    let own_summary = matches!(
                        leaf.kind(),
                        LeafKind::DetailsSummary { details: of, .. } if of == details
                    );
                    if !own_summary {
                        paint.details.push(*details);
                    }
                }
            }
        }
        tight
    }

    /// A table leaf's rows (with the header on its first), each cell's text styled and cut to
    /// `CELL_TEXT_BYTES`; body rows are striped by their place in the whole table.
    fn table_rows(
        &mut self,
        table_id: TableId,
        rows: &Range<RowNo>,
        with_header: bool,
        last: bool,
    ) -> Option<TablePaint> {
        let doc = self.doc;
        let table = doc.table(table_id)?;
        let body = table.row_range(rows)?;
        let columns = self.samples.get(&table_id).cloned().unwrap_or_else(|| {
            let count = table
                .rows()
                .iter()
                .map(Vec::len)
                .chain([table.header().len()])
                .max()
                .unwrap_or(0);
            let rows = self.sample_rows(table);
            let mut chars = vec![0; count];
            for row in &rows {
                for (cell, longest) in row.cells.iter().zip(chars.iter_mut()) {
                    let line = cell.text.lines().map(|line| line.chars().count()).max();
                    *longest = (*longest).max(line.unwrap_or(0));
                }
            }
            let images = rows
                .iter()
                .flat_map(|row| row.cells.iter())
                .flat_map(|cell| cell.attrs.iter().filter_map(|attr| attr.kind.image()))
                .collect();
            let sample = Arc::new(ColumnSample {
                table: table_id,
                count,
                rows,
                chars,
                images,
            });
            self.samples.insert(table_id, Arc::clone(&sample));
            sample
        });
        let mut painted = Vec::with_capacity(body.len() + 1);
        if with_header {
            painted.extend(self.header_row(table));
        }
        painted.extend(self.body_rows(table, rows.start.index(), body));
        Some(TablePaint {
            columns,
            rows: painted,
            last,
        })
    }

    /// The rows a table's column widths are measured from: its header and first body rows, up to
    /// `SAMPLE_ROWS` rows or `SAMPLE_BYTES` of text, each cell cut to `SAMPLE_CELL_BYTES`. A
    /// cell that long is wider than any view anyway, and the sample is measured before the first
    /// frame when a table is on the first screen (the M3 smoke run: 50 rows of 2 KiB cells gave
    /// Pango 104 KiB before the first frame).
    fn sample_rows(&self, table: &galley_core::model::Table) -> Vec<RowPaint> {
        let mut rows: Vec<RowPaint> = self.header_row(table).into_iter().collect();
        let body = table.rows();
        let body = body.get(..SAMPLE_ROWS.min(body.len())).unwrap_or_default();
        rows.extend(self.body_rows(table, 0, body));
        let (mut bytes, mut cells) = (0, 0);
        let mut kept = Vec::with_capacity(rows.len());
        for mut row in rows {
            for cell in &mut row.cells {
                cut_cell(cell, SAMPLE_CELL_BYTES);
            }
            bytes += row.cells.iter().map(|cell| cell.text.len()).sum::<usize>();
            cells += row.cells.len();
            if (bytes > SAMPLE_BYTES || cells > SAMPLE_CELLS) && !kept.is_empty() {
                break;
            }
            kept.push(row);
        }
        kept
    }

    fn header_row(&self, table: &galley_core::model::Table) -> Option<RowPaint> {
        if table.header().is_empty() {
            return None;
        }
        let mut row = self.row(table, table.header(), false);
        for cell in &mut row.cells {
            let bold = whole(&cell.text, AttrKind::Bold);
            cell.attrs.push(bold);
        }
        Some(row)
    }

    fn body_rows(
        &self,
        table: &galley_core::model::Table,
        first: usize,
        rows: &[Vec<Inlines>],
    ) -> Vec<RowPaint> {
        rows.iter()
            .enumerate()
            .map(|(offset, cells)| self.row(table, cells, (first + offset) % 2 == 1))
            .collect()
    }

    fn row(&self, table: &galley_core::model::Table, cells: &[Inlines], striped: bool) -> RowPaint {
        RowPaint {
            cells: cells
                .iter()
                .enumerate()
                .map(|(column, inlines)| {
                    let mut cell = CellPaint {
                        text: String::new(),
                        attrs: Vec::new(),
                        links: Vec::new(),
                        align: table.alignments().get(column).copied().unwrap_or_default(),
                    };
                    self.push_inlines(
                        &mut Styled {
                            text: &mut cell.text,
                            attrs: &mut cell.attrs,
                            links: &mut cell.links,
                        },
                        inlines,
                    );
                    cut_cell(&mut cell, CELL_TEXT_BYTES);
                    cell
                })
                .collect(),
            striped,
        }
    }

    /// Where a link goes.
    fn target(&self, link: LinkId) -> Option<Target> {
        let doc = self.doc;
        Some(match doc.link(link)?.target() {
            LinkTarget::Fragment(fragment) => doc
                .anchor_leaf(fragment)
                .map_or_else(|| Target::Missing(fragment.clone()), Target::Leaf),
            LinkTarget::Footnote(footnote) => Target::Leaf(*self.footnotes.get(footnote)?),
            LinkTarget::Absolute(url) => Target::Uri(url.clone()),
            LinkTarget::Relative { path, fragment, .. } => {
                if path.is_empty() {
                    // `?query#fragment` on this document: the fragment is what matters here.
                    return fragment
                        .as_deref()
                        .and_then(|fragment| doc.anchor_leaf(fragment))
                        .map(Target::Leaf);
                }
                Target::Relative {
                    path: path.clone(),
                    fragment: fragment.clone(),
                }
            }
        })
    }

    /// Appends styled text. Each image stays at its object character, marked `Image`: Pango
    /// reserves its box there (`text`), and the view draws it (or its placeholder) in it.
    fn push_inlines(&self, out: &mut Styled<'_>, inlines: &Inlines) {
        let base = out.text.len();
        out.text.push_str(inlines.text());
        for (offset, object) in inlines.objects() {
            let InlineObject::Image(image) = object;
            let start = base + offset.index();
            out.attrs.push(TextAttr {
                start,
                end: start + Inlines::OBJECT.len_utf8(),
                kind: AttrKind::Image(*image),
            });
        }
        let map = |position: usize| base + position;
        let images: Vec<usize> = inlines
            .objects()
            .iter()
            .map(|(offset, _object)| map(offset.index()))
            .collect();
        // Text attributes stop at images: a link's underline or a code span's background is
        // not drawn under a badge (the link itself still covers it, for clicking).
        let mut add = |start: usize, end: usize, kind: AttrKind| {
            for (start, end) in around(start, end, &images) {
                out.attrs.push(TextAttr { start, end, kind });
            }
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
                    add(start, end, kind);
                }
            }
            let Some(target) = span.link().and_then(|link| self.target(link)) else {
                continue;
            };
            add(start, end, AttrKind::Link);
            // Runs of one link in several styles are one link.
            match out.links.last_mut() {
                Some(last) if last.end == start && last.target == target => last.end = end,
                _ => out.links.push(LinkSpan { start, end, target }),
            }
        }
    }
}

impl LeafPaint {
    fn styled(&mut self) -> Styled<'_> {
        Styled {
            text: &mut self.text,
            attrs: &mut self.attrs,
            links: &mut self.links,
        }
    }
}

/// Whether inline content is display math and nothing else: display-math spans one after
/// another from its start to its end.
fn is_display_math(inlines: &Inlines) -> bool {
    let mut at = 0;
    for span in inlines.spans() {
        let range = span.range();
        if range.start.index() != at || !span.style().contains(Style::DISPLAY_MATH) {
            return false;
        }
        at = range.end.index();
    }
    // No spans: plain text (an empty paragraph is never painted).
    !inlines.spans().is_empty() && at == inlines.text().len()
}

/// `start..end` without the object characters at `images` (sorted byte offsets).
fn around(start: usize, end: usize, images: &[usize]) -> Vec<(usize, usize)> {
    let mut pieces = Vec::new();
    let mut from = start;
    for &image in images
        .iter()
        .filter(|&&image| (start..end).contains(&image))
    {
        if image > from {
            pieces.push((from, image));
        }
        from = image + Inlines::OBJECT.len_utf8();
    }
    if end > from {
        pieces.push((from, end));
    }
    pieces
}

/// The images a paint holds, in its text's or its cells' attributes.
fn images_in(paint: &LeafPaint) -> Vec<ImageId> {
    let cells = paint
        .table
        .iter()
        .flat_map(|table| table.rows.iter().flat_map(|row| row.cells.iter()))
        .flat_map(|cell| cell.attrs.iter());
    paint
        .attrs
        .iter()
        .chain(cells)
        .filter_map(|attr| attr.kind.image())
        .collect()
}

/// Cuts a cell's text to `max` bytes, with an ellipsis.
fn cut_cell(cell: &mut CellPaint, max: usize) {
    if cell.text.len() <= max {
        return;
    }
    let at = cell.text.floor_char_boundary(max);
    cell.text.truncate(at);
    cell.text.push('…');
    clamp_spans(&mut cell.attrs, &mut cell.links, 0, at);
}

/// Keeps the attributes and links over `from..to`, rebased to start at `from`.
fn clamp_spans(attrs: &mut Vec<TextAttr>, links: &mut Vec<LinkSpan>, from: usize, to: usize) {
    *attrs = attrs
        .iter()
        .filter(|attr| attr.start < to && attr.end > from)
        .map(|attr| TextAttr {
            start: attr.start.clamp(from, to) - from,
            end: attr.end.clamp(from, to) - from,
            kind: attr.kind,
        })
        .collect();
    *links = links
        .iter()
        .filter(|link| link.start < to && link.end > from)
        .map(|link| LinkSpan {
            start: link.start.clamp(from, to) - from,
            end: link.end.clamp(from, to) - from,
            target: link.target.clone(),
        })
        .collect();
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

/// Cuts a paint that is too much to lay out at once into pieces that each lay out quickly:
/// text at `cuts`, a table between rows (see `table_cuts`). The pieces stack into the original
/// box: gaps, padding, markers and bar ends stay with the first or last piece.
fn split(paint: LeafPaint) -> Vec<LeafPaint> {
    if let Some(table) = paint.table.as_ref() {
        let ranges = table_cuts(table);
        if ranges.len() <= 1 {
            return vec![paint];
        }
        let count = ranges.len();
        return ranges
            .into_iter()
            .enumerate()
            .map(|(index, rows)| {
                let mut piece = piece(&paint, index == 0, index + 1 == count);
                piece.table = paint.table.as_ref().map(|table| TablePaint {
                    columns: Arc::clone(&table.columns),
                    rows: table.rows.get(rows).unwrap_or_default().to_vec(),
                    last: table.last && index + 1 == count,
                });
                piece.images = images_in(&piece);
                piece
            })
            .collect();
    }
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
        let mut piece = piece(&paint, index == 0, index + 1 == count);
        paint
            .text
            .get(from..to)
            .unwrap_or_default()
            .clone_into(&mut piece.text);
        piece.code = paint.code.map(|code| CodeSpan {
            start: code.start + from,
            ..code
        });
        piece.attrs.clone_from(&paint.attrs);
        piece.links.clone_from(&paint.links);
        clamp_spans(&mut piece.attrs, &mut piece.links, from, to);
        piece.images = images_in(&piece);
        pieces.push(piece);
        from = next;
    }
    pieces
}

/// A piece of `paint` with no content yet, first and/or last of the pieces.
fn piece(paint: &LeafPaint, first: bool, last: bool) -> LeafPaint {
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
            Decoration::Marker { .. } | Decoration::Disclosure { .. } | Decoration::Rule => {
                first.then(|| decoration.clone())
            }
            Decoration::HeadingRule => last.then(|| decoration.clone()),
        })
        .collect();
    LeafPaint {
        source: paint.source,
        text: String::new(),
        attrs: Vec::new(),
        links: Vec::new(),
        table: None,
        code: None,
        images: Vec::new(),
        font: paint.font,
        align: paint.align,
        indent_em: paint.indent_em,
        gap_above_em: if first { paint.gap_above_em } else { 0.0 },
        gap_below_em: if last { paint.gap_below_em } else { 0.0 },
        pad_above_em: if first { paint.pad_above_em } else { 0.0 },
        pad_below_em: if last { paint.pad_below_em } else { 0.0 },
        background: paint.background,
        decorations,
        details: paint.details.clone(),
        summary_of: paint.summary_of,
    }
}

/// Row ranges for a table's pieces: as many rows as fit in `SPLIT_AT_LINE_BYTES` of cell text
/// and `PIECE_CELLS` cells, at least one.
fn table_cuts(table: &TablePaint) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let (mut start, mut bytes, mut cells) = (0, 0, 0);
    for (index, row) in table.rows.iter().enumerate() {
        let size: usize = row.cells.iter().map(|cell| cell.text.len()).sum();
        let count = row.cells.len();
        if index > start && (bytes + size > SPLIT_AT_LINE_BYTES || cells + count > PIECE_CELLS) {
            ranges.push(start..index);
            start = index;
            bytes = 0;
            cells = 0;
        }
        bytes += size;
        cells += count;
    }
    ranges.push(start..table.rows.len());
    ranges
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
        paint_document(&galley_core::load(markdown.as_bytes()))
    }

    fn markers(paint: &LeafPaint) -> Vec<(f64, String)> {
        paint
            .decorations
            .iter()
            .filter_map(|decoration| match decoration {
                Decoration::Marker { x_em, text, .. } => Some((*x_em, text.clone())),
                Decoration::Disclosure { x_em } => Some((*x_em, "disclosure".to_owned())),
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
                    Decoration::Marker { .. }
                    | Decoration::Disclosure { .. }
                    | Decoration::Rule
                    | Decoration::HeadingRule => None,
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
                    Decoration::Marker { .. }
                    | Decoration::Disclosure { .. }
                    | Decoration::Rule
                    | Decoration::HeadingRule => None,
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
    fn images_stay_at_their_object_character_and_are_listed() {
        let paints =
            paints("a ![logo](x.png) **bold** ![](y.png) [link](z)\n\n| ![c](c.png) |\n|---|\n");
        let paint = paints.first().expect("paragraph");
        assert_eq!(paint.text, "a \u{FFFC} bold \u{FFFC} link");
        let at = |kind: fn(AttrKind) -> bool| -> Vec<&str> {
            paint
                .attrs
                .iter()
                .filter(|attr| kind(attr.kind))
                .map(|attr| paint.text.get(attr.start..attr.end).expect("in text"))
                .collect()
        };
        assert_eq!(at(|kind| kind == AttrKind::Bold), vec!["bold"]);
        assert_eq!(
            at(|kind| matches!(kind, AttrKind::Image(_))),
            vec!["\u{FFFC}", "\u{FFFC}"]
        );
        assert_eq!(at(|kind| kind == AttrKind::Link), vec!["link"]);
        assert_eq!(paint.images.len(), 2);
        // A table piece lists its cells' images.
        let table = paints.get(1).expect("table");
        assert_eq!(table.images.len(), 1);
        assert_ne!(table.images, paint.images);
    }

    #[test]
    fn links_cover_images_for_clicking_but_draw_nothing_under_them() {
        let paints = paints("[![badge](b.svg) docs ![x](y.png)](https://x.org)\n");
        let paint = paints.first().expect("paragraph");
        let link_attrs: Vec<(usize, usize)> = paint
            .attrs
            .iter()
            .filter(|attr| attr.kind == AttrKind::Link)
            .map(|attr| (attr.start, attr.end))
            .collect();
        // "\u{FFFC} docs \u{FFFC}": the underline is only under " docs ".
        assert_eq!(link_attrs, vec![(3, 9)]);
        let spans: Vec<(usize, usize)> = paint
            .links
            .iter()
            .map(|link| (link.start, link.end))
            .collect();
        assert_eq!(spans, vec![(0, 12)]);
        assert_eq!(around(0, 12, &[0, 9]), vec![(3, 9)]);
        assert_eq!(around(0, 4, &[]), vec![(0, 4)]);
        assert_eq!(around(2, 4, &[0, 9]), vec![(2, 4)]);
        assert_eq!(around(0, 3, &[0]), Vec::<(usize, usize)>::new());
    }

    #[test]
    fn image_infos_keep_alt_size_and_loadable_sources() {
        let doc = galley_core::load(
            b"![a](x.png) <img src=\"https://h/y.svg\" width=\"30\" alt=\"b\"> ![c](#frag) ![d](data:x) ![e](?query)",
        );
        let infos = image_infos(&doc);
        let summary: Vec<(&str, Option<u32>, Option<ImageSource>)> = infos
            .iter()
            .map(|info| (info.alt.as_str(), info.width, info.source.clone()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("a", None, Some(ImageSource::Path("x.png".to_owned()))),
                (
                    "b",
                    Some(30),
                    Some(ImageSource::Url("https://h/y.svg".to_owned()))
                ),
                ("c", None, None),
                ("d", None, None),
                ("e", None, None),
            ]
        );
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
                Decoration::Marker { .. }
                | Decoration::Disclosure { .. }
                | Decoration::Rule
                | Decoration::HeadingRule => None,
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
    fn code_pieces_know_where_their_text_is_in_the_block() {
        // 450 lines of 31 bytes: three leaves in the model, each cut into 2 KiB pieces.
        let code = (0..450)
            .map(|i| format!("{i:030}\n"))
            .collect::<Vec<_>>()
            .concat();
        let doc = galley_core::load(
            format!("```rust\n{code}```\n\n```\nplain\n```\n\n```math\nx\n```\n").as_bytes(),
        );
        let paints = paint_document(&doc);
        let block = doc.code_blocks().first().expect("rust");
        let rust: Vec<&LeafPaint> = paints.iter().filter(|paint| paint.code.is_some()).collect();
        assert!(rust.len() > 3);
        for paint in &rust {
            let span = paint.code.expect("a span");
            assert_eq!(
                block.text().get(span.start..span.start + paint.text.len()),
                Some(paint.text.as_str())
            );
        }
        // Unnamed code and math are not highlighted.
        assert_eq!(rust.len() + 2, paints.len());
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

    fn cell_texts(paint: &LeafPaint) -> Vec<Vec<String>> {
        paint
            .table
            .as_ref()
            .expect("a table piece")
            .rows
            .iter()
            .map(|row| row.cells.iter().map(|cell| cell.text.clone()).collect())
            .collect()
    }

    #[test]
    fn tables_are_rows_of_cells_with_a_bold_header_and_striped_body() {
        let short = paints("| A | B |\n|:-:|--:|\n| 1 | 2 |\n| 3 | 4 |\n| 5 | 6 |\n");
        let piece = short.first().expect("table");
        assert_eq!(piece.text, "");
        assert_eq!(
            cell_texts(piece),
            vec![
                vec!["A", "B"],
                vec!["1", "2"],
                vec!["3", "4"],
                vec!["5", "6"]
            ]
        );
        let table = piece.table.as_ref().expect("table");
        let header = table.rows.first().expect("header");
        assert!(
            header
                .cells
                .iter()
                .all(|cell| cell.attrs.contains(&whole(&cell.text, AttrKind::Bold)))
        );
        let striped: Vec<bool> = table.rows.iter().map(|row| row.striped).collect();
        assert_eq!(striped, vec![false, false, true, false]);
        let aligns: Vec<Align> = header.cells.iter().map(|cell| cell.align).collect();
        assert_eq!(aligns, vec![Align::Center, Align::End]);
        assert!(table.last);
        assert_eq!(table.columns.count, 2);
        assert_eq!(table.columns.rows.len(), 4);
        assert!((piece.gap_below_em - PARAGRAPH_GAP).abs() < f64::EPSILON);
    }

    #[test]
    fn long_tables_share_one_sample_and_close_on_their_last_piece() {
        let rows = (0..60)
            .map(|i| format!("| {i} |\n"))
            .collect::<Vec<_>>()
            .concat();
        let long = paints(&format!("| N |\n|---|\n{rows}"));
        let gaps: Vec<f64> = long.iter().map(|paint| paint.gap_below_em).collect();
        assert_eq!(gaps, vec![0.0, PARAGRAPH_GAP]);
        let lasts: Vec<bool> = long
            .iter()
            .filter_map(|paint| paint.table.as_ref().map(|table| table.last))
            .collect();
        assert_eq!(lasts, vec![false, true]);
        let (first, second) = (long.first().expect("first"), long.get(1).expect("second"));
        let sample = |paint: &LeafPaint| Arc::clone(&paint.table.as_ref().expect("table").columns);
        assert!(Arc::ptr_eq(&sample(first), &sample(second)));
        // Header plus SAMPLE_ROWS body rows.
        assert_eq!(sample(first).rows.len(), 1 + SAMPLE_ROWS);
        // The second chunk starts at body row 50: even, so not striped.
        assert_eq!(cell_texts(second).first(), Some(&vec!["50".to_owned()]));
        assert!(
            !second
                .table
                .as_ref()
                .expect("table")
                .rows
                .first()
                .expect("row")
                .striped
        );
    }

    #[test]
    fn big_tables_split_between_rows_and_cells_are_capped() {
        let wide = "w".repeat(900);
        let rows = (0..5)
            .map(|_| format!("| {wide} |\n"))
            .collect::<Vec<_>>()
            .concat();
        let pieces = paints(&format!("| H |\n|---|\n{rows}"));
        let sizes: Vec<usize> = pieces.iter().map(|paint| cell_texts(paint).len()).collect();
        // Header + 2 rows fit in 2 KiB, then 2 rows, then 1.
        assert_eq!(sizes, vec![3, 2, 1]);
        let lasts: Vec<bool> = pieces
            .iter()
            .filter_map(|paint| paint.table.as_ref().map(|table| table.last))
            .collect();
        assert_eq!(lasts, vec![false, false, true]);
        let huge = paints(&format!("| H |\n|---|\n| **{}** |\n", "x".repeat(5000)));
        let cell = huge
            .iter()
            .flat_map(|paint| paint.table.iter().flat_map(|table| table.rows.iter()))
            .nth(1)
            .and_then(|row| row.cells.first())
            .expect("the big cell");
        assert_eq!(cell.text.len(), CELL_TEXT_BYTES + '…'.len_utf8());
        assert!(cell.attrs.iter().all(|attr| attr.end <= CELL_TEXT_BYTES));
    }

    #[test]
    fn details_hide_their_content_but_not_their_summary() {
        let paints = paints(
            "<details open>\n<summary>Outer</summary>\n\nouter body\n\n<details>\n<summary>Inner</summary>\n\ninner body\n\n</details>\n</details>\n\nafter\n",
        );
        let shape: Vec<(&str, Vec<usize>, Option<usize>)> = paints
            .iter()
            .map(|paint| {
                (
                    paint.text.as_str(),
                    paint.details.iter().map(|id| id.index()).collect(),
                    paint.summary_of.map(DetailsId::index),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("Outer", vec![], Some(0)),
                ("outer body", vec![0], None),
                ("Inner", vec![0], Some(1)),
                ("inner body", vec![0, 1], None),
                ("after", vec![], None),
            ]
        );
        // A summary's marker is a disclosure: the view decides which way it points.
        let summary = paints.first().expect("summary");
        assert_eq!(
            markers(summary),
            vec![(DETAILS_INDENT - MARKER_GAP, "disclosure".to_owned())]
        );
        let doc = galley_core::load(b"<details open></details><details></details>x");
        assert_eq!(details_open(&doc), vec![true, false]);
        assert_eq!(details_open(&galley_core::load(b"x")), Vec::<bool>::new());
    }

    #[test]
    fn display_math_is_centred_and_math_blocks_are_not_boxed() {
        let paints = paints(
            "$$\nx^2\n$$\n\nText $$y$$ inline.\n\nText $$y$$\n\n$$y$$ text\n\n```math\nE = mc^2\n```\n",
        );
        let aligns: Vec<Align> = paints.iter().map(|paint| paint.align).collect();
        assert_eq!(
            aligns,
            vec![
                Align::Center,
                Align::Start,
                Align::Start,
                Align::Start,
                Align::Center
            ]
        );
        let math = paints.last().expect("math block");
        assert_eq!(math.background, None);
        assert_eq!(math.font, FontRole::Code);
        assert!(math.pad_above_em.abs() < f64::EPSILON);
    }

    #[test]
    fn has_links_finds_links_in_text_cells_and_footnote_numbers() {
        let paints = paints(
            "plain\n\n[a](https://x.org)\n\n| [c](#t) |\n|---|\n\n| d |\n|---|\n\nx[^1]\n\n[^1]: note\n",
        );
        let has: Vec<bool> = paints.iter().map(LeafPaint::has_links).collect();
        // The reference paragraph links to the note; the note's number links back.
        assert_eq!(has, vec![false, true, true, false, true, true]);
    }

    #[test]
    fn first_paints_and_anchors_name_the_right_leaves() {
        let code = format!(
            "# Top\n\n```\n{}```\n\n<a id=\"x\"></a>after\n",
            "line\n".repeat(1000)
        );
        let doc = galley_core::load(code.as_bytes());
        let paints = paint_document(&doc);
        assert!(paints.len() > 3, "the code is split");
        let last_leaf = paints.last().expect("paints").source;
        assert_eq!(first_paint_of(&paints, 0), Some(0));
        assert_eq!(first_paint_of(&paints, 1), Some(1));
        assert_eq!(first_paint_of(&paints, last_leaf), Some(paints.len() - 1));
        assert_eq!(first_paint_of(&paints, last_leaf + 1), None);
        let anchors = anchor_leaves(&doc);
        assert_eq!(anchors.get("top").map(|leaf| leaf.index()), Some(0));
        assert_eq!(anchors.get("x").map(|leaf| leaf.index()), Some(last_leaf));
        assert_eq!(anchors.len(), 2);
    }

    #[test]
    fn adjacent_links_stay_apart_unless_they_are_one() {
        let paints = paints("[a](https://x.org)[b](https://y.org) [c](https://x.org)\n");
        let spans: Vec<(usize, usize)> = paints
            .first()
            .expect("paragraph")
            .links
            .iter()
            .map(|link| (link.start, link.end))
            .collect();
        assert_eq!(spans, vec![(0, 1), (1, 2), (3, 4)]);
    }

    #[test]
    fn pieces_never_start_empty_and_cut_past_the_limits_not_at_them() {
        // A first row over 2 KiB is a piece of its own, with no empty piece before it.
        let big = paints(&format!("| {} |\n|---|\n| a |\n", "h".repeat(2500)));
        let sizes: Vec<usize> = big.iter().map(|paint| cell_texts(paint).len()).collect();
        assert_eq!(sizes, vec![1, 1]);
        // Two rows of exactly 1 KiB make 2 KiB: one piece.
        let kib = "k".repeat(1024);
        let exact = paints(&format!("| {kib} |\n|---|\n| {kib} |\n"));
        assert_eq!(exact.len(), 1);
        // The 2 KiB cap on a cell is 2048 bytes, then the ellipsis.
        let cell = big
            .first()
            .map(cell_texts)
            .and_then(|rows| rows.into_iter().next());
        assert_eq!(
            cell.and_then(|row| row.into_iter().next())
                .map(|text| text.len()),
            Some(2048 + 3)
        );
    }

    #[test]
    fn samples_keep_a_row_that_brings_them_to_exactly_8_kib() {
        // A 163-byte header and rows of 256 + 3 bytes: 163 + 31 × 259 = 8192.
        let rows = (0..40)
            .map(|_| format!("| {} |\n", "w".repeat(1000)))
            .collect::<Vec<_>>()
            .concat();
        let pieces = paints(&format!("| {} |\n|---|\n{rows}", "h".repeat(163)));
        let sample = &pieces
            .first()
            .and_then(|p| p.table.as_ref())
            .expect("table")
            .columns;
        assert_eq!(sample.rows.len(), 1 + 31);
    }

    #[test]
    fn samples_stop_at_128_cells() {
        let rows = (0..60)
            .map(|i| format!("| {i} | a | b | c |\n"))
            .collect::<Vec<_>>()
            .concat();
        let pieces = paints(&format!("| H | I | J | K |\n|---|---|---|---|\n{rows}"));
        let sample = &pieces
            .first()
            .and_then(|p| p.table.as_ref())
            .expect("table")
            .columns;
        // Header and 31 rows of 4 cells: 128; the next would be 132.
        assert_eq!(sample.rows.len(), 32);
    }

    #[test]
    fn targets_have_one_word_labels() {
        let doc = galley_core::load(b"a\n\nb\n\nc\n\nd\n");
        let fourth = doc
            .leaves_with_ids()
            .nth(3)
            .map(|(id, _)| id)
            .expect("leaf 3");
        assert_eq!(Target::Leaf(fourth).label(), "leaf:3");
        assert_eq!(Target::Missing("x".to_owned()).label(), "missing:x");
        assert_eq!(Target::Uri("https://a".to_owned()).label(), "uri:https://a");
        let relative = |fragment: Option<&str>| Target::Relative {
            path: "a.md".to_owned(),
            fragment: fragment.map(str::to_owned),
        };
        assert_eq!(relative(None).label(), "relative:a.md");
        assert_eq!(relative(Some("b")).label(), "relative:a.md#b");
    }

    #[test]
    fn table_pieces_in_a_quote_keep_its_bar_ends_on_the_first_and_last() {
        let rows = (0..40)
            .map(|i| format!("> | {i} | {i} |\n"))
            .collect::<Vec<_>>()
            .concat();
        let pieces = paints(&format!("> | a | b |\n> |---|---|\n{rows}"));
        // 41 rows of 2 cells: 64 cells to a piece, so 32 rows, then 9.
        let sizes: Vec<usize> = pieces.iter().map(|paint| cell_texts(paint).len()).collect();
        assert_eq!(sizes, vec![32, 9]);
        assert_eq!(
            bar_ends(pieces.first().expect("first")),
            vec![(true, false)]
        );
        assert_eq!(bar_ends(pieces.last().expect("last")), vec![(false, true)]);
        let lasts: Vec<bool> = pieces
            .iter()
            .filter_map(|paint| paint.table.as_ref().map(|table| table.last))
            .collect();
        assert_eq!(lasts, vec![false, true]);
    }

    #[test]
    fn column_samples_are_capped_by_cell_and_in_total() {
        let wide = "w".repeat(1000);
        let rows = (0..50)
            .map(|_| format!("| {wide} | x |\n"))
            .collect::<Vec<_>>()
            .concat();
        let pieces = paints(&format!("| H | I |\n|---|---|\n{rows}"));
        let sample = &pieces
            .first()
            .and_then(|p| p.table.as_ref())
            .expect("table")
            .columns;
        let longest = sample
            .rows
            .iter()
            .flat_map(|row| row.cells.iter().map(|cell| cell.text.len()))
            .max()
            .expect("cells");
        assert_eq!(longest, SAMPLE_CELL_BYTES + '…'.len_utf8());
        let total: usize = sample
            .rows
            .iter()
            .flat_map(|row| row.cells.iter().map(|cell| cell.text.len()))
            .sum();
        assert!(total <= SAMPLE_BYTES, "{total}");
        // Header (2 bytes) and rows of 256 + 3 + 1 bytes while the total stays within 8192.
        assert_eq!(sample.rows.len(), 1 + 31);
        // The rows drawn are not cut to the sample's length.
        let drawn = pieces
            .iter()
            .flat_map(|p| p.table.iter().flat_map(|t| t.rows.iter()))
            .nth(1)
            .and_then(|row| row.cells.first())
            .expect("a body cell");
        assert_eq!(drawn.text.len(), 1000);
    }

    #[test]
    fn links_resolve_to_leaves_uris_and_paths() {
        let paints = paints(
            "# Top\n\n[up](#top) [gone](#nowhere) [web](https://x.org) [doc](docs/a.md#b) [self](?x#top)\n\nNote[^n].\n\n[^n]: The note.\n",
        );
        let text = paints.get(1).expect("links");
        let targets: Vec<(&str, String)> = text
            .links
            .iter()
            .map(|link| {
                (
                    text.text.get(link.start..link.end).unwrap_or(""),
                    link.target.label(),
                )
            })
            .collect();
        assert_eq!(
            targets,
            vec![
                ("up", "leaf:0".to_owned()),
                ("gone", "missing:nowhere".to_owned()),
                ("web", "uri:https://x.org".to_owned()),
                ("doc", "relative:docs/a.md#b".to_owned()),
                ("self", "leaf:0".to_owned()),
            ]
        );
        let reference = paints.get(2).expect("reference");
        assert_eq!(
            reference.links.first().map(|link| link.target.label()),
            Some("leaf:3".to_owned())
        );
        let note = paints.get(3).expect("footnote");
        let back = note
            .decorations
            .iter()
            .find_map(|decoration| match decoration {
                Decoration::Marker { link, .. } => link.as_ref().map(Target::label),
                Decoration::Bar { .. }
                | Decoration::Disclosure { .. }
                | Decoration::Rule
                | Decoration::HeadingRule => None,
            });
        assert_eq!(back, Some("leaf:2".to_owned()));
    }

    #[test]
    fn a_link_in_several_styles_is_one_link_and_cut_links_stay_on_both_sides() {
        let styled = paints("[plain **bold** plain](https://x.org)\n");
        let links = &styled.first().expect("paragraph").links;
        assert_eq!(links.len(), 1);
        assert_eq!(
            links.first().map(|link| (link.start, link.end)),
            Some((0, 16))
        );
        let long = format!("[{}](https://x.org)\n", "a".repeat(3000));
        let pieces = paints(&long);
        let spans: Vec<(usize, usize)> = pieces
            .iter()
            .flat_map(|paint| paint.links.iter().map(|link| (link.start, link.end)))
            .collect();
        assert_eq!(spans, vec![(0, 2048), (0, 952)]);
        // A link ending exactly at a cut is not in the next piece, nor one starting at it in
        // the one before.
        let at_cut = format!(
            "[{}](https://x.org)[{}](https://y.org)\n",
            "a".repeat(2048),
            "b".repeat(100)
        );
        let pieces = paints(&at_cut);
        let per_piece: Vec<usize> = pieces.iter().map(|paint| paint.links.len()).collect();
        assert_eq!(per_piece, vec![1, 1]);
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
            vec![(DETAILS_INDENT - MARKER_GAP, "disclosure".to_owned())]
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
