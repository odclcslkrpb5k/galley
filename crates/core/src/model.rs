//! The document model: what the renderer draws (docs/plan.md §4.2).
//!
//! A document is a flat list of leaves. A leaf is something the renderer lays out as one unit:
//! a paragraph (or part of a very long one), a heading, up to `CODE_CHUNK_LINES` lines of a code
//! block, up to `TABLE_CHUNK_ROWS` rows of a table. Containers (quotes, alerts, list items,
//! footnotes, `<details>`) are not leaves; they form a tree of contexts, and each leaf points to
//! its innermost one. The renderer draws quote bars and list markers per leaf, so it never has
//! to lay out a whole list or table to show its first screen.
//!
//! The model is read-only outside this crate: fields are private and `load` is the only way to
//! build a `Document`, so the invariants the renderer relies on (spans sorted and inside their
//! text, every id resolving) cannot be broken by hand. Enums are exhaustive on purpose: a new
//! variant must be a compile error in every `match` of the renderer.
//!
//! Positions inside a leaf are (leaf, `ByteOffset`), which is what text selection and find will
//! use (plan §11: not in v1, but the addressing is kept).

use std::ops::Range;

use crate::units::{
    AnchorId, ByteOffset, CodeBlockId, ContextId, DetailsId, FootnoteId, ImageId, LeafId, LineNo,
    LinkId, RowNo, TableId,
};

/// Most lines of a code block in one leaf.
pub const CODE_CHUNK_LINES: usize = 200;
/// Most body rows of a table in one leaf.
pub const TABLE_CHUNK_ROWS: usize = 50;
/// Most bytes of text in one leaf (paragraphs are split at a space, code chunks at a line):
/// a leaf is laid out as one unit, and Pango takes seconds over a megabyte.
pub const LEAF_TEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub(crate) leaves: Vec<Leaf>,
    pub(crate) contexts: Vec<Context>,
    pub(crate) links: Vec<Link>,
    pub(crate) images: Vec<Image>,
    pub(crate) footnotes: Vec<Footnote>,
    pub(crate) tables: Vec<Table>,
    pub(crate) code_blocks: Vec<CodeBlock>,
    pub(crate) anchors: Vec<Anchor>,
    pub(crate) notices: Vec<Notice>,
}

impl Document {
    #[must_use]
    pub fn leaves(&self) -> &[Leaf] {
        &self.leaves
    }

    /// The leaves with their ids, in document order.
    pub fn leaves_with_ids(&self) -> impl Iterator<Item = (LeafId, &Leaf)> {
        self.leaves
            .iter()
            .enumerate()
            .map(|(index, leaf)| (LeafId::new(index), leaf))
    }

    #[must_use]
    pub fn leaf(&self, id: LeafId) -> Option<&Leaf> {
        self.leaves.get(id.index())
    }

    #[must_use]
    pub fn context(&self, id: ContextId) -> Option<&Context> {
        self.contexts.get(id.index())
    }

    /// Every container, with its id, parents before children.
    pub fn contexts_with_ids(&self) -> impl Iterator<Item = (ContextId, &Context)> {
        self.contexts
            .iter()
            .enumerate()
            .map(|(index, context)| (ContextId::new(index), context))
    }

    /// The containers around a leaf, innermost first.
    pub fn containers(&self, leaf: &Leaf) -> impl Iterator<Item = (ContextId, &Context)> {
        std::iter::successors(
            leaf.context
                .and_then(|id| self.context(id).map(|context| (id, context))),
            |(_id, context)| {
                context
                    .parent
                    .and_then(|id| self.context(id).map(|parent| (id, parent)))
            },
        )
    }

    #[must_use]
    pub fn link(&self, id: LinkId) -> Option<&Link> {
        self.links.get(id.index())
    }

    #[must_use]
    pub fn links(&self) -> &[Link] {
        &self.links
    }

    #[must_use]
    pub fn image(&self, id: ImageId) -> Option<&Image> {
        self.images.get(id.index())
    }

    #[must_use]
    pub fn images(&self) -> &[Image] {
        &self.images
    }

    #[must_use]
    pub fn footnote(&self, id: FootnoteId) -> Option<&Footnote> {
        self.footnotes.get(id.index())
    }

    #[must_use]
    pub fn footnotes(&self) -> &[Footnote] {
        &self.footnotes
    }

    #[must_use]
    pub fn table(&self, id: TableId) -> Option<&Table> {
        self.tables.get(id.index())
    }

    #[must_use]
    pub fn tables(&self) -> &[Table] {
        &self.tables
    }

    #[must_use]
    pub fn code_block(&self, id: CodeBlockId) -> Option<&CodeBlock> {
        self.code_blocks.get(id.index())
    }

