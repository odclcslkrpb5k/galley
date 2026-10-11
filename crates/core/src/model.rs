//! The document model: what the renderer draws (docs/plan.md §4.2).
//!
//! A document is a flat list of leaves. A leaf is something the renderer lays out as one unit:
//! a paragraph, a heading, up to `CODE_CHUNK_LINES` lines of a code block, up to
//! `TABLE_CHUNK_ROWS` rows of a table. Containers (quotes, alerts, list items, footnotes,
//! `<details>`) are not leaves; each leaf carries the stack of containers it sits in, so the
//! renderer can draw quote bars and list markers per leaf and never has to lay out a whole list
//! or table to show its first screen.
//!
//! Positions inside a leaf are (leaf, `ByteOffset`), which is what text selection and find will
//! use (plan §11: not in v1, but the addressing is kept).

use std::ops::Range;

use crate::units::{
    AnchorId, ByteOffset, CodeBlockId, DetailsId, FootnoteId, ImageId, LeafId, LineNo, LinkId,
    RowNo, TableId,
};

/// Most lines of a code block in one leaf.
pub const CODE_CHUNK_LINES: usize = 200;
/// Most body rows of a table in one leaf.
pub const TABLE_CHUNK_ROWS: usize = 50;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Document {
    pub leaves: Vec<Leaf>,
    pub links: Vec<Link>,
    pub images: Vec<Image>,
    pub footnotes: Vec<Footnote>,
    pub tables: Vec<Table>,
    pub code_blocks: Vec<CodeBlock>,
    pub anchors: Vec<Anchor>,
    pub notices: Vec<Notice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leaf {
    pub kind: LeafKind,
    /// The containers this leaf sits in, outermost first.
    pub containers: Vec<Container>,
    pub align: Align,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Container {
    Quote,
    Alert(AlertKind),
    ListItem(ListItem),
    /// A footnote definition; `show_number` is true on its first leaf only.
    Footnote {
        footnote: FootnoteId,
        show_number: bool,
    },
    Details(DetailsId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListItem {
    pub marker: Marker,
    /// Whether the list is tight (no blank lines between items): drawn with less spacing.
    pub tight: bool,
    /// True on the item's first leaf only: that is where the marker is drawn.
    pub show_marker: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    Bullet,
    Ordered { number: u64, delimiter: Delimiter },
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

    /// The level for a number 1-6; anything else is clamped into that range.
    #[must_use]
    pub fn clamped(level: u8) -> Self {
        match level {
            0 | 1 => Self::H1,
            2 => Self::H2,
            3 => Self::H3,
            4 => Self::H4,
            5 => Self::H5,
            6..=u8::MAX => Self::H6,
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

    /// The marker as written in the source, upper case.
    #[must_use]
    pub const fn marker(self) -> &'static str {
        match self {
            Self::Note => "[!NOTE]",
            Self::Tip => "[!TIP]",
            Self::Important => "[!IMPORTANT]",
            Self::Warning => "[!WARNING]",
            Self::Caution => "[!CAUTION]",
        }
    }
}

/// Styled text: one string, with non-overlapping runs of style and inline objects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inlines {
    pub text: String,
    /// Sorted, non-overlapping, and only where the style or link differs from plain text.
    pub spans: Vec<Span>,
    /// Images, each standing at a U+FFFC (object replacement character) in `text`.
    pub objects: Vec<(ByteOffset, InlineObject)>,
}

impl Inlines {
    /// The character that marks an inline object's place in `text`.
    pub const OBJECT: char = '\u{FFFC}';
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub range: Range<ByteOffset>,
    pub style: Style,
    pub link: Option<LinkId>,
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
    pub const MATH: Self = Self(1 << 7);
    pub const FOOTNOTE_REFERENCE: Self = Self(1 << 8);
    pub const UNDERLINE: Self = Self(1 << 9);
    pub const HIGHLIGHT: Self = Self(1 << 10);

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
    pub target: LinkTarget,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// `#section`: a heading anchor in this document. The fragment is percent-decoded.
    Fragment(String),
    /// A path relative to the document (or, starting with `/`, to the repository root).
    Relative {
        path: String,
        query: Option<String>,
        fragment: Option<String>,
    },
    /// A URL with a scheme (`https:`, `mailto:`, ...), kept as written.
    Absolute(String),
    /// A footnote reference.
    Footnote(FootnoteId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub source: LinkTarget,
    pub alt: String,
    pub title: Option<String>,
    /// Size from `width`/`height` attributes of an HTML `<img>`, in CSS pixels.
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footnote {
    pub label: String,
    /// The number shown, as GitHub numbers them: order of first reference.
    pub number: u32,
    /// The leaves of its definition.
    pub leaves: Range<LeafId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub alignments: Vec<Align>,
    pub header: Vec<Inlines>,
    pub rows: Vec<Vec<Inlines>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock {
    /// The info string after the opening fence (`rust`, `math`, ...), empty if none.
    pub info: String,
    pub text: String,
    /// Where each line starts in `text`.
    pub line_starts: Vec<ByteOffset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// The id GitHub gives the heading (`#user-content-` prefix not included).
    pub slug: String,
    pub leaf: LeafId,
}

/// Something about the document the person viewing it should know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// The file was not valid UTF-8; invalid bytes show as U+FFFD.
    InvalidUtf8Replaced,
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
    fn heading_levels_clamp() {
        assert_eq!(HeadingLevel::clamped(0), HeadingLevel::H1);
        assert_eq!(HeadingLevel::clamped(3), HeadingLevel::H3);
        assert_eq!(HeadingLevel::clamped(9), HeadingLevel::H6);
        assert_eq!(HeadingLevel::H4.number(), 4);
    }
}
