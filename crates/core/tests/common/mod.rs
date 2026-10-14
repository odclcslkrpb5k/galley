//! Fidelity oracles (docs/plan.md §9): reduce both an expected HTML rendering and our document
//! model to the same comparable views.
//!
//! - **Blocks**: each block (paragraph, heading, code block with its info string, rule, table
//!   with its row count, alert title) with its containers: quotes, alerts by kind, lists with
//!   each ordered item's number and each task item's checkbox, footnotes. A paragraph directly
//!   in a list item is `p-tight` when the list is tight (HTML: no `<p>` tag). Catches dropped or
//!   merged blocks, lost nesting, tightness, numbering and checkbox state.
//! - **Styled characters**: every visible character with its inline style (emphasis, strong,
//!   strikethrough, code, footnote reference) and the target of the link it is in. Whitespace
//!   is normalised the same way on both sides: each run becomes one unstyled space, trimmed at
//!   block edges, while hard line breaks stay distinct from spaces.
//! - **Images**: alt text and source of every image, in order.
#![cfg(test)]

use std::fmt;

use galley_core::Document;
use std::ops::Range;

use galley_core::model::{Container, Inlines, LeafKind, LinkTarget, Marker, Style};
use galley_core::units::{RowNo, TableId};
use html5gum::{Token, Tokenizer};

/// The styles the oracles compare. Others (keyboard, sub/superscript, ...) only come from raw
/// HTML, which the spec HTML keeps as tags.
const COMPARED: [Style; 5] = [
    Style::EMPHASIS,
    Style::STRONG,
    Style::STRIKETHROUGH,
    Style::CODE,
    Style::FOOTNOTE_REFERENCE,
];