    #[must_use]
    pub fn code_blocks(&self) -> &[CodeBlock] {
        &self.code_blocks
    }

    #[must_use]
    pub fn anchor(&self, id: AnchorId) -> Option<&Anchor> {
        self.anchors.get(id.index())
    }

    #[must_use]
    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    /// The leaf a `#fragment` link goes to: a heading's, or a raw `<a id>`'s or `<a name>`'s.
    #[must_use]
    pub fn anchor_leaf(&self, fragment: &str) -> Option<LeafId> {
        let name = anchor_name(fragment);
        self.anchors
            .iter()
            .find(|anchor| anchor.slug == name)
            .map(|anchor| anchor.leaf)
    }

    #[must_use]
    pub fn notices(&self) -> &[Notice] {
        &self.notices
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    pub(crate) kind: LeafKind,
    /// The innermost container this leaf sits in, if any.
    pub(crate) context: Option<ContextId>,
    pub(crate) align: Align,
}

impl Leaf {
    #[must_use]
    pub fn kind(&self) -> &LeafKind {
        &self.kind
    }

    #[must_use]
    pub fn context(&self) -> Option<ContextId> {
        self.context
    }

    #[must_use]
    pub fn align(&self) -> Align {
        self.align
    }
}

/// A container, and where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub(crate) container: Container,
    pub(crate) parent: Option<ContextId>,
    /// Its first leaf: where a list marker or footnote number is drawn.
    pub(crate) first_leaf: Option<LeafId>,
}

impl Context {
    #[must_use]
    pub fn container(&self) -> &Container {
        &self.container
    }

    #[must_use]
    pub fn parent(&self) -> Option<ContextId> {
        self.parent
    }

    /// Whether `leaf` is this container's first, where its marker or number is drawn.
    #[must_use]
    pub fn starts_at(&self, leaf: LeafId) -> bool {
        self.first_leaf == Some(leaf)
    }

