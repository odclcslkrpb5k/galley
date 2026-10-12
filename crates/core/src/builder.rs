//! Building a `Document` leaf by leaf (docs/plan.md §4.2, §5.1).
//!
//! No comrak here: `parse` walks comrak's tree and calls these methods, so everything about the
//! model's shape (the context tree, chunking, styled runs, what HTML tags mean) is in one place
//! that does not depend on the parser.
//!
//! Raw HTML has one tag stack for the whole document. Tags opened in HTML blocks stay open
//! across Markdown until closed (`<div align="center">` ... Markdown ... `</div>`, or a link
//! around a block of sponsors); tags opened inside a Markdown paragraph close with it. Each
//! entry caches the cumulative state (alignment, style, link, details, hidden), so every query
//! is O(1) however deep the stack, and a closing tag with no opener is rejected in O(1).

use std::collections::HashMap;

use crate::html::{HtmlToken, tokens};
use crate::link::{Use, classify};
use crate::model::{
    AlertKind, Align, Anchor, CODE_CHUNK_LINES, CodeBlock, Container, Context, Document, Footnote,
    HeadingLevel, Image, InlineObject, Inlines, LEAF_TEXT_BYTES, Leaf, LeafKind, Link, LinkTarget,
    Notice, Span, Style, TABLE_CHUNK_ROWS, Table,
};
use crate::units::{
    AnchorId, ByteOffset, CodeBlockId, ContextId, DetailsId, FootnoteId, ImageId, LeafId, LineNo,
    LinkId, RowNo, TableId,
};

/// Turns heading text into a GitHub anchor slug, unique within the document.
pub(crate) type Slugger = Box<dyn FnMut(&str) -> String>;

/// The widest `colspan` followed: more would only make empty columns.
const MAX_COLSPAN: usize = 64;

/// HTML tags followed deeper than this are ignored (with a notice): the stack is per document
/// and a generated file could otherwise open tags without end.
const MAX_HTML_DEPTH: usize = 256;

/// Tags GitHub's tag filter shows as literal text instead of interpreting.
fn is_filtered(name: &str) -> bool {
    matches!(
        name,
        "title"
            | "textarea"
            | "style"
            | "xmp"
            | "iframe"
            | "noembed"
            | "noframes"
            | "script"
            | "plaintext"
    )
}

/// An open container, to be closed with `Builder::end_container`.
#[must_use = "a container must be closed with end_container"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContainerToken(usize);

pub(crate) struct Builder {
    doc: Document,
    /// Open containers, innermost last.
    open: Vec<ContextId>,
    /// Open containers from `started..` have no leaf yet; the next leaf is their first.
    started: usize,
    /// Raw HTML containers closed while Markdown containers opened inside them are still open:
    /// each closes as soon as those do, so `</details>` inside a list item does not tear the
    /// item down from inside.
    deferred: Vec<ContainerToken>,
    html: HtmlStack,
    /// Text from HTML blocks not yet made into a leaf.
    pending: Option<Pending>,
    /// Text of an open raw `<pre>`, kept verbatim.
    pre: Option<String>,
    /// An open `<summary>`: everything until `</summary>` (HTML text, Markdown paragraphs,
    /// headings) is its one line.
    summary: Option<(DetailsId, InlineBuilder)>,
    /// A `<details>` with no summary and no leaf yet. If anything else comes first, it gets the
    /// summary a browser gives it: "Details".
    untitled: Option<DetailsId>,
    /// Names from raw `<a id>` and `<a name>` tags, for the next leaf.
    pending_anchors: Vec<String>,
    /// An open raw HTML `<table>`.
    html_table: Option<HtmlTable>,
    next_details: usize,
    slugger: Slugger,
}

struct Pending {
    inlines: InlineBuilder,
    kind: PendingKind,
}

enum PendingKind {
    Paragraph,
    /// An HTML heading, with its text as written (GitHub slugs that, not the collapsed text).
    Heading {
        level: HeadingLevel,
        slug: String,
    },
}

/// Where an inline builder's HTML state comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct HtmlState {
    style: Style,
    link: Option<LinkId>,
    hidden: bool,
}

impl Builder {
    pub(crate) fn new(slugger: Slugger) -> Self {
        Self {
            doc: Document::default(),
            open: Vec::new(),
            started: 0,
            deferred: Vec::new(),
            html: HtmlStack::default(),
            pending: None,
            pre: None,
            summary: None,
            untitled: None,
            pending_anchors: Vec::new(),
            html_table: None,
            next_details: 0,
            slugger,
        }
    }

    pub(crate) fn notice(&mut self, notice: Notice) {
        if !self.doc.notices.contains(&notice) {
            self.doc.notices.push(notice);
        }
    }

    pub(crate) fn finish(mut self) -> Document {
        self.flush_pre();
        self.flush_pending();
        self.finish_html_table();
        self.flush_summary();
        self.default_summary();
        // Anchors after the last leaf (`<a name="bottom">` at the end) name the last leaf.
        if let Some(last) = self.doc.leaves.len().checked_sub(1) {
            for slug in std::mem::take(&mut self.pending_anchors) {
                self.doc.anchors.push(Anchor {
                    slug,
                    leaf: LeafId::new(last),
                });
            }
        }
        self.doc.index_anchors();
        self.doc
    }

    /// A builder for inline content, carrying the HTML styles and link open at this point.
    pub(crate) fn inline_builder(&self) -> InlineBuilder {
        InlineBuilder {
            html: self.html.state(),
            ..InlineBuilder::default()
        }
    }

    // Containers.

    pub(crate) fn begin_container(&mut self, container: Container) -> ContainerToken {
        self.flush_pending();
        // A summary goes before anything nested in its `<details>`.
        self.default_summary();
        let token = ContainerToken(self.open.len());
        let id = ContextId::new(self.doc.contexts.len());
        self.doc.contexts.push(Context {
            container,
            parent: self.open.last().copied(),
            first_leaf: None,
        });
        self.open.push(id);
        token
    }

    /// Closes a container, and anything opened inside it that is still open (an unclosed
    /// `<details>` in a list item ends with the item).
    pub(crate) fn end_container(&mut self, token: ContainerToken) {
        self.flush_pending();
        self.close_containers_from(token.0);
    }

    /// Gives a container its first leaf if it has none, so its marker still shows: a list item
    /// whose only content was an HTML comment, or nothing.
    pub(crate) fn ensure_started(&mut self, token: ContainerToken) {
        self.flush_pending();
        if self.open.len() > token.0 && self.started <= token.0 {
            self.push_leaf(LeafKind::Paragraph(Inlines::default()));
        }
    }

    pub(crate) fn begin_alert(&mut self, kind: AlertKind) -> ContainerToken {
        let token = self.begin_container(Container::Alert(kind));
        self.push_leaf(LeafKind::AlertTitle(kind));
        token
    }

    pub(crate) fn add_footnote(&mut self, label: &str, number: usize) -> FootnoteId {
        let id = FootnoteId::new(self.doc.footnotes.len());
        self.doc.footnotes.push(Footnote {
            label: label.to_owned(),
            number,
        });
        id
    }

    // Leaves.

    /// A paragraph. Inside an open `<summary>` it becomes part of the summary. An empty one is
    /// dropped (a list item or footnote gets its marker leaf from `ensure_started`).
    pub(crate) fn paragraph(&mut self, inlines: InlineBuilder) {
        self.flush_pending();
        let inlines = self.finish_inlines(inlines);
        if self.collected(&inlines) {
            return;
        }
        if !inlines.text.is_empty() {
            self.push_paragraph(inlines);
        }
    }

    /// A heading; `slug_text` is the text GitHub derives its anchor from.
    pub(crate) fn heading(&mut self, level: HeadingLevel, inlines: InlineBuilder, slug_text: &str) {
        self.flush_pending();
        let inlines = self.finish_inlines(inlines);
        if self.collected(&inlines) {
            return;
        }
        self.push_heading(level, inlines, slug_text);
    }

    pub(crate) fn thematic_break(&mut self) {
        self.flush_pending();
        self.push_leaf(LeafKind::ThematicBreak);
    }

    /// A code block, split into leaves of at most `CODE_CHUNK_LINES` lines and (unless one line
    /// is longer) `LEAF_TEXT_BYTES` bytes. `text` is the block's literal: each line ends in a
    /// newline, so "" has no lines and "\n" has one empty line.
    pub(crate) fn code_block(&mut self, info: &str, text: &str) {
        self.flush_pending();
        let body = text.strip_suffix('\n').unwrap_or(text);
        let mut line_starts = Vec::new();
        if !text.is_empty() {
            line_starts.push(ByteOffset::new(0));
            line_starts.extend(
                body.match_indices('\n')
                    .map(|(at, _newline)| ByteOffset::new(at + 1)),
            );
        }
        let block = CodeBlockId::new(self.doc.code_blocks.len());
        let chunks = code_chunks(&line_starts, body.len());
        self.doc.code_blocks.push(CodeBlock {
            info: info.to_owned(),
            text: body.to_owned(),
            line_starts,
        });
        for (start, end) in chunks {
            self.push_leaf(LeafKind::Code {
                block,
                lines: LineNo::new(start)..LineNo::new(end),
            });
        }
    }

    /// A table, split into leaves of at most `TABLE_CHUNK_ROWS` body rows; the first carries
    /// the header.
    pub(crate) fn table(&mut self, table: Table) {
        self.flush_pending();
        let rows = table.rows.len();
        let id = TableId::new(self.doc.tables.len());
        self.doc.tables.push(table);
        let count = rows.div_ceil(TABLE_CHUNK_ROWS).max(1);
        for chunk in 0..count {
            let start = chunk * TABLE_CHUNK_ROWS;
            self.push_leaf(LeafKind::TableRows {
                table: id,
                rows: RowNo::new(start)..RowNo::new((start + TABLE_CHUNK_ROWS).min(rows)),
                with_header: chunk == 0,
            });
        }
    }

