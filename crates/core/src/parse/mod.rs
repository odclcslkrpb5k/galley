//! The only place comrak is used (docs/plan.md §3): parse with GitHub's options, then walk
//! comrak's tree into `Builder` calls. No comrak type leaves this module; the gate's boundary
//! check refuses comrak code anywhere else.

use std::collections::HashMap;

use comrak::nodes::{AlertType, AstNode, ListDelimType, ListType, NodeValue, TableAlignment};
use comrak::{Anchorizer, Arena, Options, parse_document};

use crate::builder::{Builder, ContainerToken, InlineBuilder};
use crate::model::{
    AlertKind, Align, Container, Delimiter, Document, HeadingLevel, InlineObject, Marker, Style,
    Table,
};
use crate::units::FootnoteId;

/// The comrak options matching what github.com renders (docs/plan.md §5). Every option GitHub
/// does not support is set off explicitly, so a changed comrak default cannot turn it on.
/// Shortcodes, Phoenix `HEEx` and attribute syntax are cargo features of comrak, left out of the
/// build entirely (`default-features = false`); shortcodes come in M3.
pub(crate) fn gfm_options() -> Options<'static> {
    let mut options = Options::default();
    let extension = &mut options.extension;
    extension.strikethrough = true;
    extension.table = true;
    extension.autolink = true;
    extension.tasklist = true;
    extension.footnotes = true;
    extension.alerts = true;
    extension.math_dollars = true;
    extension.math_code = true;
    extension.front_matter_delimiter = Some("---".to_owned());
    // Off: GitHub has none of these.
    extension.superscript = false;
    extension.subscript = false;
    extension.underline = false;
    extension.spoiler = false;
    extension.highlight = false;
    extension.insert = false;
    extension.greentext = false;
    extension.subtext = false;
    extension.inline_footnotes = false;
    extension.description_lists = false;
    extension.multiline_block_quotes = false;
    extension.wikilinks_title_after_pipe = false;
    extension.wikilinks_title_before_pipe = false;
    extension.math_latex = false;
    extension.block_directive = false;
    extension.cjk_friendly_emphasis = false;
    extension.header_id_prefix = None;
    let parse = &mut options.parse;
    parse.smart = false;
    parse.relaxed_tasklist_matching = false;
    parse.tasklist_in_table = false;
    parse.relaxed_autolinks = false;
    parse.ignore_setext = false;
    parse.leave_footnote_definitions = false;
    options.render.hardbreaks = false;
    options
}

/// Parses Markdown text into the model.
pub(crate) fn parse(text: &str, mut builder: Builder) -> Document {
    let arena = Arena::new();
    let root = parse_document(&arena, text, &gfm_options());
    let footnotes = register_footnotes(root, &mut builder);
    let mut walker = Walker {
        builder,
        source: text,
        footnotes,
        lowered_footnotes: Vec::new(),
    };
    walker.block(root);
    walker.builder.finish()
}

/// A slugger with GitHub's heading-anchor rules (comrak's `Anchorizer`, one per document so
/// repeated headings get `-1`, `-2`, ...).
pub(crate) fn github_slugger() -> crate::builder::Slugger {
    let mut anchorizer = Anchorizer::new();
    Box::new(move |text: &str| anchorizer.anchorize(text))
}

/// Registers the footnotes, numbered as GitHub shows them. comrak moves definitions to the end
/// of the document in order of first reference and drops unreferenced ones; a label defined
/// twice keeps its first definition.
fn register_footnotes<'a>(
    root: &'a AstNode<'a>,
    builder: &mut Builder,
) -> HashMap<String, (FootnoteId, usize)> {
    let mut footnotes = HashMap::new();
    for child in root.children() {
        if let NodeValue::FootnoteDefinition(definition) = &child.data().value
            && !footnotes.contains_key(&definition.name)
        {
            let number = footnotes.len() + 1;
            let id = builder.add_footnote(&definition.name, number);
            footnotes.insert(definition.name.clone(), (id, number));
        }
    }
    footnotes
}

struct Walker<'s> {
    builder: Builder,
    /// The Markdown text, for what comrak's tree does not keep (a custom alert title as written).
    source: &'s str,
    footnotes: HashMap<String, (FootnoteId, usize)>,
    lowered_footnotes: Vec<FootnoteId>,
}

