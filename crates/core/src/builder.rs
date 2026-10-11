//! Building a `Document` leaf by leaf (docs/plan.md §4.2, §5.1).
//!
//! No comrak here: `parse` walks comrak's tree and calls these methods, so everything about the
//! model's shape (containers per leaf, chunking, styled runs, what HTML tags mean) is in one
//! place that does not depend on the parser.

use crate::html::{HtmlToken, tokenize};
use crate::link::classify;
use crate::model::{
    AlertKind, Align, Anchor, CODE_CHUNK_LINES, CodeBlock, Container, Document, Footnote,
    HeadingLevel, Image, InlineObject, Inlines, Leaf, LeafKind, Link, Notice, Span, Style,
    TABLE_CHUNK_ROWS, Table,
};
use crate::units::{
    AnchorId, ByteOffset, CodeBlockId, DetailsId, FootnoteId, ImageId, LeafId, LineNo, LinkId,
    RowNo, TableId,
};

/// Turns heading text into a GitHub anchor slug, unique within the document.
pub(crate) type Slugger = Box<dyn FnMut(&str) -> String>;

pub(crate) struct Builder {
    doc: Document,
    containers: Vec<Container>,
    /// HTML tags with block effects still open, possibly across several HTML blocks:
    /// `<div align="center">` ... Markdown ... `</div>` is common in READMEs.
    html_open: Vec<HtmlOpen>,
    /// Text from HTML blocks not yet made into a leaf.
    pending: Option<Pending>,
    /// Text of an open raw `<pre>`, kept verbatim.
    pre: Option<String>,
    next_details: usize,
    slugger: Slugger,
}

struct HtmlOpen {
    name: String,
    effect: BlockEffect,
    /// `containers.len()` when the tag opened; closing a Markdown container below this closes it.
    depth: usize,
}

#[derive(Clone, Copy)]
enum BlockEffect {
    Align(Align),
    Details(DetailsId),
    /// A raw `<blockquote>`: the quote container it pushed, at this index of `containers`.
    Quote(usize),
    /// `<script>` and `<style>`: their content is never shown.
    Skip,
    /// A block tag with no effect beyond ending the text before it.
    None,
}

struct Pending {
    inlines: InlineBuilder,
    kind: PendingKind,
}

#[derive(Clone, Copy)]
enum PendingKind {
    Paragraph,
    Heading(HeadingLevel),
    Summary(DetailsId),
}

impl Builder {
    pub(crate) fn new(slugger: Slugger) -> Self {
        Self {
            doc: Document::default(),
            containers: Vec::new(),
            html_open: Vec::new(),
            pending: None,
            pre: None,
            next_details: 0,
            slugger,
        }
    }

    pub(crate) fn notice(&mut self, notice: Notice) {
        self.doc.notices.push(notice);
    }

    pub(crate) fn finish(mut self) -> Document {
        self.flush_pre();
        self.flush_pending();
        self.doc
    }

    // Containers.

    pub(crate) fn begin_container(&mut self, container: Container) {
        self.flush_pending();
        self.containers.push(container);
    }

    pub(crate) fn end_container(&mut self) {
        self.flush_pending();
        self.containers.pop();
        // HTML effects opened inside the closed container end with it.
        let depth = self.containers.len();
        self.html_open.retain(|open| open.depth <= depth);
    }

    pub(crate) fn begin_alert(&mut self, kind: AlertKind) {
        self.begin_container(Container::Alert(kind));
        self.push_leaf(LeafKind::AlertTitle(kind));
    }

    pub(crate) fn begin_footnote(&mut self, label: &str, number: u32) -> FootnoteId {
        self.flush_pending();
        let id = FootnoteId::new(self.doc.footnotes.len());
        let first = self.next_leaf();
        self.doc.footnotes.push(Footnote {
            label: label.to_owned(),
            number,
            leaves: first..first,
        });
        self.containers.push(Container::Footnote {
            footnote: id,
            show_number: true,
        });
        id
    }

    pub(crate) fn end_footnote(&mut self, id: FootnoteId) {
        self.end_container();
        let end = self.next_leaf();
        if let Some(footnote) = self.doc.footnotes.get_mut(id.index()) {
            footnote.leaves.end = end;
        }
    }

    // Leaves.