    /// YAML front matter (`body` without its `---` lines): a table of keys and values, as GitHub
    /// shows it, when it is flat; otherwise the YAML as a code block.
    pub(crate) fn front_matter(&mut self, body: &str) {
        let Some(rows) = crate::front_matter::rows(body) else {
            let body = body.trim_end_matches('\n');
            let literal = if body.is_empty() {
                String::new()
            } else {
                format!("{body}\n")
            };
            self.code_block("yaml", &literal);
            return;
        };
        let cell = |text: &str, style: Style| {
            let mut inlines = InlineBuilder::default();
            let token = inlines.begin_style(style);
            inlines.text(text);
            inlines.end_style(token);
            inlines.finish()
        };
        self.table(Table {
            alignments: vec![Align::Start, Align::Start],
            header: Vec::new(),
            rows: rows
                .iter()
                .map(|(key, value)| vec![cell(key, Style::STRONG), cell(value, Style::PLAIN)])
                .collect(),
        });
    }

    /// Ends inline content: closes HTML tags opened inside it and returns the text.
    pub(crate) fn finish_inlines(&mut self, inlines: InlineBuilder) -> Inlines {
        self.html.close_inline_scope();
        let mut inlines = inlines.finish();
        trim_end(&mut inlines);
        inlines
    }

    // Links and images.

    /// A link, or `None` where GitHub would drop it (see `link::classify`): its text then
    /// shows as plain text.
    pub(crate) fn add_link(&mut self, destination: &str, title: &str) -> Option<LinkId> {
        let target = classify(destination, Use::Link)?;
        let id = LinkId::new(self.doc.links.len());
        self.doc.links.push(Link {
            target,
            title: non_empty(title),
        });
        Some(id)
    }

    pub(crate) fn add_footnote_link(&mut self, footnote: FootnoteId) -> LinkId {
        let id = LinkId::new(self.doc.links.len());
        self.doc.links.push(Link {
            target: LinkTarget::Footnote(footnote),
            title: None,
        });
        id
    }

    pub(crate) fn markdown_image(
        &mut self,
        destination: &str,
        title: &str,
        alt: String,
    ) -> ImageId {
        self.add_image(Image {
            source: classify(destination, Use::Image),
            alt,
            title: non_empty(title),
            width: None,
            height: None,
        })
    }

    fn add_image(&mut self, image: Image) -> ImageId {
        let id = ImageId::new(self.doc.images.len());
        self.doc.images.push(image);
        id
    }

    // HTML.

    /// Raw HTML inside a paragraph, heading or cell: one tag per comrak node. Tags opened here
    /// close with the paragraph; closing tags close whatever they match, including a `</div>`
    /// or `</details>` opened by an earlier HTML block.
    pub(crate) fn html_inline(&mut self, inlines: &mut InlineBuilder, html: &str) {
        for token in tokens(html) {
            match &token {
                HtmlToken::Text(text) => inlines.text(text),
                HtmlToken::Start {
                    name,
                    self_closing,
                    source,
                    ..
                } => {
                    if is_filtered(name) {
                        inlines.text(&collapse_whitespace(source));
                    } else if let Some(void) = self.void_element(name, &token) {
                        match void {
                            Void::Break => inlines.line_break(),
                            Void::Object(object) => inlines.object(object),
                        }
                    } else if matches!(name.as_str(), "video" | "audio") {
                        let link = self.media_link(&token);
                        media_label(inlines, name, link);
                        if !self_closing {
                            self.push_html(name, Effect::Hidden, Scope::Inline);
                        }
                    } else if name == "summary" {
                        // A summary opened inside a paragraph collects the text after it.
                        self.open_summary();
                        inlines.summary_from = Some(inlines.text.len());
                    } else if !self_closing && let Some(effect) = self.inline_effect(name, &token) {
                        self.push_html(name, effect, Scope::Inline);
                    }
                }
                HtmlToken::End { name, source } => {
                    if is_filtered(name) {
                        inlines.text(&collapse_whitespace(source));
                    } else if name == "summary" {
                        if let Some(from) = inlines.summary_from.take() {
                            let tail = inlines.split_off(from);
                            if let Some((_details, summary)) = self.summary.as_mut() {
                                summary.append(&tail, ' ');
                            }
                        }
                        self.flush_summary();
                    } else {
                        self.close_html(name);
                    }
                }
            }
            inlines.html = self.html.state();
        }
    }

    /// A raw HTML block: tags become leaves, styles, links, images, alignment, quotes and
    /// `<details>`.
    pub(crate) fn html_block(&mut self, html: &str) {
        for token in tokens(html) {
            if self.pre.is_some() {
                self.pre_token(&token);
                continue;
            }
            match &token {
                HtmlToken::Text(text) => self.html_text(text),
                HtmlToken::Start {
                    name,
                    self_closing,
                    source,
                    ..
                } => {
                    if is_filtered(name) {
                        self.pending_inlines().text(&collapse_whitespace(source));
                    } else {
                        self.html_start(name, &token, *self_closing);
                    }
                }
                HtmlToken::End { name, source } => {
                    if is_filtered(name) {
                        self.pending_inlines().text(&collapse_whitespace(source));
                    } else {
                        self.html_end(name);
                    }
                }
            }
        }
        // A <pre> left open ends with its block, so content after it stays after it.
        self.flush_pre();
        self.flush_pending();
    }

    fn html_start(&mut self, name: &str, token: &HtmlToken<'_>, self_closing: bool) {
        if name == "hr" {
            self.thematic_break();
            return;
        }
        if let Some(void) = self.void_element(name, token) {
            let inlines = self.pending_inlines();
            match void {
                Void::Break => inlines.line_break(),
                Void::Object(object) => inlines.object(object),
            }
            return;
        }
        if self_closing {
            return;
        }
        match name {
            "pre" => {
                self.flush_pending();
                self.pre = Some(String::new());
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.open_block(name, token);
                // Inside a summary, a heading is just the summary's text.
                if self.summary.is_none()
                    && let Some(level) = heading_level(name)
                {
                    self.pending = Some(Pending {
                        inlines: self.inline_builder(),
                        kind: PendingKind::Heading {
                            level,
                            slug: String::new(),
                        },
                    });
                }
            }
            // In a table cell, `<details>` and `<summary>` are lines of the cell: a cell holds
            // text, and a summary leaf would end the table.
            "details" | "summary" if self.in_cell() => self.open_block(name, token),
            "details" => {
                self.flush_pending();
                let id = DetailsId::new(self.next_details);
                self.next_details += 1;
                let open = token.has_attribute("open");
                let container = self.begin_container(Container::Details { id, open });
                self.push_html(name, Effect::Details { id, container }, Scope::Block);
                self.untitled = Some(id);
            }
            "summary" => {
                self.flush_pending();
                self.open_summary();
            }
            "blockquote" => {
                self.flush_pending();
                let container = self.begin_container(Container::Quote);
                self.push_html(name, Effect::Container(container), Scope::Block);
            }
            "table" => self.open_html_table(),
            "tr" => self.html_table_row(),
            "td" | "th" => self.html_table_cell(name == "th", token),
            "template" => self.push_html(name, Effect::Hidden, Scope::Block),
            "video" | "audio" => {
                let link = self.media_link(token);
                media_label(self.pending_inlines(), name, link);
                self.push_html(name, Effect::Hidden, Scope::Block);
                self.refresh_pending_state();
            }
            name if is_block(name) => self.open_block(name, token),
            name => {
                if let Some(effect) = self.inline_effect(name, token) {
                    self.push_html(name, effect, Scope::Block);
                    self.refresh_pending_state();
                }
            }
        }
    }

    fn html_end(&mut self, name: &str) {
        match name {
            "summary" => {
                self.flush_summary();
                // In a table cell, `<summary>` was opened as a line of the cell.
                self.close_html(name);
            }
            "table" => self.close_html_table(),
            "tr" => self.end_html_table_row(),
            "td" | "th" => self.end_html_table_cell(),
            "pre" => {}
            name => {
                let block = is_block(name)
                    || heading_level(name).is_some()
                    || matches!(name, "details" | "summary");
                if block {
                    self.flush_pending();
                }
                self.close_html(name);
                self.refresh_pending_state();
            }
        }
    }

    fn refresh_pending_state(&mut self) {
        let state = self.html.state();
        if let Some(pending) = self.pending.as_mut() {
            pending.inlines.html = state;
        }
    }