impl Walker<'_> {
    fn children<'a>(&mut self, node: &'a AstNode<'a>) {
        for child in node.children() {
            self.block(child);
        }
    }

    /// Lowers `node`'s children inside `container`. A list item or footnote always gets a leaf,
    /// to carry its marker.
    fn container<'a>(&mut self, container: Container, node: &'a AstNode<'a>) {
        let needs_leaf = matches!(
            container,
            Container::ListItem { .. } | Container::Footnote(_)
        );
        let token = self.builder.begin_container(container);
        self.children(node);
        self.close(token, needs_leaf);
    }

    fn close(&mut self, token: ContainerToken, needs_leaf: bool) {
        if needs_leaf {
            self.builder.ensure_started(token);
        }
        self.builder.end_container(token);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per comrak block type, each short; splitting would scatter the mapping"
    )]
    fn block<'a>(&mut self, node: &'a AstNode<'a>) {
        let data = node.data();
        match &data.value {
            NodeValue::Document
            | NodeValue::DescriptionList
            | NodeValue::DescriptionItem(_)
            | NodeValue::DescriptionTerm
            | NodeValue::DescriptionDetails
            | NodeValue::BlockDirective(_)
            | NodeValue::Subtext => self.children(node),
            NodeValue::FrontMatter(text) => {
                // GitHub shows front matter as a table; v1 shows it as YAML (a known deviation).
                let lines: Vec<&str> = text
                    .lines()
                    .filter(|line| line.trim_end() != "---")
                    .collect();
                let body = lines.join("\n");
                let body = body.trim_end_matches('\n');
                let literal = if body.is_empty() {
                    String::new()
                } else {
                    format!("{body}\n")
                };
                self.builder.code_block("yaml", &literal);
            }
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                if let Some(kind) = unspaced_alert(node) {
                    self.unspaced_alert(kind, node);
                } else {
                    self.container(Container::Quote, node);
                }
            }
            NodeValue::Alert(alert) => {
                let kind = alert_kind(alert.alert_type);
                if alert.title.is_some() {
                    // GitHub does not support custom alert titles: it shows a plain quote whose
                    // first line is the marker line as written.
                    let line = data.sourcepos.start.line;
                    self.titled_alert(line, node);
                } else {
                    let token = self.builder.begin_alert(kind);
                    self.children(node);
                    self.close(token, false);
                }
            }
            NodeValue::List(list) => {
                for (offset, item) in node.children().enumerate() {
                    let marker = match (&item.data().value, list.list_type) {
                        (NodeValue::TaskItem(task), _) => Marker::Task {
                            checked: task.symbol.is_some(),
                        },
                        (_, ListType::Bullet) => Marker::Bullet,
                        (_, ListType::Ordered) => Marker::Ordered {
                            number: list.start.saturating_add(offset),
                            delimiter: match list.delimiter {
                                ListDelimType::Period => Delimiter::Period,
                                ListDelimType::Paren => Delimiter::Paren,
                            },
                        },
                    };
                    self.container(
                        Container::ListItem {
                            marker,
                            tight: list.tight,
                        },
                        item,
                    );
                }
            }
            NodeValue::Item(_) | NodeValue::TaskItem(_) => {
                // Only under a List, which handles its items; elsewhere, keep the content.
                self.children(node);
            }
            NodeValue::CodeBlock(code) => {
                let language = code.info.split_whitespace().next().unwrap_or("");
                self.builder.code_block(language, &code.literal);
            }
            NodeValue::HtmlBlock(html) => self.builder.html_block(&html.literal),
            NodeValue::Paragraph => {
                let mut inlines = self.builder.inline_builder();
                self.inlines(node, &mut inlines);
                self.builder.paragraph(inlines);
            }
            NodeValue::Heading(heading) => {
                let mut inlines = self.builder.inline_builder();
                self.inlines(node, &mut inlines);
                let mut slug = String::new();
                github_text(node, &mut slug);
                match HeadingLevel::from_number(heading.level) {
                    Some(level) => self.builder.heading(level, inlines, &slug),
                    // comrak's levels are 1-6; a level outside them would show as a paragraph.
                    None => self.builder.paragraph(inlines),
                }
            }
            NodeValue::ThematicBreak => self.builder.thematic_break(),
            NodeValue::FootnoteDefinition(definition) => {
                // A label defined twice: GitHub keeps the first definition.
                if let Some(&(id, _number)) = self.footnotes.get(&definition.name)
                    && !self.lowered_footnotes.contains(&id)
                {
                    self.lowered_footnotes.push(id);
                    self.container(Container::Footnote(id), node);
                }
            }
            NodeValue::Table(table) => {
                let mut header = Vec::new();
                let mut rows = Vec::new();
                for row in node.children() {
                    let is_header = matches!(row.data().value, NodeValue::TableRow(true));
                    let cells: Vec<_> = row
                        .children()
                        .map(|cell| {
                            let mut inlines = self.builder.inline_builder();
                            self.inlines(cell, &mut inlines);
                            self.builder.finish_inlines(inlines)
                        })
                        .collect();
                    if is_header {
                        header = cells;
                    } else {
                        rows.push(cells);
                    }
                }
                self.builder.table(Table {
                    alignments: table.alignments.iter().copied().map(align).collect(),
                    header,
                    rows,
                });
            }
            // Inline content never stands as a block; if it ever did, keep its text.
            NodeValue::TableRow(_)
            | NodeValue::TableCell
            | NodeValue::Text(_)
            | NodeValue::SoftBreak
            | NodeValue::LineBreak
            | NodeValue::Code(_)
            | NodeValue::HtmlInline(_)
            | NodeValue::Raw(_)
            | NodeValue::Emph
            | NodeValue::Strong
            | NodeValue::Strikethrough
            | NodeValue::Highlight
            | NodeValue::Insert
            | NodeValue::Superscript
            | NodeValue::Link(_)
            | NodeValue::Image(_)
            | NodeValue::FootnoteReference(_)
            | NodeValue::Math(_)
            | NodeValue::Escaped
            | NodeValue::WikiLink(_)
            | NodeValue::Underline
            | NodeValue::Subscript
            | NodeValue::SpoileredText
            | NodeValue::EscapedTag(_) => {
                let mut inlines = self.builder.inline_builder();
                self.inline(node, &mut inlines);
                self.builder.paragraph(inlines);
            }
        }
    }

    /// `>[!TIP]` with no space after `>`: comrak reads a plain quote, GitHub an alert. The
    /// marker text (and the line break after it) is not shown.
    fn unspaced_alert<'a>(&mut self, kind: AlertKind, node: &'a AstNode<'a>) {
        let token = self.builder.begin_alert(kind);
        for (index, child) in node.children().enumerate() {
            if index == 0 {
                let mut inlines = self.builder.inline_builder();
                for inline in child.children().skip(2) {
                    self.inline(inline, &mut inlines);
                }
                self.builder.paragraph(inlines);
            } else {
                self.block(child);
            }
        }
        self.close(token, false);
    }

    /// An alert with a custom title, which GitHub shows as a plain quote whose first line is the
    /// marker line as written (case kept, inline Markdown parsed), joined to the first paragraph
    /// by a soft break or standing alone before a non-paragraph.
    fn titled_alert<'a>(&mut self, line: usize, node: &'a AstNode<'a>) {
        let marker_line = line
            .checked_sub(1)
            .and_then(|index| self.source.lines().nth(index))
            .map_or("", |text| {
                let text = text.trim_start();
                let text = text.strip_prefix('>').unwrap_or(text);
                text.strip_prefix(' ').unwrap_or(text)
            })
            .trim_end();
        let token = self.builder.begin_container(Container::Quote);
        let mut prefix = self.builder.inline_builder();
        let arena = Arena::new();
        let parsed = parse_document(&arena, marker_line, &gfm_options());
        for paragraph in parsed.children() {
            self.inlines(paragraph, &mut prefix);
        }
        let mut children = node.children().peekable();
        if let Some(first) = children.peek()
            && matches!(first.data().value, NodeValue::Paragraph)
        {
            prefix.text(" ");
            self.inlines(first, &mut prefix);
            children.next();
        }
        self.builder.paragraph(prefix);
        for child in children {
            self.block(child);
        }
        self.close(token, false);
    }

    fn inlines<'a>(&mut self, node: &'a AstNode<'a>, inlines: &mut InlineBuilder) {
        for child in node.children() {
            self.inline(child, inlines);
        }
    }

    fn styled<'a>(&mut self, node: &'a AstNode<'a>, inlines: &mut InlineBuilder, style: Style) {
        let token = inlines.begin_style(style);
        self.inlines(node, inlines);
        inlines.end_style(token);
    }

    fn inline<'a>(&mut self, node: &'a AstNode<'a>, inlines: &mut InlineBuilder) {
        let data = node.data();
        match &data.value {
            NodeValue::Text(text) => {
                // A newline in text (from `&#10;`) is whitespace, as in a browser; hard breaks
                // are their own node.
                if text.contains('\n') {
                    inlines.text(&text.replace('\n', " "));
                } else {
                    inlines.text(text);
                }
            }
            NodeValue::SoftBreak => inlines.text(" "),
            NodeValue::LineBreak => inlines.text("\n"),
            NodeValue::Code(code) => {
                let token = inlines.begin_style(Style::CODE);
                inlines.text(&code.literal);
                inlines.end_style(token);
            }
            NodeValue::Math(math) => {
                let style = if math.display_math {
                    Style::DISPLAY_MATH
                } else {
                    Style::MATH
                };
                let token = inlines.begin_style(style);
                inlines.text(&math.literal);
                inlines.end_style(token);
            }
            NodeValue::HtmlInline(html) => self.builder.html_inline(inlines, html),
            NodeValue::Emph => self.styled(node, inlines, Style::EMPHASIS),
            NodeValue::Strong => self.styled(node, inlines, Style::STRONG),
            NodeValue::Strikethrough => self.styled(node, inlines, Style::STRIKETHROUGH),
            NodeValue::Superscript => self.styled(node, inlines, Style::SUPERSCRIPT),
            NodeValue::Subscript => self.styled(node, inlines, Style::SUBSCRIPT),
            NodeValue::Underline | NodeValue::Insert => {
                self.styled(node, inlines, Style::UNDERLINE);
            }
            NodeValue::Highlight => self.styled(node, inlines, Style::HIGHLIGHT),
            NodeValue::Link(link) => {
                let id = self.builder.add_link(&link.url, &link.title);
                let token = inlines.begin_link(id);
                self.inlines(node, inlines);
                inlines.end_link(token);
            }
            NodeValue::Image(link) => {
                let id = self
                    .builder
                    .markdown_image(&link.url, &link.title, node.collect_text());
                inlines.object(InlineObject::Image(id));
            }
            NodeValue::FootnoteReference(reference) => {
                // comrak only makes reference nodes for defined footnotes; were that ever not
                // so, the reference shows as written rather than with a made-up number.
                if let Some(&(footnote, number)) = self.footnotes.get(&reference.name) {
                    let link = self.builder.add_footnote_link(footnote);
                    let link_token = inlines.begin_link(link);
                    let style_token = inlines.begin_style(Style::FOOTNOTE_REFERENCE);
                    inlines.text(&number.to_string());
                    inlines.end_style(style_token);
                    inlines.end_link(link_token);
                } else {
                    inlines.text(&format!("[^{}]", reference.name));
                }
            }
            NodeValue::EscapedTag(tag) => inlines.text(tag),
            NodeValue::Raw(text) => inlines.text(text),
            // Containers of inline content; blocks never appear inside inline content, but if
            // one did, its text is still kept.
            NodeValue::Escaped
            | NodeValue::WikiLink(_)
            | NodeValue::SpoileredText
            | NodeValue::TableCell
            | NodeValue::Document
            | NodeValue::FrontMatter(_)
            | NodeValue::BlockQuote
            | NodeValue::List(_)
            | NodeValue::Item(_)
            | NodeValue::DescriptionList
            | NodeValue::DescriptionItem(_)
            | NodeValue::DescriptionTerm
            | NodeValue::DescriptionDetails
            | NodeValue::CodeBlock(_)
            | NodeValue::HtmlBlock(_)
            | NodeValue::Paragraph
            | NodeValue::Heading(_)
            | NodeValue::ThematicBreak
            | NodeValue::FootnoteDefinition(_)
            | NodeValue::Table(_)
            | NodeValue::TableRow(_)
            | NodeValue::TaskItem(_)
            | NodeValue::MultilineBlockQuote(_)
            | NodeValue::Alert(_)
            | NodeValue::Subtext
            | NodeValue::BlockDirective(_) => self.inlines(node, inlines),
        }
    }
}