    /// A paragraph. An empty one is kept only when it is the first leaf of a list item or
    /// footnote, which needs a leaf to carry its marker.
    pub(crate) fn paragraph(&mut self, inlines: InlineBuilder) {
        self.flush_pending();
        let inlines = inlines.finish();
        if inlines.text.is_empty() && !self.marker_pending() {
            return;
        }
        self.push_leaf(LeafKind::Paragraph(inlines));
    }

    /// A heading; `slug_text` is the text GitHub derives its anchor from.
    pub(crate) fn heading(&mut self, level: HeadingLevel, inlines: InlineBuilder, slug_text: &str) {
        self.flush_pending();
        self.push_heading(level, inlines.finish(), slug_text);
    }

    pub(crate) fn thematic_break(&mut self) {
        self.flush_pending();
        self.push_leaf(LeafKind::ThematicBreak);
    }

    /// A code block, split into leaves of at most `CODE_CHUNK_LINES` lines.
    pub(crate) fn code_block(&mut self, info: &str, text: &str) {
        self.flush_pending();
        let mut line_starts = vec![ByteOffset::new(0)];
        let body = text.strip_suffix('\n').unwrap_or(text);
        line_starts.extend(
            body.match_indices('\n')
                .map(|(at, _newline)| ByteOffset::new(at + 1)),
        );
        if body.is_empty() {
            line_starts.clear();
        }
        let lines = line_starts.len();
        let block = CodeBlockId::new(self.doc.code_blocks.len());
        self.doc.code_blocks.push(CodeBlock {
            info: info.to_owned(),
            text: body.to_owned(),
            line_starts,
        });
        for (start, end) in chunks(lines, CODE_CHUNK_LINES) {
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
        for (index, (start, end)) in chunks(rows, TABLE_CHUNK_ROWS).enumerate() {
            self.push_leaf(LeafKind::TableRows {
                table: id,
                rows: RowNo::new(start)..RowNo::new(end),
                with_header: index == 0,
            });
        }
    }

    // Links and images.

    pub(crate) fn add_link(&mut self, destination: &str, title: &str) -> LinkId {
        let id = LinkId::new(self.doc.links.len());
        self.doc.links.push(Link {
            target: classify(destination),
            title: non_empty(title),
        });
        id
    }

    pub(crate) fn add_footnote_link(&mut self, footnote: FootnoteId) -> LinkId {
        let id = LinkId::new(self.doc.links.len());
        self.doc.links.push(Link {
            target: crate::model::LinkTarget::Footnote(footnote),
            title: None,
        });
        id
    }

    pub(crate) fn add_image(&mut self, image: Image) -> ImageId {
        let id = ImageId::new(self.doc.images.len());
        self.doc.images.push(image);
        id
    }

    pub(crate) fn markdown_image(
        &mut self,
        destination: &str,
        title: &str,
        alt: String,
    ) -> ImageId {
        self.add_image(Image {
            source: classify(destination),
            alt,
            title: non_empty(title),
            width: None,
            height: None,
        })
    }

    // HTML.

    /// Raw HTML inside a paragraph or heading: one tag per comrak node, paired up by the
    /// inline builder's own stack.
    pub(crate) fn html_inline(&mut self, inlines: &mut InlineBuilder, html: &str) {
        for token in tokenize(html) {
            self.inline_token(inlines, &token);
        }
    }

    /// A raw HTML block: tags become leaves, styles, links, images, alignment and `<details>`.
    pub(crate) fn html_block(&mut self, html: &str) {
        for token in tokenize(html) {
            if self.pre.is_some() {
                self.pre_token(&token);
                continue;
            }
            if self.skipping() && !matches!(&token, HtmlToken::End { name } if is_skipped(name)) {
                continue;
            }
            match &token {
                HtmlToken::Text(text) => self.html_text(text),
                HtmlToken::Start {
                    name, self_closing, ..
                } => self.html_start(name, &token, *self_closing),
                HtmlToken::End { name } => self.html_end(name),
            }
        }
        self.flush_pending();
    }

    /// Inside a raw `<pre>`: text is kept as written, tags inside are dropped, `</pre>` ends it.
    fn pre_token(&mut self, token: &HtmlToken) {
        match token {
            HtmlToken::Text(text) => {
                if let Some(pre) = self.pre.as_mut() {
                    pre.push_str(text);
                }
            }
            HtmlToken::End { name } if name == "pre" => self.flush_pre(),
            HtmlToken::Start { .. } | HtmlToken::End { .. } => {}
        }
    }

    /// Ends a raw `<pre>` as a code block. HTML ignores a newline right after `<pre>`.
    fn flush_pre(&mut self) {
        if let Some(text) = self.pre.take() {
            let text = text.strip_prefix('\n').unwrap_or(&text);
            self.code_block("", text);
        }
    }

    fn html_start(&mut self, name: &str, token: &HtmlToken, self_closing: bool) {
        match name {
            "hr" => self.thematic_break(),
            "pre" if !self_closing => {
                self.flush_pending();
                self.pre = Some(String::new());
            }
            "blockquote" if !self_closing => {
                self.flush_pending();
                let index = self.containers.len();
                self.containers.push(Container::Quote);
                self.html_open.push(HtmlOpen {
                    name: name.to_owned(),
                    effect: BlockEffect::Quote(index),
                    depth: self.containers.len(),
                });
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_pending();
                self.open_block(name, token, self_closing);
                let level = name
                    .strip_prefix('h')
                    .and_then(|digit| digit.parse::<u8>().ok())
                    .map_or(HeadingLevel::H1, HeadingLevel::clamped);
                self.pending = Some(Pending {
                    inlines: InlineBuilder::new(),
                    kind: PendingKind::Heading(level),
                });
            }
            "details" if !self_closing => {
                self.flush_pending();
                let id = DetailsId::new(self.next_details);
                self.next_details += 1;
                self.containers.push(Container::Details(id));
                self.html_open.push(HtmlOpen {
                    name: name.to_owned(),
                    effect: BlockEffect::Details(id),
                    depth: self.containers.len(),
                });
            }
            "summary" => {
                self.flush_pending();
                if let Some(details) = self.innermost_details() {
                    self.pending = Some(Pending {
                        inlines: InlineBuilder::new(),
                        kind: PendingKind::Summary(details),
                    });
                }
            }
            "td" | "th" => {
                // Cells of a raw HTML table read as text separated by spaces.
                if let Some(pending) = self.pending.as_mut() {
                    pending.inlines.text(" ");
                }
            }
            name if is_skipped(name) && !self_closing => self.html_open.push(HtmlOpen {
                name: name.to_owned(),
                effect: BlockEffect::Skip,
                depth: self.containers.len(),
            }),
            name if is_block(name) => {
                self.flush_pending();
                self.open_block(name, token, self_closing);
            }
            _inline => {
                let mut pending = self.take_pending_or_new();
                self.inline_token(&mut pending.inlines, token);
                self.pending = Some(pending);
            }
        }
    }

    fn html_end(&mut self, name: &str) {
        let is_heading = matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6");
        if is_block(name) || is_heading || matches!(name, "summary" | "details") || is_skipped(name)
        {
            self.flush_pending();
            self.close_block(name);
        } else if let Some(mut pending) = self.pending.take() {
            let token = HtmlToken::End {
                name: name.to_owned(),
            };
            self.inline_token(&mut pending.inlines, &token);
            self.pending = Some(pending);
        }
    }

    fn html_text(&mut self, text: &str) {
        // HTML collapses whitespace; text that is only whitespace between block tags is nothing.
        let collapsed = collapse_whitespace(text);
        let pending_empty = self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.inlines.ends_with_space());
        let collapsed = if pending_empty {
            collapsed.trim_start()
        } else {
            collapsed.as_str()
        };
        if collapsed.is_empty() {
            return;
        }
        let mut pending = self.take_pending_or_new();
        pending.inlines.text(collapsed);
        self.pending = Some(pending);
    }

