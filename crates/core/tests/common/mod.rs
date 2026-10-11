//! Fidelity oracles (docs/plan.md §9): reduce both an expected HTML rendering and our document
//! model to the same comparable views.
//!
//! - **Blocks**: each block-level thing (paragraph, heading, code block, rule, table, alert
//!   title) with the containers around it (quotes, lists, alerts, footnotes). A paragraph
//!   directly in a list item is `p-tight` when the list is tight (HTML: no `<p>` tag).
//!   Catches dropped or merged blocks, lost nesting, lost tightness.
//! - **Styled characters**: every visible character with its inline style (emphasis, strong,
//!   strikethrough, code, footnote reference) and whether it is in a link. Catches lost or
//!   misplaced text and styles. Whitespace is normalised the same way on both sides: each run
//!   becomes one unstyled space, trimmed at block edges, so words that run together are caught
//!   while layout differences (indentation, soft breaks) are not.
#![cfg(test)]

use std::fmt;

use gbk_markdown_core::Document;
use gbk_markdown_core::model::{Container, Inlines, LeafKind, Marker, Style};
use html5gum::{Token, Tokenizer};

/// The styles the oracles compare. Others (keyboard, sub/superscript, ...) only come from raw
/// HTML, which comrak passes through and the spec HTML keeps as tags we do not interpret.
const COMPARED: [Style; 5] = [
    Style::EMPHASIS,
    Style::STRONG,
    Style::STRIKETHROUGH,
    Style::CODE,
    Style::FOOTNOTE_REFERENCE,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct View {
    pub(crate) blocks: Vec<Block>,
    pub(crate) chars: Vec<StyledChar>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Block {
    /// Containers, outermost first: `quote`, `ul`/`ol` then `li`, `alert`, `footnote`,
    /// `details`.
    pub(crate) path: Vec<&'static str>,
    pub(crate) kind: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StyledChar {
    pub(crate) c: char,
    /// The compared styles, as a bit per entry of `COMPARED`.
    pub(crate) styles: u8,
    pub(crate) link: bool,
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.path.join(">"), self.kind)
    }
}

/// The text of a view, for messages.
pub(crate) fn text(chars: &[StyledChar]) -> String {
    chars.iter().map(|c| c.c).collect()
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
    let at = expected
        .chars
        .iter()
        .zip(&actual.chars)
        .position(|(e, a)| e != a)?;
    let context = |chars: &[StyledChar]| {
        chars
            .iter()
            .skip(at.saturating_sub(3))
            .take(7)
            .map(|c| format!("{}:{:05b}{}", c.c, c.styles, if c.link { "L" } else { "" }))
            .collect::<Vec<_>>()
            .join(" ")
    };
    Some(format!(
        "styles differ at char {at}: expected [{}] got [{}]",
        context(&expected.chars),
        context(&actual.chars)
    ))
}

fn style_bits(style: Style) -> u8 {
    COMPARED
        .iter()
        .zip(0..)
        .filter(|(compared, _bit)| style.contains(**compared))
        .fold(0, |bits, (_compared, bit)| bits | 1 << bit)
}

// The model side.

#[expect(
    clippy::too_many_lines,
    reason = "one arm per leaf kind; splitting would scatter the mapping"
)]
pub(crate) fn model_view(doc: &Document) -> View {
    let mut view = View {
        blocks: Vec::new(),
        chars: Vec::new(),
    };
    let mut previous_code = None;
    let mut seen = 0;
    for leaf in &doc.leaves {
        let path = model_path(&leaf.containers);
        match &leaf.kind {
            LeafKind::Paragraph(inlines) => {
                previous_code = None;
                if inlines.text.is_empty() {
                    continue; // an empty paragraph shows nothing; HTML's are dropped too
                }
                let tight = matches!(
                    leaf.containers.last(),
                    Some(Container::ListItem(item)) if item.tight
                );
                let kind = if tight { "p-tight" } else { "p" };
                view.blocks.push(Block {
                    path,
                    kind: kind.to_owned(),
                });
                push_inlines(&mut view, &mut seen, inlines);
            }
            LeafKind::Heading { level, inlines, .. } => {
                previous_code = None;
                let level = level.number();
                view.blocks.push(Block {
                    path,
                    kind: format!("h{level}"),
                });
                push_inlines(&mut view, &mut seen, inlines);
            }
            LeafKind::Code { block, lines } => {
                // Consecutive chunks of one block are one <pre>.
                if previous_code != Some(*block) {
                    view.blocks.push(Block {
                        path,
                        kind: "pre".to_owned(),
                    });
                }
                previous_code = Some(*block);
                let code = doc
                    .code_blocks
                    .get(block.index())
                    .expect("code block exists");
                let start = code
                    .line_starts
                    .get(lines.start.index())
                    .map_or(code.text.len(), |at| at.index());
                let end = code
                    .line_starts
                    .get(lines.end.index())
                    .map_or(code.text.len(), |at| at.index());
                push_plain(
                    &mut view,
                    &mut seen,
                    code.text.get(start..end).expect("line range"),
                );
            }
            LeafKind::TableRows {
                table,
                rows,
                with_header,
            } => {
                previous_code = None;
                let table = doc.tables.get(table.index()).expect("table exists");
                if *with_header {
                    view.blocks.push(Block {
                        path,
                        kind: format!("table{}", table.rows.len() + 1),
                    });
                    for cell in &table.header {
                        push_char(&mut view, &mut seen, SPACE);
                        push_inlines(&mut view, &mut seen, cell);
                    }
                }
                for row in table
                    .rows
                    .get(rows.start.index()..rows.end.index())
                    .expect("rows")
                {
                    for cell in row {
                        push_char(&mut view, &mut seen, SPACE);
                        push_inlines(&mut view, &mut seen, cell);
                    }
                }
            }
            LeafKind::ThematicBreak => {
                previous_code = None;
                view.blocks.push(Block {
                    path,
                    kind: "hr".to_owned(),
                });
            }
            LeafKind::AlertTitle(kind) => {
                previous_code = None;
                view.blocks.push(Block {
                    path,
                    kind: "alert-title".to_owned(),
                });
                push_plain(&mut view, &mut seen, kind.title());
            }
            LeafKind::DetailsSummary { inlines, .. } => {
                previous_code = None;
                view.blocks.push(Block {
                    path,
                    kind: "summary".to_owned(),
                });
                push_inlines(&mut view, &mut seen, inlines);
            }
        }
    }
    view.chars = normalise(view.chars);
    view
}