/// The text GitHub slugs a heading from: its rendered text content. Images contribute nothing
/// (alt text is an attribute), raw HTML tags nothing, and line breaks a newline, which the
/// slugger drops.
fn github_text<'a>(node: &'a AstNode<'a>, out: &mut String) {
    for child in node.children() {
        let data = child.data();
        match &data.value {
            NodeValue::Text(text) => out.push_str(text),
            NodeValue::Code(code) => out.push_str(&code.literal),
            NodeValue::Math(math) => out.push_str(&math.literal),
            NodeValue::SoftBreak | NodeValue::LineBreak => out.push('\n'),
            NodeValue::Image(_) | NodeValue::HtmlInline(_) | NodeValue::FootnoteReference(_) => {}
            NodeValue::Document
            | NodeValue::FrontMatter(_)
            | NodeValue::BlockQuote
            | NodeValue::List(_)
            | NodeValue::Item(_)
            | NodeValue::DescriptionList
            | NodeValue::DescriptionItem(_)
            | NodeValue::DescriptionTerm
            | NodeValue::DescriptionDetails
            | NodeValue::CodeBlock(_)
            | NodeValue::HtmlBlock(_)
            | NodeValue::Paragraph
            | NodeValue::Heading(_)
            | NodeValue::ThematicBreak
            | NodeValue::FootnoteDefinition(_)
            | NodeValue::Table(_)
            | NodeValue::TableRow(_)
            | NodeValue::TableCell
            | NodeValue::TaskItem(_)
            | NodeValue::Raw(_)
            | NodeValue::Emph
            | NodeValue::Strong
            | NodeValue::Strikethrough
            | NodeValue::Highlight
            | NodeValue::Insert
            | NodeValue::Superscript
            | NodeValue::Link(_)
            | NodeValue::MultilineBlockQuote(_)
            | NodeValue::Escaped
            | NodeValue::WikiLink(_)
            | NodeValue::Underline
            | NodeValue::Subscript
            | NodeValue::SpoileredText
            | NodeValue::EscapedTag(_)
            | NodeValue::Alert(_)
            | NodeValue::Subtext
            | NodeValue::BlockDirective(_) => github_text(child, out),
        }
    }
}