    fn html_text(&mut self, text: &str) {
        if let Some(Pending {
            kind: PendingKind::Heading { slug, .. },
            ..
        }) = self.pending.as_mut()
        {
            slug.push_str(text);
        }
        // HTML collapses whitespace; text that is only whitespace between block tags is nothing.
        let collapsed = collapse_whitespace(text);
        let at_start = self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.inlines.ends_with_space());
        let collapsed = if at_start {
            collapsed.trim_start()
        } else {
            collapsed.as_str()
        };
        if !collapsed.is_empty() {
            self.pending_inlines().text(collapsed);
        }
    }

    /// Inside a raw `<pre>`: text is kept as written, tags inside are dropped, `</pre>` ends it.
    fn pre_token(&mut self, token: &HtmlToken<'_>) {
        match token {
            HtmlToken::Text(text) => {
                if let Some(pre) = self.pre.as_mut() {
                    pre.push_str(text);
                }
            }
            HtmlToken::End { name, .. } if name == "pre" => self.flush_pre(),
            HtmlToken::Start { .. } | HtmlToken::End { .. } => {}
        }
    }

    /// Ends a raw `<pre>` as a code block. HTML ignores a newline right after `<pre>`.
    fn flush_pre(&mut self) {
        if let Some(text) = self.pre.take() {
            let text = text.strip_prefix('\n').unwrap_or(&text);
            let mut literal = text.trim_end_matches('\n').to_owned();
            if !literal.is_empty() {
                literal.push('\n');
            }
            self.code_block("", &literal);
        }
    }

    fn open_summary(&mut self) {
        self.flush_summary();
        if let Some(details) = self.html.details() {
            if self.untitled == Some(details) {
                self.untitled = None;
            }
            self.summary = Some((details, self.inline_builder()));
        }
    }

    /// Gives a `<details>` still waiting for its summary the one a browser shows.
    fn default_summary(&mut self) {
        if let Some(details) = self.untitled.take() {
            let mut inlines = InlineBuilder::default();
            inlines.text("Details");
            self.push_leaf(LeafKind::DetailsSummary {
                details,
                inlines: inlines.finish(),
            });
        }
    }

    fn flush_summary(&mut self) {
        self.flush_pending();
        if let Some((details, inlines)) = self.summary.take() {
            let mut inlines = inlines.finish();
            trim_end(&mut inlines);
            self.push_leaf(LeafKind::DetailsSummary { details, inlines });
        }
    }

    fn void_element(&mut self, name: &str, token: &HtmlToken<'_>) -> Option<Void> {
        match name {
            "br" => Some(Void::Break),
            "img" => html_image(token).map(|image| {
                let id = self.add_image(image);
                Void::Object(InlineObject::Image(id))
            }),
            _ => None,
        }
    }

    /// The link to a `<video>` or `<audio>` element's source, if it has one.
    fn media_link(&mut self, token: &HtmlToken<'_>) -> Option<LinkId> {
        let source = token.attribute("src").unwrap_or("");
        if source.is_empty() {
            return None;
        }
        self.add_link(source, "")
    }

    fn inline_effect(&mut self, name: &str, token: &HtmlToken<'_>) -> Option<Effect> {
        if name == "a" {
            // A named place a `#fragment` link can go to. Stored without GitHub's
            // `user-content-` prefix, as headings' slugs are, so `#x` and `#user-content-x`
            // both reach `<a id="x">` and `<a id="user-content-x">`.
            for attribute in ["id", "name"] {
                if let Some(value) = token.attribute(attribute) {
                    let name = crate::model::anchor_name(value);
                    if !name.is_empty() {
                        self.pending_anchors.push(name.to_owned());
                    }
                }
            }
            let link = token
                .attribute("href")
                .and_then(|href| self.add_link(href, token.attribute("title").unwrap_or("")));
            return Some(Effect::Link(link));
        }
        html_style(name).map(Effect::Style)
    }

    fn open_block(&mut self, name: &str, token: &HtmlToken<'_>) {
        self.flush_pending();
        let align = match (name, token.attribute("align")) {
            ("center", _) => Some(Align::Center),
            (_, Some(value)) => parse_align(value),
            (_, None) => None,
        };
        self.push_html(
            name,
            align.map_or(Effect::None, Effect::Align),
            Scope::Block,
        );
    }

    fn push_html(&mut self, name: &str, effect: Effect, scope: Scope) {
        if !self.html.push(name, effect, scope, self.open.len()) {
            self.notice(Notice::HtmlTooDeep);
        }
    }

    fn close_html(&mut self, name: &str) {
        let closed = self.html.close(name);
        if closed.details {
            self.flush_summary();
            self.default_summary();
        }
        // Innermost first: each closes now if nothing opened after it is still open, or else
        // once the Markdown containers inside it close.
        for token in closed.containers {
            if self.open.len() == token.0 + 1 {
                self.open.truncate(token.0);
            } else if self.open.len() > token.0 {
                self.deferred.push(token);
            }
        }
        self.started = self.started.min(self.open.len());
    }

    fn close_containers_from(&mut self, index: usize) {
        if self.html.details_from(index) {
            self.flush_summary();
            self.default_summary();
        }
        self.open.truncate(index);
        self.html.drop_deeper_than(index);
        // Deferred HTML closes whose inner Markdown containers have now all closed.
        while let Some(&token) = self.deferred.last() {
            if token.0 >= self.open.len() {
                self.deferred.pop();
            } else if token.0 + 1 == self.open.len() {
                self.open.truncate(token.0);
                self.deferred.pop();
            } else {
                break;
            }
        }
        self.started = self.started.min(self.open.len());
    }

    fn pending_inlines(&mut self) -> &mut InlineBuilder {
        let html = self.html.state();
        &mut self
            .pending
            .get_or_insert_with(|| Pending {
                inlines: InlineBuilder {
                    html,
                    ..InlineBuilder::default()
                },
                kind: PendingKind::Paragraph,
            })
            .inlines
    }

    fn flush_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let mut inlines = pending.inlines.finish();
        trim_end(&mut inlines);
        if self.collected(&inlines) {
            return;
        }
        match pending.kind {
            PendingKind::Paragraph => {
                if !inlines.text.is_empty() {
                    self.push_paragraph(inlines);
                }
            }
            PendingKind::Heading { level, slug } => self.push_heading(level, inlines, &slug),
        }
    }

    // Raw HTML tables: rows of cells, each cell's text in lines (its paragraphs, `<br>`s and
    // block tags). A table inside a cell reads as text in that cell.

    fn open_html_table(&mut self) {
        if let Some(table) = self.html_table.as_mut() {
            table.nested += 1;
            return;
        }
        self.flush_pending();
        self.html_table = Some(HtmlTable::default());
    }

    /// `</table>`: ends a nested table, or the table; with no table open, ends the text before
    /// it, as any block tag does.
    fn close_html_table(&mut self) {
        match self.html_table.as_mut() {
            None => self.flush_pending(),
            Some(table) if table.nested > 0 => table.nested -= 1,
            Some(_) => self.finish_html_table(),
        }
    }

    fn html_table_row(&mut self) {
        match self.html_table.as_ref().map(|table| table.nested) {
            None => {}
            Some(0) => {
                self.end_html_table_row();
                if let Some(table) = self.html_table.as_mut() {
                    table.row = Some(HtmlRow::default());
                }
            }
            Some(_nested) => self.separate_words(),
        }
    }

    fn html_table_cell(&mut self, header: bool, token: &HtmlToken<'_>) {
        match self.html_table.as_ref().map(|table| table.nested) {
            // A cell outside any table: its text follows the text before it.
            None => self.separate_words(),
            Some(0) => {
                self.end_html_table_cell();
                // Text in the table outside any cell shows before the table.
                self.flush_pending();
                if let Some(table) = self.html_table.as_mut() {
                    table.cell = Some(HtmlCell {
                        inlines: InlineBuilder::default(),
                        header,
                        align: token.attribute("align").and_then(parse_align),
                        span: token
                            .attribute("colspan")
                            .and_then(|span| span.trim().parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, MAX_COLSPAN),
                    });
                }
            }
            Some(_nested) => self.separate_words(),
        }
    }

    /// A space after the text being read, unless there is none or it ends in one.
    fn in_cell(&self) -> bool {
        self.html_table
            .as_ref()
            .is_some_and(|table| table.cell.is_some())
    }

    fn separate_words(&mut self) {
        if let Some(pending) = self.pending.as_mut()
            && !pending.inlines.ends_with_space()
        {
            pending.inlines.text(" ");
        }
    }

    fn end_html_table_cell(&mut self) {
        if self
            .html_table
            .as_ref()
            .is_none_or(|table| table.nested > 0)
        {
            return;
        }
        self.flush_pending();
        if let Some(table) = self.html_table.as_mut()
            && let Some(cell) = table.cell.take()
        {
            let mut inlines = cell.inlines.finish();
            trim_end(&mut inlines);
            let row = table.row.get_or_insert_with(HtmlRow::default);
            row.cells.push(inlines);
            row.headers.push(cell.header);
            row.aligns.push(cell.align);
            // A cell spanning columns: empty cells after it keep the columns after it in place.
            for _spanned in 1..cell.span {
                row.cells.push(Inlines::default());
                row.headers.push(cell.header);
                row.aligns.push(None);
            }
        }
    }

    /// `</tr>`: ends the row; with no table open, ends the text before it, as a block tag.
    fn end_html_table_row(&mut self) {
        match self.html_table.as_ref().map(|table| table.nested) {
            None => {
                self.flush_pending();
                return;
            }
            Some(0) => {}
            Some(_nested) => return,
        }
        self.end_html_table_cell();
        if let Some(table) = self.html_table.as_mut()
            && let Some(row) = table.row.take()
            && !row.cells.is_empty()
        {
            table.rows.push(row);
        }
    }

    /// Ends the open HTML table (and anything open in it) as a table leaf. A first row of only
    /// `<th>` cells is its header; each column takes the first `align` given in it.
    fn finish_html_table(&mut self) {
        if let Some(table) = self.html_table.as_mut() {
            table.nested = 0;
        }
        self.end_html_table_row();
        let Some(HtmlTable { mut rows, .. }) = self.html_table.take() else {
            return;
        };
        let columns = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let alignments = (0..columns)
            .map(|column| {
                rows.iter()
                    .find_map(|row| row.aligns.get(column).copied().flatten())
                    .unwrap_or_default()
            })
            .collect();
        let has_header = rows
            .first()
            .is_some_and(|row| row.headers.iter().all(|&header| header));
        let pad = |mut cells: Vec<Inlines>| {
            cells.resize_with(columns, Inlines::default);
            cells
        };
        let header = if has_header && !rows.is_empty() {
            pad(rows.remove(0).cells)
        } else {
            Vec::new()
        };
        self.table(Table {
            alignments,
            header,
            rows: rows.into_iter().map(|row| pad(row.cells)).collect(),
        });
    }

    /// Whether text that would be a paragraph or heading belongs to something collecting it
    /// instead: an open table cell (as a line of the cell) or `<summary>` (joined by a space).
    fn collected(&mut self, inlines: &Inlines) -> bool {
        if let Some(cell) = self
            .html_table
            .as_mut()
            .and_then(|table| table.cell.as_mut())
        {
            cell.inlines.append(inlines, '\n');
            return true;
        }
        if let Some((_details, summary)) = self.summary.as_mut() {
            summary.append(inlines, ' ');
            return true;
        }
        false
    }

    fn push_heading(&mut self, level: HeadingLevel, inlines: Inlines, slug_text: &str) {
        // The anchor goes in first, so the heading can name it; its leaf is known once pushed
        // (a default summary or raw anchors may come before).
        let anchor = AnchorId::new(self.doc.anchors.len());
        let slug = (self.slugger)(slug_text);
        self.doc.anchors.push(Anchor {
            slug,
            leaf: LeafId::new(self.doc.leaves.len()),
        });
        let leaf = self.push_leaf(LeafKind::Heading {
            level,
            inlines,
            anchor,
        });
        if let Some(heading) = self.doc.anchors.get_mut(anchor.index()) {
            heading.leaf = leaf;
        }
    }

    /// A paragraph, as one leaf or, past `LEAF_TEXT_BYTES`, several.
    fn push_paragraph(&mut self, inlines: Inlines) {
        for part in split_inlines(inlines, LEAF_TEXT_BYTES) {
            self.push_leaf(LeafKind::Paragraph(part));
        }
    }

    fn push_leaf(&mut self, kind: LeafKind) -> LeafId {
        // Raw anchors belong to this leaf, not to a table or summary pushed before it here.
        let anchors = std::mem::take(&mut self.pending_anchors);
        // A cell holds lines of text; anything else in it (a code block, a list) ends the table
        // there. Text outside any cell shows before the table, as a browser shows it.
        if self
            .html_table
            .as_ref()
            .is_some_and(|table| table.cell.is_some())
        {
            self.finish_html_table();
        }
        if !matches!(kind, LeafKind::DetailsSummary { .. }) {
            self.default_summary();
        }
        let id = LeafId::new(self.doc.leaves.len());
        for context in self.open.get(self.started..).unwrap_or_default() {
            if let Some(context) = self.doc.contexts.get_mut(context.index()) {
                context.first_leaf = Some(id);
            }
        }
        self.started = self.open.len();
        self.doc.leaves.push(Leaf {
            kind,
            context: self.open.last().copied(),
            align: self.html.align(),
        });
        for slug in anchors {
            self.doc.anchors.push(Anchor { slug, leaf: id });
        }
        id
    }
}