/// Stands for a hard line break in the character view.
const HARD_BREAK: char = '\u{2}';
/// Stands for an image: content, so a `<br>` before one is not a block's last.
const OBJECT: char = Inlines::OBJECT;
/// Marks where a new block's characters start; removed by `normalise`.
const BOUNDARY: char = '\u{1}';

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct View {
    pub(crate) blocks: Vec<Block>,
    pub(crate) chars: Vec<StyledChar>,
    pub(crate) images: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Block {
    /// Containers, outermost first: `quote`, `alert:<kind>`, `ul`/`ol` then `li`, `li:<n>`
    /// (ordered) or `li:[x]`/`li:[ ]` (task), `footnote`, `details`.
    pub(crate) path: Vec<String>,
    pub(crate) kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StyledChar {
    pub(crate) c: char,
    /// The compared styles, as a bit per entry of `COMPARED`.
    pub(crate) styles: u8,
    /// The target of the link this character is in, normalised by `normalise_target`.
    pub(crate) link: Option<String>,
}

impl StyledChar {
    fn plain(c: char) -> Self {
        Self {
            c,
            styles: 0,
            link: None,
        }
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.path.join(">"), self.kind)
    }
}

/// The text of a view, for messages.
pub(crate) fn text(chars: &[StyledChar]) -> String {
    chars
        .iter()
        .map(|c| match c.c {
            HARD_BREAK => '⏎',
            OBJECT => '▣',
            c => c,
        })
        .collect()
}

/// The first difference between two views, described without dumping either whole.
pub(crate) fn first_difference(expected: &View, actual: &View) -> Option<String> {
    if expected.blocks != actual.blocks {
        let at = expected
            .blocks
            .iter()
            .zip(&actual.blocks)
            .position(|(e, a)| e != a)
            .unwrap_or_else(|| expected.blocks.len().min(actual.blocks.len()));
        let show = |blocks: &[Block]| {
            blocks
                .iter()
                .skip(at.saturating_sub(1))
                .take(3)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" | ")
        };
        return Some(format!(
            "blocks differ at {at} (expected {} blocks, got {}): expected [{}] got [{}]",
            expected.blocks.len(),
            actual.blocks.len(),
            show(&expected.blocks),
            show(&actual.blocks)
        ));
    }
    let expected_text = text(&expected.chars);
    let actual_text = text(&actual.chars);
    if expected_text != actual_text {
        return Some(format!(
            "text differs: expected {expected_text:?} got {actual_text:?}"
        ));
    }
    if let Some(at) = expected
        .chars
        .iter()
        .zip(&actual.chars)
        .position(|(e, a)| e != a)
    {
        let context = |chars: &[StyledChar]| {
            chars
                .iter()
                .skip(at.saturating_sub(2))
                .take(5)
                .map(|c| {
                    format!(
                        "{}:{:05b}{}",
                        c.c,
                        c.styles,
                        c.link
                            .as_deref()
                            .map_or_else(String::new, |link| format!("→{link}"))
                    )
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        return Some(format!(
            "styles or links differ at char {at}: expected [{}] got [{}]",
            context(&expected.chars),
            context(&actual.chars)
        ));
    }
    if expected.images != actual.images {
        return Some(format!(
            "images differ: expected {:?} got {:?}",
            expected.images, actual.images
        ));
    }
    None
}

fn style_bits(style: Style) -> u8 {
    COMPARED
        .iter()
        .zip(0..)
        .filter(|(compared, _bit)| style.contains(**compared))
        .fold(0, |bits, (_compared, bit)| bits | 1 << bit)
}

/// A link or image target as a comparable string: percent-decoded, `#fragment` for in-page
/// targets.
fn normalise_target(href: &str) -> String {
    let bytes = href.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'%'
            && let [high, low, tail @ ..] = after
            && let (Some(high), Some(low)) = (hex(*high), hex(*low))
        {
            out.push(high << 4 | low);
            rest = tail;
            continue;
        }
        out.push(byte);
        rest = after;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

fn model_target(doc: &Document, target: &LinkTarget) -> String {
    match target {
        LinkTarget::Fragment(fragment) => format!("#{fragment}"),
        LinkTarget::Absolute(url) => normalise_target(url),
        LinkTarget::Relative {
            path,
            query,
            fragment,
        } => {
            let mut out = path.clone();
            if let Some(query) = query {
                out.push('?');
                out.push_str(&normalise_target(query));
            }
            if let Some(fragment) = fragment {
                out.push('#');
                out.push_str(fragment);
            }
            out
        }
        LinkTarget::Footnote(id) => {
            let label = doc.footnote(*id).map_or("", |footnote| footnote.label());
            format!("#fn-{label}")
        }
    }
}

// The model side.

pub(crate) fn model_view(doc: &Document) -> View {
    let mut view = View {
        blocks: Vec::new(),
        chars: Vec::new(),
        images: Vec::new(),
    };
    let mut previous_code = None;
    let mut seen = 0;
    for leaf in doc.leaves() {
        let path = model_path(doc, leaf);
        if !matches!(leaf.kind(), LeafKind::Code { .. }) {
            previous_code = None;
        }
        match leaf.kind() {
            LeafKind::Paragraph(inlines) => {
                if inlines.text().is_empty() {
                    continue; // an empty paragraph shows nothing; HTML's are dropped too
                }
                let tight = doc.containers(leaf).next().is_some_and(|(_id, context)| {
                    matches!(context.container(), Container::ListItem { tight: true, .. })
                });
                let kind = if tight { "p-tight" } else { "p" };
                view.blocks.push(Block {
                    path,
                    kind: kind.to_owned(),
                });
                push_inlines(&mut view, &mut seen, doc, inlines);
            }
            LeafKind::Heading { level, inlines, .. } => {
                view.blocks.push(Block {
                    path,
                    kind: format!("h{}", level.number()),
                });
                push_inlines(&mut view, &mut seen, doc, inlines);
            }
            LeafKind::Code { block, lines } => {
                let code = doc.code_block(*block).expect("code block exists");
                // Consecutive chunks of one block are one <pre>.
                if previous_code == Some(*block) {
                    push_char(&mut view, &mut seen, StyledChar::plain(' '));
                } else {
                    let kind = if code.info().is_empty() {
                        "pre".to_owned()
                    } else {
                        format!("pre:{}", code.info())
                    };
                    view.blocks.push(Block { path, kind });
                }
                previous_code = Some(*block);
                push_code(&mut view, &mut seen, code.lines(lines).expect("line range"));
            }
            LeafKind::TableRows {
                table,
                rows,
                with_header,
            } => push_table_rows(
                &mut view,
                &mut seen,
                doc,
                (*table, rows, *with_header),
                path,
            ),
            LeafKind::ThematicBreak => {
                view.blocks.push(Block {
                    path,
                    kind: "hr".to_owned(),
                });
            }
            LeafKind::AlertTitle(kind) => {
                view.blocks.push(Block {
                    path,
                    kind: "alert-title".to_owned(),
                });
                for c in kind.title().chars() {
                    push_char(&mut view, &mut seen, StyledChar::plain(c));
                }
            }
            LeafKind::DetailsSummary { inlines, .. } => {
                view.blocks.push(Block {
                    path,
                    kind: "summary".to_owned(),
                });
                push_inlines(&mut view, &mut seen, doc, inlines);
            }
        }
    }
    view.chars = normalise(view.chars);
    view
}

/// A table leaf: its block (with the table's row count) if it has the header, then its cells.
fn push_table_rows(
    view: &mut View,
    seen: &mut usize,
    doc: &Document,
    (table, rows, with_header): (TableId, &Range<RowNo>, bool),
    path: Vec<String>,
) {
    let table = doc.table(table).expect("table exists");
    if with_header {
        view.blocks.push(Block {
            path,
            kind: format!(
                "table{}",
                table.rows().len() + usize::from(!table.header().is_empty())
            ),
        });
        for cell in table.header() {
            push_char(view, seen, StyledChar::plain(' '));
            push_inlines(view, seen, doc, cell);
        }
    }
    for row in table.row_range(rows).expect("rows") {
        for cell in row {
            push_char(view, seen, StyledChar::plain(' '));
            push_inlines(view, seen, doc, cell);
        }
    }
}

fn model_path(doc: &Document, leaf: &galley_core::model::Leaf) -> Vec<String> {
    let mut path = Vec::new();
    for (_id, context) in doc.containers(leaf) {
        // Innermost first; reversed at the end.
        match context.container() {
            Container::Quote => path.push("quote".to_owned()),
            Container::Alert(kind) => {
                path.push(format!("alert:{}", kind.title().to_ascii_lowercase()));
            }
            Container::ListItem { marker, .. } => {
                let (list, item) = match marker {
                    Marker::Bullet => ("ul", "li".to_owned()),
                    Marker::Task { checked } => {
                        ("ul", format!("li:[{}]", if *checked { "x" } else { " " }))
                    }
                    Marker::Ordered { number, .. } => ("ol", format!("li:{number}")),
                };
                path.push(item);
                path.push(list.to_owned());
            }
            Container::Footnote(_) => path.push("footnote".to_owned()),
            Container::Details { .. } => path.push("details".to_owned()),
        }
    }
    path.reverse();
    path
}

/// Appends one character, marking a block boundary first if a block was recorded since the
/// last character. Whitespace becomes an unstyled space.
fn push_char(view: &mut View, seen_blocks: &mut usize, c: StyledChar) {
    if *seen_blocks != view.blocks.len() {
        view.chars.push(StyledChar::plain(BOUNDARY));
        *seen_blocks = view.blocks.len();
    }
    if c.c.is_whitespace() {
        view.chars.push(StyledChar::plain(' '));
    } else if c.c == HARD_BREAK {
        view.chars.push(StyledChar::plain(HARD_BREAK));
    } else {
        view.chars.push(c);
    }
}

/// Collapses each whitespace run to one space, trims at block edges and around hard breaks.
fn normalise(chars: Vec<StyledChar>) -> Vec<StyledChar> {
    let mut out: Vec<StyledChar> = Vec::with_capacity(chars.len());
    let mut space = false;
    let mut at_start = true;
    for c in chars {
        match c.c {
            BOUNDARY => {
                space = false;
                at_start = true;
            }
            ' ' => space = !at_start,
            HARD_BREAK => {
                out.push(c);
                space = false;
                at_start = true;
            }
            _ => {
                if space {
                    out.push(StyledChar::plain(' '));
                }
                out.push(c);
                space = false;
                at_start = false;
            }
        }
    }
    out
}

fn push_inlines(view: &mut View, seen: &mut usize, doc: &Document, inlines: &Inlines) {
    for (at, c) in inlines.text().char_indices() {
        let span = inlines
            .spans()
            .iter()
            .find(|span| span.range().start.index() <= at && at < span.range().end.index());
        let link = span
            .and_then(galley_core::model::Span::link)
            .and_then(|id| doc.link(id))
            .map(|link| model_target(doc, link.target()));
        let c = if c == '\n' { HARD_BREAK } else { c };
        push_char(
            view,
            seen,
            StyledChar {
                c,
                styles: span.map_or(0, |span| style_bits(span.style())),
                link,
            },
        );
    }
    for (_at, object) in inlines.objects() {
        let galley_core::model::InlineObject::Image(id) = object;
        if let Some(image) = doc.image(*id) {
            view.images.push((
                image.alt().to_owned(),
                image
                    .source()
                    .map_or_else(String::new, |source| model_target(doc, source)),
            ));
        }
    }
}

/// Code text: newlines are layout, compared as spaces.
fn push_code(view: &mut View, seen: &mut usize, text: &str) {
    for c in text.chars() {
        push_char(view, seen, StyledChar::plain(c));
    }
}

// The HTML side.

/// Elements whose text belongs to no compared block (footnote back-references) or is never
/// shown.
fn skips_content(name: &str, attributes: &[(String, String)]) -> bool {
    has_class(attributes, "footnote-backref")
        // Text for screen readers only (GitHub's "Loading" on a diagram being drawn).
        || has_class(attributes, "sr-only")
        || attributes
            .iter()
            .any(|(name, _)| name == "data-footnote-backref")
        || matches!(name, "template")
}

fn has_class(attributes: &[(String, String)], class: &str) -> bool {
    attributes
        .iter()
        .any(|(name, value)| name == "class" && value.split_whitespace().any(|c| c == class))
}

fn attribute<'a>(attributes: &'a [(String, String)], wanted: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(name, _)| name == wanted)
        .map(|(_, value)| value.as_str())
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "br" | "img"
            | "input"
            | "hr"
            | "meta"
            | "link"
            | "area"
            | "base"
            | "col"
            | "embed"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Tags that end the text before them, as the builder's block tags and headings do.
fn is_block_level(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "center"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "aside"
            | "figure"
            | "figcaption"
            | "blockquote"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "pre"
            | "address"
            | "picture"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "hr"
            | "details"
            | "summary"
    )
}

/// What happens to an element's text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Content {
    Shown,
    /// Never shown (see `skips_content`).
    Skipped,
    /// GitHub's `<math-renderer>`: its text keeps the `$` delimiters, which the model drops.
    Math,
}

struct Open {
    name: String,
    /// Container this element contributes to the path, if any.
    container: Option<String>,
    style: Option<Style>,
    link: Option<String>,
    /// Whether it opens a block whose text is recorded (p, h1-h6, pre, alert title, cells).
    block: bool,
    /// The `<ol>` of a footnote section, whose items are footnotes, not list items.
    footnote_list: bool,
    content: Content,
    /// For a recorded <p>: the block count and text bytes seen when it opened, to drop it on
    /// close if it showed nothing (a paragraph of only HTML tags).
    content_at: Option<(usize, usize)>,
    /// For an <ol>: the number of its next item.
    next_number: usize,
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per HTML element kind; splitting would scatter the mapping"
)]
pub(crate) fn html_view(html: &str) -> View {
    let mut view = View {
        blocks: Vec::new(),
        chars: Vec::new(),
        images: Vec::new(),
    };
    let mut open: Vec<Open> = Vec::new();
    // Whether the last block is text outside any block element (in a tight list item, or in raw
    // block HTML such as <div>) that further text continues. Block-level tags end it, as they
    // end the builder's paragraph.
    let mut implicit_open = false;
    // Bytes of text seen, whitespace included: a paragraph of only spaces still shows a line.
    let mut text_seen = 0_usize;
    let mut images = 0_usize;
    let mut images_at_open: Vec<usize> = Vec::new();
    let mut seen = 0;
    // `view.chars.len()` when a block last ended, so one ending block drops at most one break.
    let mut ended_at = None;
    for token in Tokenizer::new(html) {
        let Ok(token) = token;
        match token {
            Token::StartTag(tag) => {
                let name = String::from_utf8_lossy(&tag.name).to_ascii_lowercase();
                if is_block_level(&name) {
                    implicit_open = false;
                    end_line_box(&mut view, &mut ended_at);
                }
                let attributes: Vec<(String, String)> = tag
                    .attributes
                    .iter()
                    .map(|(n, v)| {
                        (
                            String::from_utf8_lossy(n).to_ascii_lowercase(),
                            String::from_utf8_lossy(v).into_owned(),
                        )
                    })
                    .collect();
                let in_pre = open.iter().any(|o| o.name == "pre");
                let in_footnotes = open
                    .iter()
                    .any(|o| o.container.as_deref() == Some("footnote"));
                let parent_is_footnote_list = open.last().is_some_and(|o| o.footnote_list);
                let path = html_path(&open);
                let mut cell_started = false;
                let mut element = Open {
                    name: name.clone(),
                    container: None,
                    style: None,
                    link: None,
                    block: false,
                    footnote_list: false,
                    content: if skips_content(&name, &attributes) {
                        Content::Skipped
                    } else if name == "math-renderer" {
                        Content::Math
                    } else {
                        Content::Shown
                    },
                    content_at: None,
                    next_number: 1,
                };
                match name.as_str() {
                    "blockquote" => element.container = Some("quote".to_owned()),
                    "ol" if in_footnotes && !open.iter().any(|o| o.name == "li") => {
                        element.footnote_list = true;
                    }
                    "ul" => element.container = Some("ul".to_owned()),
                    "ol" => {
                        element.container = Some("ol".to_owned());
                        element.next_number = attribute(&attributes, "start")
                            .and_then(|start| start.parse().ok())
                            .unwrap_or(1);
                    }
                    "li" if !parent_is_footnote_list => {
                        let label = match open.last_mut() {
                            Some(list) if list.name == "ol" => {
                                let number = list.next_number;
                                list.next_number += 1;
                                format!("li:{number}")
                            }
                            _ => "li".to_owned(),
                        };
                        element.container = Some(label);
                    }
                    "input" if attribute(&attributes, "type") == Some("checkbox") => {
                        // A task item's checkbox: label its list item.
                        let checked = attribute(&attributes, "checked").is_some();
                        if let Some(item) = open
                            .iter_mut()
                            .rev()
                            .find(|o| o.name == "li" && o.container.is_some())
                        {
                            item.container =
                                Some(format!("li:[{}]", if checked { "x" } else { " " }));
                        }
                    }
                    "section" if has_class(&attributes, "footnotes") => {
                        element.container = Some("footnote".to_owned());
                    }
                    "div" if has_class(&attributes, "markdown-alert") => {
                        let kind = attribute(&attributes, "class")
                            .and_then(|classes| {
                                classes
                                    .split_whitespace()
                                    .find_map(|c| c.strip_prefix("markdown-alert-"))
                            })
                            .unwrap_or("?");
                        element.container = Some(format!("alert:{kind}"));
                    }
                    // GitHub wraps an uploaded video in a `details-reset` box whose summary is
                    // the file name; the viewer shows the video as a "▶ video" link.
                    "details" if has_class(&attributes, "details-reset") => {}
                    "summary"
                        if open
                            .last()
                            .is_some_and(|o| o.name == "details" && o.container.is_none()) =>
                    {
                        element.content = Content::Skipped;
                    }
                    "video" | "audio" => {
                        start_implicit_paragraph(&mut view, &open, &mut implicit_open);
                        let link = attribute(&attributes, "src").map(normalise_target);
                        let label = if name == "video" {
                            "▶ video"
                        } else {
                            "▶ audio"
                        };
                        for c in label.chars() {
                            let character = StyledChar {
                                c,
                                styles: 0,
                                link: link.clone(),
                            };
                            push_char(&mut view, &mut seen, character);
                        }
                        element.content = Content::Skipped;
                    }
                    "details" => element.container = Some("details".to_owned()),
                    "summary" => {
                        view.blocks.push(Block {
                            path: path.clone(),
                            kind: "summary".to_owned(),
                        });
                        element.block = true;
                    }
                    "p" if has_class(&attributes, "markdown-alert-title") => {
                        view.blocks.push(Block {
                            path: path.clone(),
                            kind: "alert-title".to_owned(),
                        });
                        element.block = true;
                    }
                    "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre" => {
                        view.blocks.push(Block {
                            path: path.clone(),
                            kind: name.clone(),
                        });
                        element.block = true;
                        element.content_at = Some((view.blocks.len(), text_seen));
                    }
                    "code" if in_pre => {
                        // <pre><code class="language-x">: the info string.
                        if let Some(language) =
                            attribute(&attributes, "class").and_then(|classes| {
                                classes
                                    .split_whitespace()
                                    .find_map(|c| c.strip_prefix("language-"))
                            })
                            && let Some(block) = view.blocks.last_mut()
                            && block.kind == "pre"
                        {
                            block.kind = format!("pre:{language}");
                        }
                    }
                    // Cell text belongs to the table, not to a paragraph of its own; cells are
                    // separated like words.
                    "td" | "th" => {
                        element.block = true;
                        cell_started = true;
                    }
                    "hr" => view.blocks.push(Block {
                        path: path.clone(),
                        kind: "hr".to_owned(),
                    }),
                    "table" => view.blocks.push(Block {
                        path: path.clone(),
                        kind: "table0".to_owned(),
                    }),
                    "tr" => {
                        if let Some(table) = view
                            .blocks
                            .iter_mut()
                            .rev()
                            .find(|b| b.kind.starts_with("table"))
                        {
                            let rows: usize =
                                table.kind.trim_start_matches("table").parse().unwrap_or(0);
                            table.kind = format!("table{}", rows + 1);
                        }
                    }
                    "br" => {
                        push_char(&mut view, &mut seen, StyledChar::plain(HARD_BREAK));
                    }
                    "img" => {
                        images += 1;
                        start_implicit_paragraph(&mut view, &open, &mut implicit_open);
                        let character = StyledChar {
                            c: OBJECT,
                            styles: style_bits(open_styles(&open)),
                            link: open.iter().rev().find_map(|o| o.link.clone()),
                        };
                        push_char(&mut view, &mut seen, character);
                        view.images.push((
                            attribute(&attributes, "alt").unwrap_or("").to_owned(),
                            normalise_target(
                                attribute(&attributes, "data-canonical-src")
                                    .or_else(|| attribute(&attributes, "src"))
                                    .unwrap_or(""),
                            ),
                        ));
                    }
                    "em" | "i" => element.style = Some(Style::EMPHASIS),
                    "strong" | "b" => element.style = Some(Style::STRONG),
                    "del" | "s" | "strike" => element.style = Some(Style::STRIKETHROUGH),
                    "code" => element.style = Some(Style::CODE),
                    "sup" if has_class(&attributes, "footnote-ref") => {
                        element.style = Some(Style::FOOTNOTE_REFERENCE);
                    }
                    // GitHub wraps each image in a `target="_blank"` link to the image itself;
                    // links written in the Markdown never carry a target (GitHub strips it).
                    "a" if attribute(&attributes, "target").is_some() => {}
                    // GitHub's footnote references: `#user-content-fn-<label>-<document hash>`.
                    "a" if attribute(&attributes, "data-footnote-ref").is_some() => {
                        element.style = Some(Style::FOOTNOTE_REFERENCE);
                        element.link = attribute(&attributes, "href").map(|href| {
                            let href = href.replacen("#user-content-", "#", 1);
                            let label = href
                                .rsplit_once('-')
                                .filter(|(_label, hash)| {
                                    hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit())
                                })
                                .map_or(href.as_str(), |(label, _hash)| label);
                            normalise_target(label)
                        });
                    }
                    "a" => element.link = attribute(&attributes, "href").map(normalise_target),
                    _ => {}
                }
                if cell_started {
                    push_char(&mut view, &mut seen, StyledChar::plain(' '));
                }
                if !is_void(&name) && !tag.self_closing {
                    images_at_open.push(images);
                    open.push(element);
                }
            }
            Token::EndTag(tag) => {
                let name = String::from_utf8_lossy(&tag.name).to_ascii_lowercase();
                if is_block_level(&name) {
                    implicit_open = false;
                    end_line_box(&mut view, &mut ended_at);
                }
                if let Some(at) = open.iter().rposition(|o| o.name == name) {
                    let element = open.get(at).expect("position is in range");
                    let images_before = images_at_open.get(at).copied().unwrap_or(images);
                    if let Some((blocks, text)) = element.content_at
                        && element.name == "p"
                        && text_seen == text
                        && images == images_before
                        && view.blocks.len() == blocks
                    {
                        view.blocks.pop();
                    }
                    open.truncate(at);
                    images_at_open.truncate(at);
                }
            }
            Token::String(text) => {
                if open.iter().any(|o| o.content == Content::Skipped) {
                    continue;
                }
                let mut text = String::from_utf8_lossy(&text).into_owned();
                if open.iter().any(|o| o.content == Content::Math) {
                    // Exactly one pair: a `$` inside the formula is the formula's.
                    let trimmed = text.trim();
                    text = trimmed
                        .strip_prefix("$$")
                        .and_then(|inner| inner.strip_suffix("$$"))
                        .or_else(|| trimmed.strip_prefix('$')?.strip_suffix('$'))
                        .unwrap_or(trimmed)
                        .to_owned();
                }
                text_seen += text.len();
                if text.chars().all(char::is_whitespace) {
                    // Whitespace between elements separates words but starts no block.
                    push_char(&mut view, &mut seen, StyledChar::plain(' '));
                    continue;
                }
                start_implicit_paragraph(&mut view, &open, &mut implicit_open);
                let styles = open_styles(&open);
                let link = open.iter().rev().find_map(|o| o.link.clone());
                for c in text.chars() {
                    let character = StyledChar {
                        c,
                        styles: style_bits(styles),
                        link: link.clone(),
                    };
                    push_char(&mut view, &mut seen, character);
                }
            }
            Token::Comment(_) | Token::Doctype(_) | Token::Error(_) => {}
        }
    }
    view.chars = normalise(view.chars);
    view
}