/// A quote whose first paragraph is exactly `[!KIND]` and a line break (or nothing): written
/// `>[!TIP]`, which comrak's alert parser misses and GitHub accepts.
fn unspaced_alert<'a>(node: &'a AstNode<'a>) -> Option<AlertKind> {
    let paragraph = node.first_child()?;
    if !matches!(paragraph.data().value, NodeValue::Paragraph) {
        return None;
    }
    let marker = paragraph.first_child()?;
    let NodeValue::Text(text) = &marker.data().value else {
        return None;
    };
    let name = text.trim().strip_prefix("[!")?.strip_suffix(']')?;
    let kind = AlertKind::from_marker(name)?;
    let next_is_break = marker.next_sibling().is_none_or(|next| {
        matches!(
            next.data().value,
            NodeValue::SoftBreak | NodeValue::LineBreak
        )
    });
    next_is_break.then_some(kind)
}

fn alert_kind(alert: AlertType) -> AlertKind {
    match alert {
        AlertType::Note => AlertKind::Note,
        AlertType::Tip => AlertKind::Tip,
        AlertType::Important => AlertKind::Important,
        AlertType::Warning => AlertKind::Warning,
        AlertType::Caution => AlertKind::Caution,
    }
}

fn align(alignment: TableAlignment) -> Align {
    match alignment {
        TableAlignment::None | TableAlignment::Left => Align::Start,
        TableAlignment::Center => Align::Center,
        TableAlignment::Right => Align::End,
    }
}