enum Void {
    Break,
    Object(InlineObject),
}

#[derive(Default)]
struct HtmlTable {
    rows: Vec<HtmlRow>,
    row: Option<HtmlRow>,
    cell: Option<HtmlCell>,
    /// Tables opened inside a cell and not yet closed.
    nested: usize,
}

#[derive(Default)]
struct HtmlRow {
    cells: Vec<Inlines>,
    /// Per cell: whether it is a `<th>`, and its `align`.
    headers: Vec<bool>,
    aligns: Vec<Option<Align>>,
}

struct HtmlCell {
    /// The cell's lines so far; text being read is in `Builder::pending` until a block tag or
    /// the cell's end adds it here.
    inlines: InlineBuilder,
    header: bool,
    align: Option<Align>,
    /// `colspan`: how many columns it covers.
    span: usize,
}

/// Media cannot play here: a link to it takes its place.
fn media_label(inlines: &mut InlineBuilder, name: &str, link: Option<LinkId>) {
    let label = if name == "video" {
        "▶ video"
    } else {
        "▶ audio"
    };
    let link_token = link.map(|link| inlines.begin_link(link));
    inlines.text(label);
    if let Some(link_token) = link_token {
        inlines.end_link(link_token);
    }
}

/// Line ranges for code chunks: at most `CODE_CHUNK_LINES` lines and, unless a single line is
/// longer, `LEAF_TEXT_BYTES` bytes. One empty range for a block with no lines.
fn code_chunks(line_starts: &[ByteOffset], text_len: usize) -> Vec<(usize, usize)> {
    let lines = line_starts.len();
    if lines == 0 {
        return vec![(0, 0)];
    }
    let start_of = |line: usize| {
        line_starts
            .get(line)
            .map_or(text_len, |offset| offset.index())
    };
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < lines {
        let mut end = start + 1;
        while end < lines
            && end - start < CODE_CHUNK_LINES
            && start_of(end + 1) - start_of(start) <= LEAF_TEXT_BYTES
        {
            end += 1;
        }
        chunks.push((start, end));
        start = end;
    }
    chunks
}