    /// Applies one HTML token to inline text: styles, links, images and line breaks.
    fn inline_token(&mut self, inlines: &mut InlineBuilder, token: &HtmlToken) {
        match token {
            HtmlToken::Text(text) => inlines.text(text),
            HtmlToken::Start {
                name, self_closing, ..
            } => match name.as_str() {
                "br" => inlines.text("\n"),
                "img" => {
                    if let Some(image) = html_image(token) {
                        let id = self.add_image(image);
                        inlines.object(InlineObject::Image(id));
                    }
                }
                "a" if !self_closing => {
                    let link = token
                        .attribute("href")
                        .map(|href| self.add_link(href, token.attribute("title").unwrap_or("")));
                    inlines.open_html(name, InlineEffect::Link(link));
                }
                name if !self_closing => {
                    if let Some(style) = html_style(name) {
                        inlines.open_html(name, InlineEffect::Style(style));
                    }
                }
                _ => {}
            },
            HtmlToken::End { name } => inlines.close_html(name),
        }
    }

    // Internals.

    fn open_block(&mut self, name: &str, token: &HtmlToken, self_closing: bool) {
        if self_closing {
            return;
        }
        let align = match (name, token.attribute("align")) {
            ("center", _) => Some(Align::Center),
            (_, Some(value)) => parse_align(value),
            (_, None) => None,
        };
        self.html_open.push(HtmlOpen {
            name: name.to_owned(),
            effect: align.map_or(BlockEffect::None, BlockEffect::Align),
            depth: self.containers.len(),
        });
    }