fn model_path(containers: &[Container]) -> Vec<&'static str> {
    let mut path = Vec::new();
    for container in containers {
        match container {
            Container::Quote => path.push("quote"),
            Container::Alert(_) => path.push("alert"),
            Container::ListItem(item) => {
                path.push(match item.marker {
                    Marker::Ordered { .. } => "ol",
                    Marker::Bullet | Marker::Task { .. } => "ul",
                });
                path.push("li");
            }
            Container::Footnote { .. } => path.push("footnote"),
            Container::Details(_) => path.push("details"),
        }
    }
    path
}

const SPACE: StyledChar = StyledChar {
    c: ' ',
    styles: 0,
    link: false,
};
/// Marks where a new block's characters start; removed by `normalise`.
const BOUNDARY: StyledChar = StyledChar {
    c: '\u{1}',
    styles: 0,
    link: false,
};

/// Appends one character, marking a block boundary first if a block was recorded since the
/// last character. Whitespace becomes an unstyled space.
fn push_char(view: &mut View, seen_blocks: &mut usize, c: StyledChar) {
    if *seen_blocks != view.blocks.len() {
        view.chars.push(BOUNDARY);
        *seen_blocks = view.blocks.len();
    }
    view.chars.push(if c.c.is_whitespace() { SPACE } else { c });
}

/// Collapses each whitespace run to one space and trims it at block edges.
fn normalise(chars: Vec<StyledChar>) -> Vec<StyledChar> {
    let mut out: Vec<StyledChar> = Vec::with_capacity(chars.len());
    let mut space = false;
    let mut block_start = true;
    for c in chars {
        if c == BOUNDARY {
            space = false;
            block_start = true;
        } else if c == SPACE {
            space = !block_start;
        } else {
            if space {
                out.push(SPACE);
            }
            out.push(c);
            space = false;
            block_start = false;
        }
    }
    out
}

fn push_inlines(view: &mut View, seen: &mut usize, inlines: &Inlines) {
    for (at, c) in inlines.text.char_indices() {
        if c == Inlines::OBJECT {
            continue;
        }
        let span = inlines
            .spans
            .iter()
            .find(|span| span.range.start.index() <= at && at < span.range.end.index());
        let styled = StyledChar {
            c,
            styles: span.map_or(0, |span| style_bits(span.style)),
            link: span.is_some_and(|span| span.link.is_some()),
        };
        push_char(view, seen, styled);
    }
}

fn push_plain(view: &mut View, seen: &mut usize, text: &str) {
    for c in text.chars() {
        push_char(
            view,
            seen,
            StyledChar {
                c,
                styles: 0,
                link: false,
            },
        );
    }
}

// The HTML side.

/// Elements whose text belongs to no compared block (footnote back-references) or is never
/// shown.
fn skips_content(name: &str, attributes: &[(String, String)]) -> bool {
    has_class(attributes, "footnote-backref")
        || attributes
            .iter()
            .any(|(name, _)| name == "data-footnote-backref")
        || matches!(name, "script" | "style" | "title" | "textarea")
}