/// Splits inlines longer than `max` bytes into parts, each cut after a space or newline where
/// one falls in the second half of the part, else at a character boundary.
fn split_inlines(inlines: Inlines, max: usize) -> Vec<Inlines> {
    if inlines.text.len() <= max {
        return vec![inlines];
    }
    let mut cuts = Vec::new();
    let mut start = 0;
    while inlines.text.len() - start > max {
        let limit = floor_char_boundary(&inlines.text, start + max);
        let window = inlines.text.get(start..limit).unwrap_or_default();
        let cut = window
            .rfind([' ', '\n'])
            .map(|at| start + at + 1)
            .filter(|&cut| cut > start + max / 2)
            .unwrap_or(limit);
        cuts.push(cut);
        start = cut;
    }
    let mut parts = Vec::with_capacity(cuts.len() + 1);
    let mut from = 0;
    for to in cuts.into_iter().chain([inlines.text.len()]) {
        parts.push(slice_inlines(&inlines, from, to));
        from = to;
    }
    parts
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The part of `inlines` in `from..to`, offsets rebased to 0.
fn slice_inlines(inlines: &Inlines, from: usize, to: usize) -> Inlines {
    let rebase = |at: usize| ByteOffset::new(at.clamp(from, to) - from);
    Inlines {
        text: inlines.text.get(from..to).unwrap_or_default().to_owned(),
        spans: inlines
            .spans
            .iter()
            .filter(|span| span.range.start.index() < to && span.range.end.index() > from)
            .map(|span| Span {
                range: rebase(span.range.start.index())..rebase(span.range.end.index()),
                style: span.style,
                link: span.link,
            })
            .collect(),
        objects: inlines
            .objects
            .iter()
            .filter(|(at, _)| (from..to).contains(&at.index()))
            .map(|(at, object)| (rebase(at.index()), *object))
            .collect(),
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

fn heading_level(name: &str) -> Option<HeadingLevel> {
    match name {
        "h1" => Some(HeadingLevel::H1),
        "h2" => Some(HeadingLevel::H2),
        "h3" => Some(HeadingLevel::H3),
        "h4" => Some(HeadingLevel::H4),
        "h5" => Some(HeadingLevel::H5),
        "h6" => Some(HeadingLevel::H6),
        _ => None,
    }
}

/// Tags that end the text before them and start new text after.
fn is_block(name: &str) -> bool {
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
            | "address"
            | "picture"
    )
}

fn html_style(name: &str) -> Option<Style> {
    match name {
        "b" | "strong" => Some(Style::STRONG),
        "i" | "em" | "cite" | "dfn" | "var" => Some(Style::EMPHASIS),
        "code" | "tt" | "samp" => Some(Style::CODE),
        "kbd" => Some(Style::KEYBOARD),
        "sub" => Some(Style::SUBSCRIPT),
        "sup" => Some(Style::SUPERSCRIPT),
        "del" | "s" | "strike" => Some(Style::STRIKETHROUGH),
        "ins" | "u" => Some(Style::UNDERLINE),
        "mark" => Some(Style::HIGHLIGHT),
        _ => None,
    }
}

fn parse_align(value: &str) -> Option<Align> {
    match value.trim().to_ascii_lowercase().as_str() {
        "center" | "middle" => Some(Align::Center),
        "right" => Some(Align::End),
        "left" => Some(Align::Start),
        _ => None,
    }
}

fn html_image(token: &HtmlToken<'_>) -> Option<Image> {
    let source = token.attribute("src")?;
    let size = |name: &str| {
        token
            .attribute(name)
            .and_then(|value| value.trim().trim_end_matches("px").parse::<u32>().ok())
    };
    Some(Image {
        source: classify(source, Use::Image),
        alt: token.attribute("alt").unwrap_or("").to_owned(),
        title: token.attribute("title").and_then(non_empty),
        width: size("width"),
        height: size("height"),
    })
}

fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for c in text.chars() {
        if c.is_ascii_whitespace() {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// Drops trailing spaces and one final line break, and clamps spans to the shortened text.
///
/// A browser ends a block's last line at its last `<br>` without starting another, so a single
/// trailing `<br>` shows nothing while each further one is an empty line: "a<br><br>" is "a" and
/// one empty line, which is "a\n" laid out.
fn trim_end(inlines: &mut Inlines) {
    let trimmed = inlines.text.trim_end_matches(' ');
    let kept = trimmed
        .strip_suffix('\n')
        .map_or(trimmed, |before| before.trim_end_matches(' '))
        .len();
    inlines.text.truncate(kept);
    for span in &mut inlines.spans {
        if span.range.end.index() > kept {
            span.range.end = ByteOffset::new(kept);
        }
    }
    inlines
        .spans
        .retain(|span| span.range.start < span.range.end);
}

// The HTML tag stack.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Opened in an HTML block: open until closed.
    Block,
    /// Opened in a Markdown paragraph: closes with it.
    Inline,
}

#[derive(Debug, Clone, Copy)]
enum Effect {
    None,
    Align(Align),
    Style(Style),
    /// A link, or `None` for an `<a>` with no `href`, which still has to be closed.
    Link(Option<LinkId>),
    Details {
        id: DetailsId,
        container: ContainerToken,
    },
    Container(ContainerToken),
    /// Content never shown (`<template>`, the inside of `<video>`).
    Hidden,
}

#[derive(Debug, Clone, Copy, Default)]
struct Derived {
    align: Align,
    state: HtmlState,
    details: Option<DetailsId>,
}

struct HtmlEntry {
    name: String,
    effect: Effect,
    scope: Scope,
    /// `Builder::open.len()` when the tag opened: closing Markdown containers below this ends it.
    depth: usize,
    /// The state with this tag and all below it applied.
    derived: Derived,
}

#[derive(Default)]
struct HtmlStack {
    entries: Vec<HtmlEntry>,
    /// Open entries per tag name, so a closing tag with no opener costs O(1).
    open_by_name: HashMap<String, usize>,
}

#[derive(Default)]
struct Closed {
    /// A `<details>` was closed, so an open summary must end.
    details: bool,
    /// Containers the closed tags had opened, innermost first.
    containers: Vec<ContainerToken>,
}

impl HtmlStack {
    fn top(&self) -> Derived {
        self.entries
            .last()
            .map(|entry| entry.derived)
            .unwrap_or_default()
    }

    fn state(&self) -> HtmlState {
        self.top().state
    }

    fn align(&self) -> Align {
        self.top().align
    }

    fn details(&self) -> Option<DetailsId> {
        self.top().details
    }

    /// Pushes a tag; false (and nothing pushed) past `MAX_HTML_DEPTH`.
    fn push(&mut self, name: &str, effect: Effect, scope: Scope, depth: usize) -> bool {
        if self.entries.len() >= MAX_HTML_DEPTH {
            return false;
        }
        let mut derived = self.top();
        match effect {
            Effect::None | Effect::Container(_) => {}
            Effect::Align(align) => derived.align = align,
            Effect::Style(style) => derived.state.style = derived.state.style.union(style),
            Effect::Link(link) => derived.state.link = link.or(derived.state.link),
            Effect::Details { id, .. } => derived.details = Some(id),
            Effect::Hidden => derived.state.hidden = true,
        }
        *self.open_by_name.entry(name.to_owned()).or_insert(0) += 1;
        self.entries.push(HtmlEntry {
            name: name.to_owned(),
            effect,
            scope,
            depth,
            derived,
        });
        true
    }

    /// Closes the innermost open tag called `name` and every tag opened after it.
    fn close(&mut self, name: &str) -> Closed {
        let mut closed = Closed::default();
        if self.open_by_name.get(name).is_none_or(|&count| count == 0) {
            return closed;
        }
        while let Some(entry) = self.entries.pop() {
            self.forget(&entry.name);
            match entry.effect {
                Effect::Details { container, .. } => {
                    closed.details = true;
                    closed.containers.push(container);
                }
                Effect::Container(container) => closed.containers.push(container),
                Effect::None
                | Effect::Align(_)
                | Effect::Style(_)
                | Effect::Link(_)
                | Effect::Hidden => {}
            }
            if entry.name == name {
                break;
            }
        }
        closed
    }

    /// Closes the tags a Markdown paragraph opened (they are on top; inline HTML opens no
    /// containers).
    fn close_inline_scope(&mut self) {
        while self
            .entries
            .last()
            .is_some_and(|entry| entry.scope == Scope::Inline)
        {
            if let Some(entry) = self.entries.pop() {
                self.forget(&entry.name);
            }
        }
    }

    /// Drops tags opened inside Markdown containers that have just closed.
    fn drop_deeper_than(&mut self, depth: usize) {
        while self.entries.last().is_some_and(|entry| entry.depth > depth) {
            if let Some(entry) = self.entries.pop() {
                self.forget(&entry.name);
            }
        }
    }

    /// Whether a `<details>` whose container is at `index` or deeper is open.
    fn details_from(&self, index: usize) -> bool {
        self.entries.iter().rev().any(
            |entry| matches!(entry.effect, Effect::Details { container, .. } if container.0 >= index),
        )
    }

    fn forget(&mut self, name: &str) {
        if let Some(count) = self.open_by_name.get_mut(name) {
            *count = count.saturating_sub(1);
        }
    }
}

/// An open Markdown style, to be ended with `InlineBuilder::end_style`.
#[must_use = "a style must be ended with end_style"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StyleToken(u32);

/// An open Markdown link, to be ended with `InlineBuilder::end_link`.
#[must_use = "a link must be ended with end_link"]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LinkToken(u32);

/// Builds one `Inlines`: text with styled runs.
///
/// Markdown styles and links are opened and closed by token, each closing exactly what it
/// opened; HTML's are applied by the `Builder` through `html`. The two never close each other's,
/// so `*a <b>b* c</b>` shows "c" bold, as a browser would.
#[derive(Debug, Default)]
pub(crate) struct InlineBuilder {
    text: String,
    spans: Vec<Span>,
    objects: Vec<(ByteOffset, InlineObject)>,
    styles: Vec<(u32, Style)>,
    links: Vec<(u32, LinkId)>,
    next_token: u32,
    /// The union of `styles`, kept current.
    style: Style,
    html: HtmlState,
    /// Where an inline `<summary>` opened: the text after it belongs to the summary.
    summary_from: Option<usize>,
}

impl InlineBuilder {
    pub(crate) fn text(&mut self, text: &str) {
        if text.is_empty() || self.html.hidden {
            return;
        }
        let start = ByteOffset::end_of(&self.text);
        self.text.push_str(text);
        let end = ByteOffset::end_of(&self.text);
        let style = self.style.union(self.html.style);
        let link = self
            .links
            .last()
            .map(|&(_token, link)| link)
            .or(self.html.link);
        self.push_span(start..end, style, link);
    }

    fn push_span(
        &mut self,
        range: std::ops::Range<ByteOffset>,
        style: Style,
        link: Option<LinkId>,
    ) {
        if style.is_plain() && link.is_none() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.range.end == range.start
            && last.style == style
            && last.link == link
        {
            last.range.end = range.end;
            return;
        }
        self.spans.push(Span { range, style, link });
    }

    /// A hard line break. Spaces before it are dropped: a browser removes whitespace at the end
    /// of a line, so a link's underline does not run on past its text.
    pub(crate) fn line_break(&mut self) {
        if self.html.hidden {
            return;
        }
        let kept = self.text.trim_end_matches(' ').len();
        self.text.truncate(kept);
        for span in &mut self.spans {
            if span.range.end.index() > kept {
                span.range.end = ByteOffset::new(kept);
            }
        }
        self.spans.retain(|span| span.range.start < span.range.end);
        self.text("\n");
    }

    pub(crate) fn object(&mut self, object: InlineObject) {
        if self.html.hidden {
            return;
        }
        self.objects.push((ByteOffset::end_of(&self.text), object));
        let mut buffer = [0; 4];
        self.text(Inlines::OBJECT.encode_utf8(&mut buffer));
    }

    pub(crate) fn begin_style(&mut self, style: Style) -> StyleToken {
        let token = self.next_token();
        self.styles.push((token, style));
        self.style = self.style.union(style);
        StyleToken(token)
    }

    pub(crate) fn end_style(&mut self, token: StyleToken) {
        if let Some(at) = self.styles.iter().rposition(|&(id, _)| id == token.0) {
            self.styles.remove(at);
            self.style = self
                .styles
                .iter()
                .fold(Style::PLAIN, |all, &(_id, style)| all.union(style));
        }
    }

    pub(crate) fn begin_link(&mut self, link: LinkId) -> LinkToken {
        let token = self.next_token();
        self.links.push((token, link));
        LinkToken(token)
    }

    pub(crate) fn end_link(&mut self, token: LinkToken) {
        if let Some(at) = self.links.iter().rposition(|&(id, _)| id == token.0) {
            self.links.remove(at);
        }
    }

    fn next_token(&mut self) -> u32 {
        self.next_token = self.next_token.wrapping_add(1);
        self.next_token
    }

    /// Appends finished inlines (a paragraph joining a `<summary>` or a table cell), after
    /// `separator` unless the text so far is empty or already ends in whitespace.
    fn append(&mut self, other: &Inlines, separator: char) {
        if other.text.is_empty() {
            return;
        }
        if !self.text.is_empty() && !self.ends_with_space() {
            self.text.push(separator);
        }
        let base = self.text.len();
        self.text.push_str(&other.text);
        for span in &other.spans {
            let range = ByteOffset::new(base + span.range.start.index())
                ..ByteOffset::new(base + span.range.end.index());
            self.push_span(range, span.style, span.link);
        }
        self.objects.extend(
            other
                .objects
                .iter()
                .map(|(at, object)| (ByteOffset::new(base + at.index()), *object)),
        );
    }

    /// Removes and returns the text from byte `at` on, with its spans and objects.
    fn split_off(&mut self, at: usize) -> Inlines {
        let at = floor_char_boundary(&self.text, at.min(self.text.len()));
        let whole = Inlines {
            text: std::mem::take(&mut self.text),
            spans: std::mem::take(&mut self.spans),
            objects: std::mem::take(&mut self.objects),
        };
        let head = slice_inlines(&whole, 0, at);
        self.text = head.text;
        self.spans = head.spans;
        self.objects = head.objects;
        slice_inlines(&whole, at, whole.text.len())
    }

    fn ends_with_space(&self) -> bool {
        self.text.is_empty() || self.text.ends_with([' ', '\n'])
    }

    pub(crate) fn finish(self) -> Inlines {
        Inlines {
            text: self.text,
            spans: self.spans,
            objects: self.objects,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> Builder {
        Builder::new(Box::new(|text: &str| text.to_lowercase().replace(' ', "-")))
    }

    fn texts(doc: &Document) -> Vec<String> {
        doc.leaves
            .iter()
            .map(|leaf| match &leaf.kind {
                LeafKind::Paragraph(inlines)
                | LeafKind::Heading { inlines, .. }
                | LeafKind::DetailsSummary { inlines, .. } => inlines.text.clone(),
                LeafKind::Code { .. } => "<code>".to_owned(),
                LeafKind::TableRows { .. } => "<table>".to_owned(),
                LeafKind::ThematicBreak => "<hr>".to_owned(),
                LeafKind::AlertTitle(kind) => kind.title().to_owned(),
            })
            .collect()
    }

    fn containers(doc: &Document, leaf: usize) -> Vec<Container> {
        doc.leaves
            .get(leaf)
            .map(|leaf| doc.containers(leaf).map(|(_id, c)| c.container).collect())
            .unwrap_or_default()
    }

    fn paragraph(builder: &mut Builder, text: &str) {
        let mut inlines = builder.inline_builder();
        inlines.text(text);
        builder.paragraph(inlines);
    }

    #[test]
    fn styled_runs_merge_and_stay_plain_where_plain() {
        let mut inlines = InlineBuilder::default();
        inlines.text("a ");
        let strong = inlines.begin_style(Style::STRONG);
        inlines.text("b");
        inlines.text("c");
        let emphasis = inlines.begin_style(Style::EMPHASIS);
        inlines.text("d");
        inlines.end_style(emphasis);
        inlines.end_style(strong);
        inlines.text(" e");
        let inlines = inlines.finish();
        assert_eq!(inlines.text, "a bcd e");
        assert_eq!(
            inlines.spans,
            vec![
                Span {
                    range: ByteOffset::new(2)..ByteOffset::new(4),
                    style: Style::STRONG,
                    link: None
                },
                Span {
                    range: ByteOffset::new(4)..ByteOffset::new(5),
                    style: Style::STRONG.union(Style::EMPHASIS),
                    link: None
                },
            ]
        );
    }

    #[test]
    fn code_blocks_chunk_by_lines_and_bytes() {
        let mut builder = builder();
        let text = (0..450)
            .map(|line| format!("line {line}\n"))
            .collect::<Vec<_>>()
            .concat();
        builder.code_block("rust", &text);
        builder.code_block("", "");
        builder.code_block("", "\n");
        let wide = (0..10)
            .map(|_| format!("{}\n", "x".repeat(5000)))
            .collect::<Vec<_>>()
            .concat();
        builder.code_block("", &wide);
        let doc = builder.finish();
        let ranges: Vec<_> = doc
            .leaves
            .iter()
            .filter_map(|leaf| {
                if let LeafKind::Code { lines, .. } = &leaf.kind {
                    Some((lines.start.index(), lines.end.index()))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            ranges,
            vec![
                (0, 200),
                (200, 400),
                (400, 450),
                (0, 0),
                (0, 1),
                (0, 3),
                (3, 6),
                (6, 9),
                (9, 10)
            ]
        );
        assert_eq!(doc.code_blocks.get(1).map(CodeBlock::line_count), Some(0));
        assert_eq!(doc.code_blocks.get(2).map(CodeBlock::line_count), Some(1));
    }

    #[test]
    fn long_paragraphs_split_into_leaves_at_spaces() {
        let mut builder = builder();
        let mut inlines = builder.inline_builder();
        let strong = inlines.begin_style(Style::STRONG);
        inlines.text(&"word ".repeat(10_000));
        inlines.end_style(strong);
        builder.paragraph(inlines);
        let doc = builder.finish();
        assert!(doc.leaves.len() >= 3);
        let mut total = 0;
        for leaf in &doc.leaves {
            let LeafKind::Paragraph(inlines) = &leaf.kind else {
                panic!("paragraph expected");
            };
            assert!(inlines.text.len() <= LEAF_TEXT_BYTES);
            assert_eq!(inlines.spans.len(), 1);
            assert_eq!(
                inlines.spans.first().map(|span| span.range.end.index()),
                Some(inlines.text.len())
            );
            total += inlines.text.len();
        }
        assert_eq!(total, "word ".repeat(10_000).trim_end().len());
    }

    #[test]
    fn list_markers_go_on_the_first_leaf_only() {
        let mut builder = builder();
        let item = builder.begin_container(Container::ListItem {
            marker: crate::model::Marker::Bullet,
            tight: true,
        });
        paragraph(&mut builder, "one");
        paragraph(&mut builder, ""); // empty: dropped
        paragraph(&mut builder, "two");
        builder.end_container(item);
        let doc = builder.finish();
        let context = doc.contexts.first().expect("context");
        assert!(context.starts_at(LeafId::new(0)));
        assert!(!context.starts_at(LeafId::new(1)));
        assert_eq!(texts(&doc), vec!["one", "two"]);
    }

    #[test]
    fn an_item_with_no_content_still_gets_a_leaf() {
        let mut builder = builder();
        let item = builder.begin_container(Container::ListItem {
            marker: crate::model::Marker::Bullet,
            tight: true,
        });
        builder.html_block("<!-- only a comment -->");
        builder.ensure_started(item);
        builder.end_container(item);
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec![""]);
    }

    #[test]
    fn html_blocks_become_aligned_leaves() {
        let mut builder = builder();
        builder.html_block(
            "<p align=\"center\">\n  <img src=\"logo.png\" alt=\"Logo\" width=\"120px\">\n</p>",
        );
        builder.html_block("<h1 align=\"center\">My <b>Tool</b></h1>");
        builder.html_block("<!-- hidden -->");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["\u{FFFC}", "My Tool"]);
        assert!(doc.leaves.iter().all(|leaf| leaf.align == Align::Center));
        let image = doc.images.first().expect("image");
        assert_eq!(image.alt, "Logo");
        assert_eq!(image.width, Some(120));
        assert_eq!(
            doc.anchors.first().map(|a| a.slug.as_str()),
            Some("my-tool")
        );
    }

    #[test]
    fn filtered_tags_show_as_text() {
        let mut builder = builder();
        builder.html_block("<script>alert(1)</script>");
        let mut inlines = builder.inline_builder();
        inlines.text("a ");
        builder.html_inline(&mut inlines, "<style>");
        inlines.text("p{}");
        builder.html_inline(&mut inlines, "</style>");
        builder.paragraph(inlines);
        let doc = builder.finish();
        assert_eq!(
            texts(&doc),
            vec!["<script>alert(1)</script>", "a <style>p{}</style>"]
        );
    }

    #[test]
    fn alignment_and_details_span_html_blocks() {
        let mut builder = builder();
        builder.html_block("<div align=\"right\">");
        paragraph(&mut builder, "markdown inside");
        builder.html_block("</div>");
        builder.html_block("<details open>\n<summary>More</summary>");
        paragraph(&mut builder, "hidden");
        builder.html_block("</details>");
        paragraph(&mut builder, "after");
        let doc = builder.finish();
        assert_eq!(
            texts(&doc),
            vec!["markdown inside", "More", "hidden", "after"]
        );
        let aligns: Vec<Align> = doc.leaves.iter().map(|leaf| leaf.align).collect();
        assert_eq!(
            aligns,
            vec![Align::End, Align::Start, Align::Start, Align::Start]
        );
        let in_details: Vec<bool> = (0..4)
            .map(|leaf| {
                containers(&doc, leaf)
                    .iter()
                    .any(|c| matches!(c, Container::Details { open: true, .. }))
            })
            .collect();
        assert_eq!(in_details, vec![false, true, true, false]);
    }

    #[test]
    fn a_closing_tag_inside_a_paragraph_closes_block_html() {
        let mut builder = builder();
        builder.html_block("<details>\n<summary>More</summary>");
        let mut inlines = builder.inline_builder();
        inlines.text("hidden text");
        builder.html_inline(&mut inlines, "</details>");
        builder.paragraph(inlines);
        paragraph(&mut builder, "after");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["More", "hidden text", "after"]);
        assert_eq!(containers(&doc, 2), Vec::new());
    }

    #[test]
    fn markdown_containers_close_html_opened_inside_them() {
        let mut builder = builder();
        let item = builder.begin_container(Container::ListItem {
            marker: crate::model::Marker::Bullet,
            tight: false,
        });
        paragraph(&mut builder, "a");
        builder.html_block("<details>");
        paragraph(&mut builder, "hidden");
        builder.end_container(item);
        paragraph(&mut builder, "after");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["a", "Details", "hidden", "after"]);
        assert_eq!(containers(&doc, 3), Vec::new());
    }

    #[test]
    fn details_without_a_summary_get_the_default_one() {
        let mut builder = builder();
        builder.html_block("<details>");
        paragraph(&mut builder, "body");
        builder.html_block("</details>");
        builder.html_block("<details></details>");
        builder.html_block("<details>\n<details><summary>Inner</summary>\n</details>\n</details>");
        let doc = builder.finish();
        assert_eq!(
            texts(&doc),
            vec!["Details", "body", "Details", "Details", "Inner"]
        );
        let summaries: Vec<usize> = doc
            .leaves
            .iter()
            .filter_map(|leaf| {
                if let LeafKind::DetailsSummary { details, .. } = &leaf.kind {
                    Some(details.index())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(summaries, vec![0, 1, 2, 3]);
        assert_eq!(containers(&doc, 1).len(), 1);
    }

    fn table_cells(doc: &Document) -> Vec<Vec<String>> {
        doc.tables
            .iter()
            .flat_map(|table| {
                table.header.iter().map(|c| vec![c.text.clone()]).chain(
                    table
                        .rows
                        .iter()
                        .map(|row| row.iter().map(|c| c.text.clone()).collect()),
                )
            })
            .collect()
    }

    #[test]
    fn raw_html_tables_are_tables() {
        let mut builder = builder();
        builder.html_block(
            "<table>\n<tr><th>Name</th><th align=\"right\">Count</th></tr>\n\
             <tr><td align=\"center\"><img src=\"a.png\" alt=\"A\"><br><sub><b>Ann</b></sub></td>\
             <td>1</td></tr>\n<tr><td>Bob</td></tr>\n</table>",
        );
        paragraph(&mut builder, "after");
        let doc = builder.finish();
        let table = doc.tables.first().expect("a table");
        let header: Vec<&str> = table.header.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(header, vec!["Name", "Count"]);
        let rows: Vec<Vec<&str>> = table
            .rows
            .iter()
            .map(|row| row.iter().map(|c| c.text.as_str()).collect())
            .collect();
        assert_eq!(rows, vec![vec!["\u{FFFC}\nAnn", "1"], vec!["Bob", ""]]);
        assert_eq!(table.alignments, vec![Align::Center, Align::End]);
        let first = table
            .rows
            .first()
            .and_then(|row| row.first())
            .expect("cell");
        assert_eq!(first.objects.len(), 1);
        assert!(
            first
                .spans
                .iter()
                .any(|span| span.style.contains(Style::STRONG))
        );
        assert_eq!(texts(&doc).last().map(String::as_str), Some("after"));
        assert!(matches!(
            doc.leaves.first().map(|leaf| &leaf.kind),
            Some(LeafKind::TableRows {
                with_header: true,
                ..
            })
        ));
    }

    #[test]
    fn html_table_cells_take_markdown_lines_and_nested_tables_as_text() {
        let mut builder = builder();
        builder.html_block("<table><tr><td>");
        paragraph(&mut builder, "first");
        paragraph(&mut builder, "second");
        builder.html_block("</td><td><p>p one</p><div>div two</div></td>");
        builder.html_block("<td><table><tr><td>in</td><td>ner</td></tr></table></td></tr></table>");
        let doc = builder.finish();
        assert!(
            doc.tables
                .first()
                .is_some_and(|table| table.header.is_empty())
        );
        assert_eq!(
            table_cells(&doc),
            vec![vec!["first\nsecond", "p one\ndiv two", "in ner"]]
        );
        assert_eq!(doc.leaves.len(), 1);
    }

    #[test]
    fn details_in_a_cell_are_lines_of_it_and_colspan_keeps_columns() {
        let mut builder = builder();
        builder.html_block(
            "<table><tr><th colspan=\"2\">Sponsors</th></tr>\n\
             <tr><td><details><summary>More</summary>body</details></td><td>x</td></tr></table>",
        );
        let doc = builder.finish();
        let table = doc.tables.first().expect("one table");
        let header: Vec<&str> = table.header.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(header, vec!["Sponsors", ""]);
        let body: Vec<&str> = table
            .rows
            .first()
            .expect("a body row")
            .iter()
            .map(|c| c.text.as_str())
            .collect();
        assert_eq!(body, vec!["More\nbody", "x"]);
        assert_eq!(doc.leaves.len(), 1);
    }

    #[test]
    fn raw_anchors_name_their_own_leaf_and_trailing_ones_the_last() {
        let mut builder = builder();
        builder.html_block("<details>");
        builder.html_block("<a id=\"inside\"></a>");
        paragraph(&mut builder, "body");
        builder.html_block("</details>");
        builder.html_block("<a name=\"bottom\"></a>");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["Details", "body"]);
        assert_eq!(doc.anchor_leaf("inside"), Some(LeafId::new(1)));
        assert_eq!(doc.anchor_leaf("bottom"), Some(LeafId::new(1)));
    }

    #[test]
    fn rows_need_no_closing_tags_and_nested_tables_close_before_more_cells() {
        let mut builder = builder();
        builder.html_block("<table><tr><td>a<tr><td>b</table>");
        builder.html_block(
            "<table><tr><td><table><tr><td>in</td></tr></table></td><td>x</td></tr></table>",
        );
        builder.html_block("text</tr>more</table>end");
        let doc = builder.finish();
        let tables: Vec<Vec<Vec<&str>>> = doc
            .tables
            .iter()
            .map(|table| {
                table
                    .rows
                    .iter()
                    .map(|row| row.iter().map(|cell| cell.text.as_str()).collect())
                    .collect()
            })
            .collect();
        assert_eq!(
            tables,
            vec![vec![vec!["a"], vec!["b"]], vec![vec!["in", "x"]]]
        );
        // With no table open, </tr> and </table> end the text before them.
        assert_eq!(texts(&doc), vec!["<table>", "<table>", "text", "more", "end"]);
    }

    #[test]
    fn a_break_drops_styled_spaces_before_it_entirely() {
        let mut builder = builder();
        builder.html_block("<b>a</b><i> </i><br>b");
        let doc = builder.finish();
        let LeafKind::Paragraph(inlines) = &doc.leaves.first().expect("paragraph").kind else {
            panic!("a paragraph");
        };
        assert_eq!(inlines.text, "a\nb");
        assert!(
            inlines
                .spans
                .iter()
                .all(|span| span.range.start < span.range.end)
        );
        assert_eq!(inlines.spans.len(), 1);
    }

    #[test]
    fn html_tables_end_where_a_cell_cannot_hold_what_comes() {
        let mut builder = builder();
        builder.html_block("<table><caption>Caption</caption><tr><td>a");
        builder.code_block("", "code\n");
        builder.html_block("</td><td>b</td></tr></table>");
        builder.html_block("<table></table>");
        builder.html_block("<td>loose</td>");
        builder.html_block("</tr>");
        let doc = builder.finish();
        assert_eq!(table_cells(&doc), vec![vec!["a"]]);
        let kinds: Vec<&str> = doc
            .leaves
            .iter()
            .map(|leaf| match &leaf.kind {
                LeafKind::Paragraph(inlines) => inlines.text.as_str(),
                LeafKind::TableRows { .. } => "table",
                LeafKind::Code { .. } => "code",
                LeafKind::Heading { .. }
                | LeafKind::ThematicBreak
                | LeafKind::AlertTitle(_)
                | LeafKind::DetailsSummary { .. } => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["Caption", "table", "code", "b", "loose"]);
    }

    #[test]
    fn summaries_collect_until_closed() {
        let mut builder = builder();
        builder.html_block("<details>\n<summary>");
        let mut inlines = builder.inline_builder();
        let strong = inlines.begin_style(Style::STRONG);
        inlines.text("Bold");
        inlines.end_style(strong);
        inlines.text(" title");
        builder.paragraph(inlines);
        builder.html_block("</summary>");
        paragraph(&mut builder, "body");
        builder.html_block("<details><summary><h2>Heading summary</h2></summary>");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["Bold title", "body", "Heading summary"]);
    }

    #[test]
    fn a_link_around_html_blocks_covers_all_of_them() {
        let mut builder = builder();
        builder.html_block("<a href=\"https://warp.dev\">\n<div><img src=\"warp.png\"></div>");
        builder.html_block("<b>Warp</b></a>");
        paragraph(&mut builder, "after");
        let doc = builder.finish();
        let linked: Vec<bool> = doc
            .leaves
            .iter()
            .map(|leaf| match &leaf.kind {
                LeafKind::Paragraph(inlines) => inlines.spans.iter().any(|s| s.link.is_some()),
                LeafKind::Heading { .. }
                | LeafKind::Code { .. }
                | LeafKind::TableRows { .. }
                | LeafKind::ThematicBreak
                | LeafKind::AlertTitle(_)
                | LeafKind::DetailsSummary { .. } => false,
            })
            .collect();
        assert_eq!(texts(&doc), vec!["\u{FFFC}", "Warp", "after"]);
        assert_eq!(linked, vec![true, true, false]);
    }

    #[test]
    fn markdown_and_html_styles_close_independently() {
        let mut builder = builder();
        let mut inlines = builder.inline_builder();
        let emphasis = inlines.begin_style(Style::EMPHASIS);
        inlines.text("a ");
        builder.html_inline(&mut inlines, "<b>");
        inlines.text("b");
        inlines.end_style(emphasis);
        inlines.text(" c");
        builder.html_inline(&mut inlines, "</b>");
        let inlines = builder.finish_inlines(inlines);
        let style_at = |text: &str| {
            let at = inlines.text.find(text).expect("present");
            inlines
                .spans
                .iter()
                .find(|s| s.range.start.index() <= at && at < s.range.end.index())
                .map_or(Style::PLAIN, |s| s.style)
        };
        assert_eq!(style_at("a"), Style::EMPHASIS);
        assert_eq!(style_at("b"), Style::EMPHASIS.union(Style::STRONG));
        assert_eq!(style_at("c"), Style::STRONG);
    }

    #[test]
    fn raw_pre_keeps_its_text_and_raw_blockquote_quotes() {
        let mut builder = builder();
        builder.html_block("<pre>\n  ascii\n   art</pre>");
        builder.html_block("<blockquote>\nquoted\n</blockquote>");
        builder.html_block("<div>\n<pre>\ncode\n");
        paragraph(&mut builder, "more");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["<code>", "quoted", "<code>", "more"]);
        assert_eq!(
            doc.code_blocks.first().map(|block| block.text.as_str()),
            Some("  ascii\n   art")
        );
        assert_eq!(containers(&doc, 1), vec![Container::Quote]);
    }

    #[test]
    fn html_depth_is_capped() {
        let mut builder = builder();
        builder.html_block(&"<div>".repeat(1000));
        paragraph(&mut builder, "deep");
        builder.html_block(&"</span>".repeat(1000));
        let doc = builder.finish();
        assert_eq!(doc.notices, vec![Notice::HtmlTooDeep]);
        assert_eq!(texts(&doc), vec!["deep"]);
    }

    #[test]
    fn one_trailing_break_is_dropped_and_the_rest_are_empty_lines() {
        let mut builder = builder();
        builder.html_block("<p>Check out my app <br></p>");
        builder.html_block("<p>two <br> <br> </p>");
        builder.html_block("<div><br><br><br></div>");
        builder.html_block("<a href=\"x\">linked </a><br>after");
        let doc = builder.finish();
        assert_eq!(
            texts(&doc),
            vec!["Check out my app", "two\n", "\n\n", "linked\nafter"]
        );
        // The link's span ends with its text, not over the dropped space.
        let LeafKind::Paragraph(linked) = &doc.leaves.last().expect("linked").kind else {
            panic!("a paragraph");
        };
        assert_eq!(
            linked.spans.first().map(|span| span.range.end.index()),
            Some("linked".len())
        );
    }

    #[test]
    fn splitting_respects_char_boundaries_and_carries_spans_and_objects() {
        // No spaces, multi-byte characters: cut at the last char boundary before the limit.
        let text = "é".repeat(LEAF_TEXT_BYTES);
        let mut inlines = InlineBuilder::default();
        let strong = inlines.begin_style(Style::STRONG);
        inlines.text(&text);
        inlines.end_style(strong);
        inlines.object(InlineObject::Image(ImageId::new(0)));
        let parts = split_inlines(inlines.finish(), LEAF_TEXT_BYTES);
        assert_eq!(parts.len(), 3);
        let lengths: Vec<usize> = parts.iter().map(|part| part.text.len()).collect();
        assert_eq!(lengths, vec![LEAF_TEXT_BYTES, LEAF_TEXT_BYTES, 3]);
        for part in parts.iter().take(2) {
            assert!(part.text.chars().all(|c| c == 'é'));
            assert_eq!(part.spans.len(), 1);
            let span = part.spans.first().expect("span");
            assert_eq!(span.range.start.index(), 0);
            assert_eq!(span.range.end.index(), part.text.len());
        }
        let last = parts.last().expect("last part");
        assert_eq!(
            last.objects,
            vec![(ByteOffset::new(0), InlineObject::Image(ImageId::new(0)))]
        );
        assert_eq!(parts.first().map(|part| part.objects.len()), Some(0));
    }

    #[test]
    fn splitting_prefers_a_space_in_the_second_half() {
        let early_space = format!("a {}", "b".repeat(LEAF_TEXT_BYTES));
        let parts = split_inlines(
            Inlines {
                text: early_space,
                ..Inlines::default()
            },
            LEAF_TEXT_BYTES,
        );
        // The only space is in the first half: cut at the limit instead.
        assert_eq!(
            parts.first().map(|part| part.text.len()),
            Some(LEAF_TEXT_BYTES)
        );
        let late_space = format!("{} {}", "a".repeat(LEAF_TEXT_BYTES - 10), "b".repeat(100));
        let parts = split_inlines(
            Inlines {
                text: late_space,
                ..Inlines::default()
            },
            LEAF_TEXT_BYTES,
        );
        assert_eq!(
            parts.first().map(|part| part.text.len()),
            Some(LEAF_TEXT_BYTES - 9)
        );
        assert_eq!(floor_char_boundary("aé", 2), 1);
        assert_eq!(floor_char_boundary("aé", 3), 3);
        assert_eq!(floor_char_boundary("", 0), 0);
    }

    #[test]
    fn tables_chunk_by_rows() {
        let mut builder = builder();
        builder.table(Table {
            alignments: vec![Align::Start],
            header: vec![Inlines::default()],
            rows: vec![vec![Inlines::default()]; 120],
        });
        let doc = builder.finish();
        let chunks: Vec<(usize, usize, bool)> = doc
            .leaves
            .iter()
            .filter_map(|leaf| {
                if let LeafKind::TableRows {
                    rows, with_header, ..
                } = &leaf.kind
                {
                    Some((rows.start.index(), rows.end.index(), *with_header))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            chunks,
            vec![(0, 50, true), (50, 100, false), (100, 120, false)]
        );
    }

    #[test]
    fn code_line_starts_are_exact() {
        let mut builder = builder();
        builder.code_block("", "ab\ncde\n\nf\n");
        let doc = builder.finish();
        let block = doc.code_blocks.first().expect("block");
        let starts: Vec<usize> = block.line_starts.iter().map(|at| at.index()).collect();
        assert_eq!(starts, vec![0, 3, 7, 8]);
        assert_eq!(block.text, "ab\ncde\n\nf");
    }

    #[test]
    fn every_html_style_heading_and_alignment() {
        for (tag, style) in [
            ("b", Style::STRONG),
            ("strong", Style::STRONG),
            ("i", Style::EMPHASIS),
            ("em", Style::EMPHASIS),
            ("code", Style::CODE),
            ("tt", Style::CODE),
            ("samp", Style::CODE),
            ("kbd", Style::KEYBOARD),
            ("sub", Style::SUBSCRIPT),
            ("sup", Style::SUPERSCRIPT),
            ("del", Style::STRIKETHROUGH),
            ("s", Style::STRIKETHROUGH),
            ("ins", Style::UNDERLINE),
            ("u", Style::UNDERLINE),
            ("mark", Style::HIGHLIGHT),
        ] {
            assert_eq!(html_style(tag), Some(style), "<{tag}>");
        }
        assert_eq!(html_style("span"), None);
        let levels: Vec<Option<HeadingLevel>> = ["h1", "h2", "h3", "h4", "h5", "h6", "h7"]
            .iter()
            .map(|name| heading_level(name))
            .collect();
        assert_eq!(
            levels,
            vec![
                Some(HeadingLevel::H1),
                Some(HeadingLevel::H2),
                Some(HeadingLevel::H3),
                Some(HeadingLevel::H4),
                Some(HeadingLevel::H5),
                Some(HeadingLevel::H6),
                None
            ]
        );
        assert_eq!(parse_align(" LEFT "), Some(Align::Start));
        assert_eq!(parse_align("right"), Some(Align::End));
        assert_eq!(parse_align("middle"), Some(Align::Center));
        assert_eq!(parse_align("justify"), None);
    }

    #[test]
    fn tags_inside_pre_are_dropped_and_pre_ends_at_its_closer() {
        let mut builder = builder();
        builder.html_block(
            "<pre><b>bold</b> text</pre>
<p>after</p>",
        );
        builder.html_block("<pre></pre>");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["<code>", "after", "<code>"]);
        assert_eq!(
            doc.code_blocks.first().map(|block| block.text.as_str()),
            Some("bold text")
        );
        assert_eq!(doc.code_blocks.get(1).map(CodeBlock::line_count), Some(0));
        assert_eq!(doc.notices, Vec::new());
    }

    #[test]
    fn a_summary_opened_inline_collects_and_closes() {
        let mut builder = builder();
        builder.html_block("<details>");
        let mut inlines = builder.inline_builder();
        builder.html_inline(&mut inlines, "<summary>");
        inlines.text("Title");
        builder.html_inline(&mut inlines, "</summary>");
        builder.paragraph(inlines);
        paragraph(&mut builder, "first");
        paragraph(&mut builder, "second");
        builder.html_block("</details>");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["Title", "first", "second"]);
        assert!(matches!(
            doc.leaves.first().map(|leaf| &leaf.kind),
            Some(LeafKind::DetailsSummary { .. })
        ));
    }

    #[test]
    fn summaries_join_paragraphs_with_spaces() {
        let mut builder = builder();
        builder.html_block("<details><summary>");
        paragraph(&mut builder, "one");
        paragraph(&mut builder, "two");
        builder.html_block("</summary></details>");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["one two"]);
    }

    #[test]
    fn details_ids_are_distinct_and_open_is_read() {
        let mut builder = builder();
        builder.html_block("<details><summary>a</summary>");
        builder.html_block("</details>");
        builder.html_block("<details open><summary>b</summary>");
        builder.html_block("</details>");
        let doc = builder.finish();
        let details: Vec<(usize, bool)> = doc
            .contexts
            .iter()
            .filter_map(|context| {
                if let Container::Details { id, open } = context.container {
                    Some((id.index(), open))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(details, vec![(0, false), (1, true)]);
    }

    #[test]
    fn media_become_links() {
        let mut builder = builder();
        builder.html_block("<video src=\"demo.mp4\">fallback text</video>");
        builder.html_block("<audio>no source</audio>");
        let mut inlines = builder.inline_builder();
        builder.html_inline(&mut inlines, "<video src=\"inline.mp4\" controls>");
        inlines.text("hidden");
        builder.html_inline(&mut inlines, "</video>");
        inlines.text(" after");
        builder.paragraph(inlines);
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["▶ video", "▶ audio", "▶ video after"]);
        let linked: Vec<bool> = doc
            .leaves
            .iter()
            .map(|leaf| {
                if let LeafKind::Paragraph(inlines) = &leaf.kind {
                    inlines.spans.iter().any(|s| s.link.is_some())
                } else {
                    false
                }
            })
            .collect();
        assert_eq!(linked, vec![true, false, true]);
    }

    #[test]
    fn html_opened_inside_a_container_ends_with_it() {
        let mut builder = builder();
        let item = builder.begin_container(Container::Quote);
        builder.html_block("<div align=\"right\"><b>");
        paragraph(&mut builder, "inside");
        builder.end_container(item);
        paragraph(&mut builder, "after");
        let doc = builder.finish();
        let last = doc.leaves.last().expect("after");
        assert_eq!(last.align, Align::Start);
        let LeafKind::Paragraph(inlines) = &last.kind else {
            panic!("paragraph");
        };
        assert_eq!(inlines.spans, Vec::new());
    }

    #[test]
    fn a_stray_closer_after_a_closed_tag_closes_nothing() {
        let mut builder = builder();
        builder.html_block("<div align=\"right\">");
        builder.html_block("<b>x</b></b>");
        paragraph(&mut builder, "still right");
        let doc = builder.finish();
        assert_eq!(doc.leaves.last().map(|leaf| leaf.align), Some(Align::End));
    }

    #[test]
    fn a_summary_left_open_ends_with_its_container() {
        let mut builder = builder();
        let item = builder.begin_container(Container::Quote);
        builder.html_block("<details><summary>unclosed");
        builder.end_container(item);
        paragraph(&mut builder, "after");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["unclosed", "after"]);
        assert_eq!(containers(&doc, 1), Vec::new());
    }

    #[test]
    fn deferred_html_closes_wait_for_markdown_containers() {
        let mut builder = builder();
        builder.html_block("<details>");
        let quote = builder.begin_container(Container::Quote);
        let mut inlines = builder.inline_builder();
        inlines.text("in quote");
        builder.html_inline(&mut inlines, "</details>");
        builder.paragraph(inlines);
        paragraph(&mut builder, "still in quote");
        builder.end_container(quote);
        paragraph(&mut builder, "after");
        let doc = builder.finish();
        assert_eq!(
            texts(&doc),
            vec!["Details", "in quote", "still in quote", "after"]
        );
        let in_details = |leaf| {
            containers(&doc, leaf)
                .iter()
                .any(|c| matches!(c, Container::Details { .. }))
        };
        assert!(in_details(0));
        assert_eq!(containers(&doc, 0).len(), 1);
        assert!(in_details(1));
        assert_eq!(containers(&doc, 1).first(), Some(&Container::Quote));
        assert!(!in_details(3));
        assert_eq!(containers(&doc, 3), Vec::new());
    }

    #[test]
    fn leading_whitespace_of_html_text_is_dropped_but_inner_spaces_kept() {
        let mut builder = builder();
        builder.html_block("<p>\n   a   <b>b</b>   c\n</p>");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["a b c"]);
    }

    #[test]
    fn one_line_longer_than_the_cap_is_one_chunk() {
        let mut builder = builder();
        builder.code_block("", &format!("{}\n", "x".repeat(3 * LEAF_TEXT_BYTES)));
        let doc = builder.finish();
        let chunks: Vec<_> = doc
            .leaves
            .iter()
            .filter_map(|leaf| {
                if let LeafKind::Code { lines, .. } = &leaf.kind {
                    Some((lines.start.index(), lines.end.index()))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(chunks, vec![(0, 1)]);
    }

    #[test]
    fn splitting_at_exact_multiples_and_span_edges() {
        // Exactly twice the cap, no spaces: two parts, not a third empty one.
        let parts = split_inlines(
            Inlines {
                text: "x".repeat(2 * LEAF_TEXT_BYTES),
                ..Inlines::default()
            },
            LEAF_TEXT_BYTES,
        );
        assert_eq!(parts.len(), 2);
        // A span starting exactly at the cut belongs to the second part only, and one ending
        // exactly at it to the first only: no empty spans.
        let mut inlines = InlineBuilder::default();
        inlines.text(&"a".repeat(LEAF_TEXT_BYTES));
        let strong = inlines.begin_style(Style::STRONG);
        inlines.text("bold");
        inlines.end_style(strong);
        let parts = split_inlines(inlines.finish(), LEAF_TEXT_BYTES);
        assert_eq!(parts.first().map(|p| p.spans.len()), Some(0));
        assert_eq!(parts.get(1).map(|p| p.spans.len()), Some(1));
        let mut inlines = InlineBuilder::default();
        let strong = inlines.begin_style(Style::STRONG);
        inlines.text(&"a".repeat(LEAF_TEXT_BYTES));
        inlines.end_style(strong);
        inlines.text("plain");
        let parts = split_inlines(inlines.finish(), LEAF_TEXT_BYTES);
        assert_eq!(parts.first().map(|p| p.spans.len()), Some(1));
        assert_eq!(parts.get(1).map(|p| p.spans.len()), Some(0));
    }

    #[test]
    fn html_opened_just_before_a_container_outlives_it() {
        let mut builder = builder();
        builder.html_block("<div align=\"right\">");
        let quote = builder.begin_container(Container::Quote);
        paragraph(&mut builder, "quoted");
        builder.end_container(quote);
        paragraph(&mut builder, "after");
        let doc = builder.finish();
        assert_eq!(doc.leaves.last().map(|leaf| leaf.align), Some(Align::End));
    }

    #[test]
    fn a_container_inside_a_summary_does_not_end_it() {
        let mut builder = builder();
        builder.html_block("<details><summary>");
        paragraph(&mut builder, "a");
        let quote = builder.begin_container(Container::Quote);
        paragraph(&mut builder, "b");
        builder.end_container(quote);
        paragraph(&mut builder, "c");
        builder.html_block("</summary></details>");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["a b c"]);
    }

    #[test]
    fn text_after_a_break_starts_without_a_space() {
        let mut builder = builder();
        builder.html_block("<p>a<br> b</p>");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["a\nb"]);
    }
}