    /// Closes the innermost open tag called `name`, and any opened after it.
    fn close_block(&mut self, name: &str) {
        let Some(position) = self.html_open.iter().rposition(|open| open.name == name) else {
            return;
        };
        for open in self.html_open.drain(position..).rev() {
            let index = match open.effect {
                BlockEffect::Details(id) => self
                    .containers
                    .iter()
                    .rposition(|container| *container == Container::Details(id)),
                BlockEffect::Quote(index) => {
                    (self.containers.get(index) == Some(&Container::Quote)).then_some(index)
                }
                BlockEffect::Align(_) | BlockEffect::Skip | BlockEffect::None => None,
            };
            if let Some(index) = index {
                self.containers.truncate(index);
            }
        }
    }

    fn skipping(&self) -> bool {
        self.html_open
            .iter()
            .any(|open| matches!(open.effect, BlockEffect::Skip))
    }

    fn innermost_details(&self) -> Option<DetailsId> {
        self.html_open
            .iter()
            .rev()
            .find_map(|open| match open.effect {
                BlockEffect::Details(id) => Some(id),
                BlockEffect::Align(_)
                | BlockEffect::Quote(_)
                | BlockEffect::Skip
                | BlockEffect::None => None,
            })
    }

    fn align(&self) -> Align {
        self.html_open
            .iter()
            .rev()
            .find_map(|open| match open.effect {
                BlockEffect::Align(align) => Some(align),
                BlockEffect::Details(_)
                | BlockEffect::Quote(_)
                | BlockEffect::Skip
                | BlockEffect::None => None,
            })
            .unwrap_or_default()
    }

    fn take_pending_or_new(&mut self) -> Pending {
        self.pending.take().unwrap_or_else(|| Pending {
            inlines: InlineBuilder::new(),
            kind: PendingKind::Paragraph,
        })
    }

    fn flush_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let mut inlines = pending.inlines.finish();
        trim_end(&mut inlines);
        match pending.kind {
            PendingKind::Paragraph => {
                if !inlines.text.is_empty() {
                    self.push_leaf(LeafKind::Paragraph(inlines));
                }
            }
            PendingKind::Heading(level) => {
                let slug_text = inlines.text.replace(Inlines::OBJECT, "");
                self.push_heading(level, inlines, &slug_text);
            }
            PendingKind::Summary(details) => {
                self.push_leaf(LeafKind::DetailsSummary { details, inlines });
            }
        }
    }

    fn push_heading(&mut self, level: HeadingLevel, inlines: Inlines, slug_text: &str) {
        let anchor = AnchorId::new(self.doc.anchors.len());
        let slug = (self.slugger)(slug_text);
        let leaf = self.push_leaf(LeafKind::Heading {
            level,
            inlines,
            anchor,
        });
        self.doc.anchors.push(Anchor { slug, leaf });
    }

    /// Whether the next leaf is the first of a list item or footnote (and so carries a marker).
    fn marker_pending(&self) -> bool {
        self.containers.iter().any(|container| match container {
            Container::ListItem(item) => item.show_marker,
            Container::Footnote { show_number, .. } => *show_number,
            Container::Quote | Container::Alert(_) | Container::Details(_) => false,
        })
    }

    fn next_leaf(&self) -> LeafId {
        LeafId::new(self.doc.leaves.len())
    }

    fn push_leaf(&mut self, kind: LeafKind) -> LeafId {
        let id = self.next_leaf();
        let align = self.align();
        self.doc.leaves.push(Leaf {
            kind,
            containers: self.containers.clone(),
            align,
        });
        // Markers are drawn on a container's first leaf only.
        for container in &mut self.containers {
            match container {
                Container::ListItem(item) => item.show_marker = false,
                Container::Footnote { show_number, .. } => *show_number = false,
                Container::Quote | Container::Alert(_) | Container::Details(_) => {}
            }
        }
        id
    }
}