    /// Its first leaf: where a link to it (a footnote) goes.
    #[must_use]
    pub fn first_leaf(&self) -> Option<LeafId> {
        self.first_leaf
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafKind {
    Paragraph(Inlines),
    Heading {
        level: HeadingLevel,
        inlines: Inlines,
        anchor: AnchorId,
    },
    /// Lines `lines` of a code block.
    Code {
        block: CodeBlockId,
        lines: Range<LineNo>,
    },
    /// Body rows `rows` of a table, and its header if this is the table's first chunk.
    TableRows {
        table: TableId,
        rows: Range<RowNo>,
        with_header: bool,
    },
    ThematicBreak,
    /// The title line of an alert ("Note", "Warning", ...).
    AlertTitle(AlertKind),
    /// The always-visible line of a `<details>` element.
    DetailsSummary {
        details: DetailsId,
        inlines: Inlines,
    },
}

/// A container a leaf sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Quote,
    Alert(AlertKind),
    ListItem {
        marker: Marker,
        /// Whether the list is tight (no blank lines between items): drawn with less spacing.
        tight: bool,
    },
    Footnote(FootnoteId),
    Details {
        id: DetailsId,
        /// `<details open>`: shown expanded at first.
        open: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    Bullet,
    Ordered { number: usize, delimiter: Delimiter },
    Task { checked: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delimiter {
    Period,
    Paren,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HeadingLevel {
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
}

impl HeadingLevel {
    /// The level for a number 1-6, or `None`.
    #[must_use]
    pub const fn from_number(level: u8) -> Option<Self> {
        match level {
            1 => Some(Self::H1),
            2 => Some(Self::H2),
            3 => Some(Self::H3),
            4 => Some(Self::H4),
            5 => Some(Self::H5),
            6 => Some(Self::H6),
            _ => None,
        }
    }

    /// The level as a number, 1-6.
    #[must_use]
    pub const fn number(self) -> u8 {
        match self {
            Self::H1 => 1,
            Self::H2 => 2,
            Self::H3 => 3,
            Self::H4 => 4,
            Self::H5 => 5,
            Self::H6 => 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    /// The title GitHub shows.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
        }
    }

    /// The kind named in a `[!NOTE]` marker, any case.
    #[must_use]
    pub fn from_marker(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "note" => Some(Self::Note),
            "tip" => Some(Self::Tip),
            "important" => Some(Self::Important),
            "warning" => Some(Self::Warning),
            "caution" => Some(Self::Caution),
            _ => None,
        }
    }
}

/// Styled text: one string, with non-overlapping runs of style and inline objects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inlines {
    pub(crate) text: String,
    /// Sorted, non-overlapping, and only where the style or link differs from plain text.
    pub(crate) spans: Vec<Span>,
    /// Images, each standing at a U+FFFC (object replacement character) in `text`.
    pub(crate) objects: Vec<(ByteOffset, InlineObject)>,
}

impl Inlines {
    /// The character that marks an inline object's place in `text`.
    pub const OBJECT: char = '\u{FFFC}';

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    #[must_use]
    pub fn objects(&self) -> &[(ByteOffset, InlineObject)] {
        &self.objects
    }

    /// A byte offset into `text`, if it is on a character boundary (Pango's hit test gives
    /// byte indices).
    #[must_use]
    pub fn offset(&self, index: usize) -> Option<ByteOffset> {
        self.text
            .is_char_boundary(index)
            .then(|| ByteOffset::new(index))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub(crate) range: Range<ByteOffset>,
    pub(crate) style: Style,
    pub(crate) link: Option<LinkId>,
}

impl Span {
    #[must_use]
    pub fn range(&self) -> Range<ByteOffset> {
        self.range.clone()
    }

    #[must_use]
    pub fn style(&self) -> Style {
        self.style
    }

    #[must_use]
    pub fn link(&self) -> Option<LinkId> {
        self.link
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineObject {
    Image(ImageId),
}

/// A set of inline styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Style(u16);

impl Style {
    pub const PLAIN: Self = Self(0);
    pub const EMPHASIS: Self = Self(1);
    pub const STRONG: Self = Self(1 << 1);
    pub const STRIKETHROUGH: Self = Self(1 << 2);
    pub const CODE: Self = Self(1 << 3);
    pub const KEYBOARD: Self = Self(1 << 4);
    pub const SUBSCRIPT: Self = Self(1 << 5);
    pub const SUPERSCRIPT: Self = Self(1 << 6);
    /// Inline math (`$...$`, `` $`...`$ ``), shown as monospace.
    pub const MATH: Self = Self(1 << 7);
    /// Display math (`$$...$$`), shown as monospace on a line of its own.
    pub const DISPLAY_MATH: Self = Self(1 << 8);
    pub const FOOTNOTE_REFERENCE: Self = Self(1 << 9);
    /// `<u>` and `<ins>`.
    pub const UNDERLINE: Self = Self(1 << 10);
    /// `<mark>`.
    pub const HIGHLIGHT: Self = Self(1 << 11);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn is_plain(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub(crate) target: LinkTarget,
    pub(crate) title: Option<String>,
}

impl Link {
    #[must_use]
    pub fn target(&self) -> &LinkTarget {
        &self.target
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// `#section`: a heading or a raw `<a id>` / `<a name>` in this document. The fragment is
    /// percent-decoded.
    Fragment(String),
    /// A path relative to the document (or, starting with `/`, to the repository root).
    Relative {
        path: String,
        query: Option<String>,
        fragment: Option<String>,
    },
    /// A URL with a scheme (`https:`, `mailto:`, ...), kept as written; a protocol-relative
    /// `//host/path` is given `https:`.
    Absolute(String),
    /// A footnote reference.
    Footnote(FootnoteId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// `None` where GitHub's sanitiser drops the source (a protocol other than http or https).
    pub(crate) source: Option<LinkTarget>,
    pub(crate) alt: String,
    pub(crate) title: Option<String>,
    /// Size from `width`/`height` attributes of an HTML `<img>`, in CSS pixels.
    pub(crate) width: Option<u32>,
    pub(crate) height: Option<u32>,
}

impl Image {
    #[must_use]
    pub fn source(&self) -> Option<&LinkTarget> {
        self.source.as_ref()
    }

    #[must_use]
    pub fn alt(&self) -> &str {
        &self.alt
    }

    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    #[must_use]
    pub fn size(&self) -> (Option<u32>, Option<u32>) {
        (self.width, self.height)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footnote {
    pub(crate) label: String,
    /// The number shown, as GitHub numbers them: order of first reference.
    pub(crate) number: usize,
}

impl Footnote {
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn number(&self) -> usize {
        self.number
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub(crate) alignments: Vec<Align>,
    pub(crate) header: Vec<Inlines>,
    pub(crate) rows: Vec<Vec<Inlines>>,
}

impl Table {
    #[must_use]
    pub fn alignments(&self) -> &[Align] {
        &self.alignments
    }

    #[must_use]
    pub fn header(&self) -> &[Inlines] {
        &self.header
    }

    #[must_use]
    pub fn rows(&self) -> &[Vec<Inlines>] {
        &self.rows
    }

    /// Body rows `rows`, as a leaf names them.
    #[must_use]
    pub fn row_range(&self, rows: &Range<RowNo>) -> Option<&[Vec<Inlines>]> {
        self.rows.get(rows.start.index()..rows.end.index())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock {
    /// The info string's first word after the opening fence (`rust`, `math`, ...), or empty.
    pub(crate) info: String,
    pub(crate) text: String,
    /// Where each line starts in `text`.
    pub(crate) line_starts: Vec<ByteOffset>,
}

impl CodeBlock {
    #[must_use]
    pub fn info(&self) -> &str {
        &self.info
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// The text of lines `lines`, without the final newline.
    #[must_use]
    pub fn lines(&self, lines: &Range<LineNo>) -> Option<&str> {
        if lines.start == lines.end && lines.end.index() <= self.line_starts.len() {
            return Some("");
        }
        let start = self.line_starts.get(lines.start.index())?.index();
        let end = match self.line_starts.get(lines.end.index()) {
            Some(next) => next.index().checked_sub(1)?,
            None => self.text.len(),
        };
        self.text.get(start..end)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// The name: a heading's GitHub slug, or a raw anchor's `id`/`name` (GitHub's
    /// `user-content-` prefix not included).
    pub(crate) slug: String,
    pub(crate) leaf: LeafId,
}

impl Anchor {
    #[must_use]
    pub fn slug(&self) -> &str {
        &self.slug
    }

    #[must_use]
    pub fn leaf(&self) -> LeafId {
        self.leaf
    }
}

/// The anchor name a `#fragment` names: GitHub prefixes the ids in its pages with
/// `user-content-`, and links are written with or without it.
#[must_use]
pub fn anchor_name(fragment: &str) -> &str {
    fragment.strip_prefix("user-content-").unwrap_or(fragment)
}

/// Something about the document the person viewing it should know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// The file was not valid UTF-8; invalid bytes show as U+FFFD.
    InvalidUtf8Replaced,
    /// HTML nested deeper than the viewer follows; the deeper tags were ignored.
    HtmlTooDeep,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_combine() {
        let both = Style::EMPHASIS.union(Style::STRONG);
        assert!(both.contains(Style::EMPHASIS));
        assert!(both.contains(Style::STRONG));
        assert!(!both.contains(Style::CODE));
        assert!(!both.is_plain());
        assert!(Style::PLAIN.is_plain());
    }

    #[test]
    fn style_constants_are_distinct_single_bits() {
        let all = [
            Style::EMPHASIS,
            Style::STRONG,
            Style::STRIKETHROUGH,
            Style::CODE,
            Style::KEYBOARD,
            Style::SUBSCRIPT,
            Style::SUPERSCRIPT,
            Style::MATH,
            Style::DISPLAY_MATH,
            Style::FOOTNOTE_REFERENCE,
            Style::UNDERLINE,
            Style::HIGHLIGHT,
        ];
        for (i, a) in all.iter().enumerate() {
            assert_eq!(a.0.count_ones(), 1, "{a:?}");
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn heading_levels() {
        assert_eq!(HeadingLevel::from_number(3), Some(HeadingLevel::H3));
        assert_eq!(HeadingLevel::from_number(0), None);
        assert_eq!(HeadingLevel::from_number(7), None);
        assert_eq!(HeadingLevel::H4.number(), 4);
    }

    #[test]
    fn alert_markers() {
        assert_eq!(AlertKind::from_marker("TIP"), Some(AlertKind::Tip));
        assert_eq!(AlertKind::from_marker("note"), Some(AlertKind::Note));
        assert_eq!(AlertKind::from_marker("info"), None);
        for kind in [
            AlertKind::Note,
            AlertKind::Tip,
            AlertKind::Important,
            AlertKind::Warning,
            AlertKind::Caution,
        ] {
            assert_eq!(AlertKind::from_marker(kind.title()), Some(kind));
        }
    }

    #[test]
    fn code_block_lines() {
        let block = CodeBlock {
            info: String::new(),
            text: "a\nbb\nccc".to_owned(),
            line_starts: vec![ByteOffset::new(0), ByteOffset::new(2), ByteOffset::new(5)],
        };
        assert_eq!(
            block.lines(&(LineNo::new(0)..LineNo::new(2))),
            Some("a\nbb")
        );
        assert_eq!(block.lines(&(LineNo::new(2)..LineNo::new(3))), Some("ccc"));
        assert_eq!(block.lines(&(LineNo::new(4)..LineNo::new(5))), None);
        assert_eq!(block.lines(&(LineNo::new(3)..LineNo::new(3))), Some(""));
    }

    #[test]
    fn offsets_must_be_char_boundaries() {
        let inlines = Inlines {
            text: "é!".to_owned(),
            spans: Vec::new(),
            objects: Vec::new(),
        };
        assert_eq!(inlines.offset(0), Some(ByteOffset::new(0)));
        assert_eq!(inlines.offset(1), None);
        assert_eq!(inlines.offset(2), Some(ByteOffset::new(2)));
        assert_eq!(inlines.offset(9), None);
    }
}
