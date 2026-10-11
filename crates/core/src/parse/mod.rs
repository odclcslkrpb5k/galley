//! The only place comrak is used (docs/plan.md §3): parse with GitHub's options, then walk
//! comrak's tree into `Builder` calls. No comrak type leaves this module; the gate's boundary
//! check refuses `comrak` anywhere else.

use std::collections::HashMap;

use comrak::nodes::{AlertType, AstNode, ListDelimType, ListType, NodeValue, TableAlignment};
use comrak::{Anchorizer, Arena, Options, parse_document};

use crate::builder::{Builder, InlineBuilder};
use crate::model::{
    AlertKind, Align, Container, Delimiter, Document, HeadingLevel, InlineObject, ListItem, Marker,
    Style, Table,
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
pub(crate) fn parse(text: &str, builder: Builder) -> Document {
    let arena = Arena::new();
    let root = parse_document(&arena, text, &gfm_options());
    let mut walker = Walker {
        builder,
        footnotes: footnote_numbers(root),
        alert_prefix: None,
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

/// Footnote ids and numbers by label. comrak moves definitions to the end of the document in
/// order of first reference, and drops unreferenced ones, so that order is GitHub's numbering;
/// ids follow the same order because definitions are lowered in it.
fn footnote_numbers<'a>(root: &'a AstNode<'a>) -> HashMap<String, (FootnoteId, u32)> {
    let mut numbers = HashMap::new();
    let mut next = 0;
    for child in root.children() {
        if let NodeValue::FootnoteDefinition(definition) = &child.data().value {
            next += 1;
            numbers.insert(
                definition.name.clone(),
                (FootnoteId::new(numbers.len()), next),
            );
        }
    }
    numbers
}

struct Walker {
    builder: Builder,
    footnotes: HashMap<String, (FootnoteId, u32)>,
    /// For an alert with a custom title, which GitHub shows as a plain quote: the marker line,
    /// to put in front of the quote's first paragraph.
    alert_prefix: Option<String>,
}

impl Walker {
    fn children<'a>(&mut self, node: &'a AstNode<'a>) {
        for child in node.children() {
            self.block(child);
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per comrak block type, each short; splitting would scatter the mapping"
    )]
    fn block<'a>(&mut self, node: &'a AstNode<'a>) {
        let value = node.data().value.clone();
        match value {
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
                self.builder.code_block("yaml", body.trim_end_matches('\n'));
            }
            NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) => {
                self.builder.begin_container(Container::Quote);
                self.children(node);
                self.builder.end_container();
            }
            NodeValue::Alert(alert) => {
                let kind = alert_kind(alert.alert_type);
                if let Some(title) = alert.title {
                    // GitHub does not support custom alert titles: it shows a plain quote whose
                    // first line is the marker as written.
                    self.builder.begin_container(Container::Quote);
                    self.alert_prefix = Some(format!("{} {}", kind.marker(), title.trim()));
                    self.children(node);
                    if let Some(prefix) = self.alert_prefix.take() {
                        let mut inlines = InlineBuilder::new();
                        inlines.text(&prefix);
                        self.builder.paragraph(inlines);
                    }
                } else {
                    self.builder.begin_alert(kind);
                    self.children(node);
                }
                self.builder.end_container();
            }
            NodeValue::List(list) => {
                let start = u64::try_from(list.start).unwrap_or(u64::MAX);
                for (offset, item) in (0_u64..).zip(node.children()) {
                    let marker = match (&item.data().value, list.list_type) {
                        (NodeValue::TaskItem(task), _) => Marker::Task {
                            checked: task.symbol.is_some(),
                        },
                        (_, ListType::Bullet) => Marker::Bullet,
                        (_, ListType::Ordered) => Marker::Ordered {
                            number: start.saturating_add(offset),
                            delimiter: match list.delimiter {
                                ListDelimType::Period => Delimiter::Period,
                                ListDelimType::Paren => Delimiter::Paren,
                            },
                        },
                    };
                    self.builder.begin_container(Container::ListItem(ListItem {
                        marker,
                        tight: list.tight,
                        show_marker: true,
                    }));
                    if item.first_child().is_none() {
                        // An empty item still shows its marker.
                        self.builder.paragraph(InlineBuilder::new());
                    } else {
                        self.children(item);
                    }
                    self.builder.end_container();
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
                let mut inlines = InlineBuilder::new();
                if let Some(prefix) = self.alert_prefix.take() {
                    inlines.text(&prefix);
                    inlines.text(" ");
                }
                self.inlines(node, &mut inlines);
                self.builder.paragraph(inlines);
            }
            NodeValue::Heading(heading) => {
                let mut inlines = InlineBuilder::new();
                self.inlines(node, &mut inlines);
                self.builder.heading(
                    HeadingLevel::clamped(heading.level),
                    inlines,
                    &node.collect_text(),
                );
            }
            NodeValue::ThematicBreak => self.builder.thematic_break(),
            NodeValue::FootnoteDefinition(definition) => {
                let number = self
                    .footnotes
                    .get(&definition.name)
                    .map_or(0, |&(_id, number)| number);
                let id = self.builder.begin_footnote(&definition.name, number);
                self.children(node);
                self.builder.end_footnote(id);
            }
            NodeValue::Table(table) => {
                let mut header = Vec::new();
                let mut rows = Vec::new();
                for row in node.children() {
                    let is_header = matches!(row.data().value, NodeValue::TableRow(true));
                    let cells: Vec<_> = row
                        .children()
                        .map(|cell| {
                            let mut inlines = InlineBuilder::new();
                            self.inlines(cell, &mut inlines);
                            inlines.finish()
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
                let mut inlines = InlineBuilder::new();
                self.inline(node, &mut inlines);
                self.builder.paragraph(inlines);
            }
        }
    }

    fn inlines<'a>(&mut self, node: &'a AstNode<'a>, inlines: &mut InlineBuilder) {
        for child in node.children() {
            self.inline(child, inlines);
        }
    }

    fn styled<'a>(&mut self, node: &'a AstNode<'a>, inlines: &mut InlineBuilder, style: Style) {
        inlines.begin_style(style);
        self.inlines(node, inlines);
        inlines.end_style();
    }

    fn inline<'a>(&mut self, node: &'a AstNode<'a>, inlines: &mut InlineBuilder) {
        let value = node.data().value.clone();
        match value {
            NodeValue::Text(text) => inlines.text(&text),
            NodeValue::SoftBreak => inlines.text(" "),
            NodeValue::LineBreak => inlines.text("\n"),
            NodeValue::Code(code) => {
                inlines.begin_style(Style::CODE);
                inlines.text(&code.literal);
                inlines.end_style();
            }
            NodeValue::Math(math) => {
                inlines.begin_style(Style::MATH);
                inlines.text(&math.literal);
                inlines.end_style();
            }
            NodeValue::HtmlInline(html) => self.builder.html_inline(inlines, &html),
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
                inlines.begin_link(id);
                self.inlines(node, inlines);
                inlines.end_link();
            }
            NodeValue::Image(link) => {
                let id = self
                    .builder
                    .markdown_image(&link.url, &link.title, node.collect_text());
                inlines.object(InlineObject::Image(id));
            }
            NodeValue::FootnoteReference(reference) => {
                if let Some(&(footnote, number)) = self.footnotes.get(&reference.name) {
                    let link = self.builder.add_footnote_link(footnote);
                    inlines.begin_link(link);
                    inlines.begin_style(Style::FOOTNOTE_REFERENCE);
                    inlines.text(&number.to_string());
                    inlines.end_style();
                    inlines.end_link();
                } else {
                    inlines.text(&format!("[^{}]", reference.name));
                }
            }
            NodeValue::EscapedTag(tag) => inlines.text(tag),
            NodeValue::Raw(text) => inlines.text(&text),
            // Containers of inline content; blocks never appear inside inline content, but if one
            // did, its text is still kept.
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