/// `(start, end)` index ranges of at most `size` covering `0..total`; one empty range if
/// `total` is 0, so an empty code block or table still gets a leaf.
fn chunks(total: usize, size: usize) -> impl Iterator<Item = (usize, usize)> {
    let count = total.div_ceil(size).max(1);
    (0..count).map(move |chunk| {
        let start = chunk * size;
        (start, (start + size).min(total))
    })
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

fn is_skipped(name: &str) -> bool {
    matches!(name, "script" | "style" | "template" | "textarea" | "title")
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
            | "pre"
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

fn html_image(token: &HtmlToken) -> Option<Image> {
    let source = token.attribute("src")?;
    let size = |name: &str| {
        token
            .attribute(name)
            .and_then(|value| value.trim().trim_end_matches("px").parse::<u32>().ok())
    };
    Some(Image {
        source: classify(source),
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

/// Drops trailing spaces left by collapsed HTML whitespace, and any span that then ends past the
/// text.
fn trim_end(inlines: &mut Inlines) {
    let kept = inlines.text.trim_end_matches(' ').len();
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

/// What an open inline HTML tag did, to undo when it closes.
#[derive(Debug, Clone, Copy)]
pub(crate) enum InlineEffect {
    Style(Style),
    /// A link, or `None` for an `<a>` with no `href`, which still has to be closed.
    Link(Option<LinkId>),
}

/// Builds one `Inlines`: text with styled runs.
#[derive(Debug, Default)]
pub(crate) struct InlineBuilder {
    text: String,
    spans: Vec<Span>,
    objects: Vec<(ByteOffset, InlineObject)>,
    styles: Vec<Style>,
    links: Vec<Option<LinkId>>,
    html_open: Vec<(String, InlineEffect)>,
}

impl InlineBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let start = ByteOffset::end_of(&self.text);
        self.text.push_str(text);
        let end = ByteOffset::end_of(&self.text);
        let style = self
            .styles
            .iter()
            .fold(Style::PLAIN, |all, style| all.union(*style));
        let link = self.links.last().copied().flatten();
        if style.is_plain() && link.is_none() {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.range.end == start
            && last.style == style
            && last.link == link
        {
            last.range.end = end;
            return;
        }
        self.spans.push(Span {
            range: start..end,
            style,
            link,
        });
    }

    pub(crate) fn object(&mut self, object: InlineObject) {
        self.objects.push((ByteOffset::end_of(&self.text), object));
        let mut buffer = [0; 4];
        self.text(Inlines::OBJECT.encode_utf8(&mut buffer));
    }

    pub(crate) fn begin_style(&mut self, style: Style) {
        self.styles.push(style);
    }

    pub(crate) fn end_style(&mut self) {
        self.styles.pop();
    }

    pub(crate) fn begin_link(&mut self, link: LinkId) {
        self.links.push(Some(link));
    }

    pub(crate) fn end_link(&mut self) {
        self.links.pop();
    }

    fn ends_with_space(&self) -> bool {
        self.text.is_empty() || self.text.ends_with([' ', '\n'])
    }

    fn open_html(&mut self, name: &str, effect: InlineEffect) {
        match effect {
            InlineEffect::Style(style) => self.styles.push(style),
            InlineEffect::Link(link) => self.links.push(link),
        }
        self.html_open.push((name.to_owned(), effect));
    }

    /// Closes the innermost open tag called `name` and any opened after it; a closing tag with
    /// nothing to close is ignored.
    fn close_html(&mut self, name: &str) {
        let Some(position) = self
            .html_open
            .iter()
            .rposition(|(open, _effect)| open == name)
        else {
            return;
        };
        let closed: Vec<_> = self.html_open.drain(position..).collect();
        for (_name, effect) in closed.into_iter().rev() {
            match effect {
                InlineEffect::Style(_) => {
                    self.styles.pop();
                }
                InlineEffect::Link(_) => {
                    self.links.pop();
                }
            }
        }
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

    #[test]
    fn styled_runs_merge_and_stay_plain_where_plain() {
        let mut inlines = InlineBuilder::new();
        inlines.text("a ");
        inlines.begin_style(Style::STRONG);
        inlines.text("b");
        inlines.text("c");
        inlines.begin_style(Style::EMPHASIS);
        inlines.text("d");
        inlines.end_style();
        inlines.end_style();
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
    fn code_blocks_chunk_by_lines() {
        let mut builder = builder();
        let text = (0..450)
            .map(|line| format!("line {line}\n"))
            .collect::<Vec<_>>()
            .concat();
        builder.code_block("rust", &text);
        builder.code_block("", "");
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
        assert_eq!(ranges, vec![(0, 200), (200, 400), (400, 450), (0, 0)]);
        let block = doc.code_blocks.first().expect("block");
        assert_eq!(block.line_starts.len(), 450);
        assert!(!block.text.ends_with('\n'));
    }

    #[test]
    fn list_markers_go_on_the_first_leaf_only() {
        use crate::model::{ListItem, Marker};
        let mut builder = builder();
        builder.begin_container(Container::ListItem(ListItem {
            marker: Marker::Bullet,
            tight: true,
            show_marker: true,
        }));
        let mut one = InlineBuilder::new();
        one.text("one");
        builder.paragraph(one);
        builder.paragraph(InlineBuilder::new()); // empty, not first: dropped
        let mut two = InlineBuilder::new();
        two.text("two");
        builder.paragraph(two);
        builder.end_container();
        let doc = builder.finish();
        let shown: Vec<bool> = doc
            .leaves
            .iter()
            .map(|leaf| {
                matches!(
                    leaf.containers.first(),
                    Some(Container::ListItem(item)) if item.show_marker
                )
            })
            .collect();
        assert_eq!(shown, vec![true, false]);
    }

    #[test]
    fn html_blocks_become_aligned_leaves() {
        let mut builder = builder();
        builder.html_block(
            "<p align=\"center\">\n  <img src=\"logo.png\" alt=\"Logo\" width=\"120px\">\n</p>",
        );
        builder.html_block("<h1 align=\"center\">My <b>Tool</b></h1>");
        builder.html_block("<!-- hidden -->\n<script>alert(1)</script>");
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
    fn alignment_and_details_span_html_blocks() {
        let mut builder = builder();
        builder.html_block("<div align=\"right\">");
        let mut inside = InlineBuilder::new();
        inside.text("markdown inside");
        builder.paragraph(inside);
        builder.html_block("</div>");
        builder.html_block("<details>\n<summary>More</summary>");
        let mut hidden = InlineBuilder::new();
        hidden.text("hidden");
        builder.paragraph(hidden);
        builder.html_block("</details>");
        let mut after = InlineBuilder::new();
        after.text("after");
        builder.paragraph(after);
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
        let in_details: Vec<bool> = doc
            .leaves
            .iter()
            .map(|leaf| matches!(leaf.containers.last(), Some(Container::Details(_))))
            .collect();
        assert_eq!(in_details, vec![false, true, true, false]);
    }

    #[test]
    fn raw_pre_keeps_its_text_and_raw_blockquote_quotes() {
        let mut builder = builder();
        builder.html_block("<pre>\n  ascii\n   art</pre>");
        builder.html_block("<blockquote>\nquoted\n</blockquote>");
        let doc = builder.finish();
        assert_eq!(texts(&doc), vec!["<code>", "quoted"]);
        assert_eq!(
            doc.code_blocks.first().map(|block| block.text.as_str()),
            Some("  ascii\n   art")
        );
        let quoted = doc.leaves.get(1).expect("quoted leaf");
        assert_eq!(quoted.containers, vec![Container::Quote]);
    }

    #[test]
    fn inline_html_pairs_tags_across_nodes() {
        let mut builder = builder();
        let mut inlines = InlineBuilder::new();
        builder.html_inline(&mut inlines, "<kbd>");
        inlines.text("Ctrl");
        builder.html_inline(&mut inlines, "</kbd>");
        inlines.text("+");
        builder.html_inline(&mut inlines, "</sup>"); // nothing to close: ignored
        builder.html_inline(&mut inlines, "<a href=\"#x\">");
        inlines.text("go");
        builder.html_inline(&mut inlines, "</a>");
        let inlines = inlines.finish();
        assert_eq!(inlines.text, "Ctrl+go");
        let styles: Vec<(Style, bool)> = inlines
            .spans
            .iter()
            .map(|span| (span.style, span.link.is_some()))
            .collect();
        assert_eq!(styles, vec![(Style::KEYBOARD, false), (Style::PLAIN, true)]);
    }
}