fn has_class(attributes: &[(String, String)], class: &str) -> bool {
    attributes
        .iter()
        .any(|(name, value)| name == "class" && value.split_whitespace().any(|c| c == class))
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

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about one open element"
)]
struct Open {
    name: String,
    /// Container this element contributes to the path, if any.
    container: Option<&'static str>,
    style: Option<Style>,
    link: bool,
    /// Whether it opens a block whose text is recorded (p, h1-h6, pre, alert title).
    block: bool,
    /// The `<ol>` of a footnote section, whose items are footnotes, not list items.
    footnote_list: bool,
    skip: bool,
    /// For a recorded <p>: the block count and text bytes seen when it opened, to drop it on close
    /// if it showed nothing (a paragraph of only HTML tags).
    content_at: Option<(usize, usize)>,
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per HTML element kind; splitting would scatter the mapping"
)]
pub(crate) fn html_view(html: &str) -> View {
    let mut view = View {
        blocks: Vec::new(),
        chars: Vec::new(),
    };
    let mut open: Vec<Open> = Vec::new();
    // Whether the last block is text outside any block element (in a tight list item, or in raw
    // block HTML such as <div>) that further text continues. Block-level tags end it, as they
    // end the builder's paragraph.
    let mut implicit_open = false;
    let mut images = 0_usize;
    // Bytes of text seen, whitespace included: a paragraph of only spaces still shows a line.
    let mut text_seen = 0_usize;
    let mut seen = 0;
    let mut images_at_open: Vec<usize> = Vec::new();
    for token in Tokenizer::new(html) {
        let Ok(token) = token;
        match token {
            Token::StartTag(tag) => {
                let name = String::from_utf8_lossy(&tag.name).to_ascii_lowercase();
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
                if is_block_level(&name) {
                    implicit_open = false;
                }
                let in_pre = open.iter().any(|o| o.name == "pre");
                let in_footnotes = open.iter().any(|o| o.container == Some("footnote"));
                let parent_is_footnote_list = open.last().is_some_and(|o| o.footnote_list);
                let path = html_path(&open);
                let mut cell_started = false;
                let mut element = Open {
                    name: name.clone(),
                    container: None,
                    style: None,
                    link: false,
                    block: false,
                    footnote_list: false,
                    skip: skips_content(&name, &attributes),
                    content_at: None,
                };
                let mut record = |kind: &str| {
                    view.blocks.push(Block {
                        path: path.clone(),
                        kind: kind.to_owned(),
                    });
                };
                match name.as_str() {
                    "blockquote" => element.container = Some("quote"),
                    "ol" if in_footnotes && !open.iter().any(|o| o.name == "li") => {
                        element.footnote_list = true;
                    }
                    "ul" => element.container = Some("ul"),
                    "ol" => element.container = Some("ol"),
                    "li" if !parent_is_footnote_list => element.container = Some("li"),
                    "section" if has_class(&attributes, "footnotes") => {
                        element.container = Some("footnote");
                    }
                    "div" if has_class(&attributes, "markdown-alert") => {
                        element.container = Some("alert");
                    }
                    "p" if has_class(&attributes, "markdown-alert-title") => {
                        record("alert-title");
                        element.block = true;
                    }
                    "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "pre" => {
                        record(&name);
                        element.block = true;
                        element.content_at = Some((view.blocks.len(), text_seen));
                    }
                    // Cell text belongs to the table, not to a paragraph of its own; cells are
                    // separated like words.
                    "td" | "th" => {
                        element.block = true;
                        cell_started = true;
                    }
                    "img" => images += 1,
                    "hr" => record("hr"),
                    "table" => record("table0"),
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
                    "em" | "i" => element.style = Some(Style::EMPHASIS),
                    "strong" | "b" => element.style = Some(Style::STRONG),
                    "del" | "s" | "strike" => element.style = Some(Style::STRIKETHROUGH),
                    "code" if !in_pre => element.style = Some(Style::CODE),
                    "sup" if has_class(&attributes, "footnote-ref") => {
                        element.style = Some(Style::FOOTNOTE_REFERENCE);
                    }
                    "a" => element.link = attributes.iter().any(|(n, _)| n == "href"),
                    _ => {}
                }
                if cell_started {
                    push_char(&mut view, &mut seen, SPACE);
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
                if open.iter().any(|o| o.skip) {
                    continue;
                }
                let text = String::from_utf8_lossy(&text).into_owned();
                text_seen += text.len();
                if text.chars().all(char::is_whitespace) {
                    // Whitespace between elements separates words but starts no block.
                    push_char(&mut view, &mut seen, SPACE);
                    continue;
                }
                // Text outside any block element is a paragraph of its own: a tight list
                // item's, or text in raw block HTML.
                let in_block = open.iter().any(|o| o.block);
                if !in_block && !implicit_open {
                    let path = html_path(&open);
                    let tight = path.last() == Some(&"li");
                    view.blocks.push(Block {
                        path,
                        kind: if tight { "p-tight" } else { "p" }.to_owned(),
                    });
                    implicit_open = true;
                }
                let styles = open
                    .iter()
                    .filter_map(|o| o.style)
                    .fold(Style::PLAIN, Style::union);
                let link = open.iter().any(|o| o.link);
                for c in text.chars() {
                    let character = StyledChar {
                        c,
                        styles: style_bits(styles),
                        link,
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

/// Tags that end the text before them, as `builder::is_block` and headings do for the model.
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

fn html_path(open: &[Open]) -> Vec<&'static str> {
    open.iter().filter_map(|o| o.container).collect()
}