/// A block ends: a `<br>` that was its last content shows nothing (the line it ends is the
/// block's last), so it is dropped, with the whitespace after it. `ended_at` is where content
/// ended when a block last ended: blocks ending one after another drop one break between them.
fn end_line_box(view: &mut View, ended_at: &mut Option<usize>) {
    let content_end = |chars: &[StyledChar]| {
        chars
            .iter()
            .rposition(|c| c.c != ' ')
            .map_or(0, |at| at + 1)
    };
    let content = content_end(&view.chars);
    if *ended_at == Some(content) {
        return;
    }
    if content > 0
        && view
            .chars
            .get(content - 1)
            .is_some_and(|c| c.c == HARD_BREAK)
    {
        view.chars.truncate(content - 1);
    }
    *ended_at = Some(content_end(&view.chars));
}

/// Content outside any block element is a paragraph of its own: a tight list item's, or raw
/// block HTML's, until a block-level tag ends it.
fn start_implicit_paragraph(view: &mut View, open: &[Open], implicit_open: &mut bool) {
    let in_block = open.iter().any(|o| o.block);
    if !in_block && !*implicit_open {
        let path = html_path(open);
        let tight = path.last().is_some_and(|last| last.starts_with("li"));
        view.blocks.push(Block {
            path,
            kind: if tight { "p-tight" } else { "p" }.to_owned(),
        });
        *implicit_open = true;
    }
}

fn open_styles(open: &[Open]) -> Style {
    open.iter()
        .filter_map(|o| o.style)
        .fold(Style::PLAIN, Style::union)
}

fn html_path(open: &[Open]) -> Vec<String> {
    open.iter().filter_map(|o| o.container.clone()).collect()
}
