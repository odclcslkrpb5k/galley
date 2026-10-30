//! Copying (docs/plan.md §4.5 "Copying"): the copy button a code block shows while the pointer
//! is over it, and the right-click menu. `copying.rs` decides what each copies; this finds what
//! is under the pointer, draws the button and puts text on the clipboard.

use std::path::Path;
use std::time::Duration;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, gsk};

use super::{DocumentView, Hit, Message, graphene_rect};
use crate::copying::{self, Choice, Under};
use crate::links;
use crate::render::geometry::Rect;
use crate::render::image::ImageSource;
use crate::render::paint::Target;
use crate::render::text::{self, Palette};
use crate::units;
use galley_core::model::AlertKind;
use galley_core::units::CodeBlockId;

/// The copy button's size and its distance from the code block's edges, in em.
const BUTTON_EM: f64 = 1.6;
const BUTTON_INSET_EM: f64 = 0.3;
/// How long the button shows a tick after copying.
const COPIED_FOR: Duration = Duration::from_millis(1500);
/// The menu's last item.
pub(in crate::document_view) const COPY_DOCUMENT: &str = "Copy document as Markdown";

/// The view's copying state.
#[derive(Debug, Default)]
pub(in crate::document_view) struct Copying {
    /// The code block under the pointer: it shows its button.
    hovered: Option<CodeBlockId>,
    /// The block just copied by its button, and the timer that ends its tick.
    copied: Option<(CodeBlockId, glib::SourceId)>,
    /// What the open (or last) menu offers.
    choices: Vec<Choice>,
    /// The open menu (`menu.rs`).
    pub(super) menu: Option<super::menu::Menu>,
}

impl DocumentView {
    /// The code block under the pointer at `at` (or none, the pointer gone) shows its button.
    pub(in crate::document_view) fn hover_code(&self, at: Option<(f64, f64)>) {
        let block = at.and_then(|(_x, y)| self.code_block_at(y));
        let mut copying = self.copying.borrow_mut();
        if copying.hovered != block {
            copying.hovered = block;
            drop(copying);
            self.obj().queue_draw();
        }
    }

    /// The code block whose paint is at height `y` of the view.
    fn code_block_at(&self, y: f64) -> Option<CodeBlockId> {
        let (top, _page) = self.viewport()?;
        let document = self.document.borrow();
        let doc = document.as_ref()?;
        let y = top - doc.margin() + units::round_px(y);
        let paint = doc.positions.visible(y, 1).next()?;
        let inside = (doc.positions.start(paint)..doc.positions.start(paint + 1)).contains(&y);
        doc.paints.get(paint)?.code_block.filter(|_| inside)
    }

    /// The code block whose copy button is at a point of the view (shown or not: the pointer
    /// is over the block when it is there).
    pub(in crate::document_view) fn copy_button_at(&self, x: f64, y: f64) -> Option<CodeBlockId> {
        let block = self.code_block_at(y)?;
        let rect = self.copy_button(block)?;
        let (x, y) = (units::round_px(x), units::round_px(y));
        ((rect.x..rect.x + rect.width).contains(&x) && (rect.y..rect.y + rect.height).contains(&y))
            .then_some(block)
    }

    /// Where a code block's copy button is, in the view: at the block's top right, or kept at
    /// the top of the view while the block's top is scrolled past (a long block's button stays
    /// in reach). `None` if it would not be seen.
    fn copy_button(&self, block: CodeBlockId) -> Option<Rect> {
        let (top, page) = self.viewport()?;
        let document = self.document.borrow();
        let doc = document.as_ref()?;
        let range = doc.code_blocks.get(&block)?;
        let metrics = doc.setup.metrics();
        let first = doc.paints.get(range.start)?;
        let last = doc.paints.get(range.end.checked_sub(1)?)?;
        let margin = doc.margin();
        let block_top =
            margin + doc.positions.start(range.start) + metrics.px(first.gap_above_em) - top;
        let block_bottom =
            margin + doc.positions.start(range.end) - metrics.px(last.gap_below_em) - top;
        let size = metrics.px(BUTTON_EM);
        let inset = metrics.px(BUTTON_INSET_EM);
        let lowest = block_bottom - inset - size;
        let y = (block_top.max(0) + inset).min(lowest);
        if y < block_top || y + size <= 0 || y >= page {
            return None;
        }
        Some(Rect {
            x: margin + doc.width - inset - size,
            y,
            width: size,
            height: size,
        })
    }

    /// Copies a code block, as its button does, and shows a tick on the button for a moment.
    pub(in crate::document_view) fn copy_code(&self, block: CodeBlockId) {
        let text = self
            .document
            .borrow()
            .as_ref()
            .and_then(|doc| copying::block_text(&doc.code_texts, block));
        let Some(text) = text else {
            return;
        };
        self.obj().clipboard().set_text(&text);
        let done = glib::timeout_add_local_once(
            COPIED_FOR,
            glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                move || {
                    view.imp().copying.borrow_mut().copied = None;
                    view.queue_draw();
                }
            ),
        );
        if let Some((_block, timer)) = self.copying.borrow_mut().copied.replace((block, done)) {
            timer.remove();
        }
        self.obj().queue_draw();
    }

    /// Draws the copy button of the code block under the pointer (or just copied).
    pub(in crate::document_view) fn draw_copy_button(
        &self,
        snapshot: &gtk::Snapshot,
        palette: Palette,
    ) {
        let (block, copied) = {
            let copying = self.copying.borrow();
            match (copying.copied.as_ref(), copying.hovered) {
                (Some((block, _timer)), _) => (*block, true),
                (None, Some(block)) => (block, false),
                (None, None) => return,
            }
        };
        let Some(rect) = self.copy_button(block) else {
            return;
        };
        let bounds = graphene_rect(rect);
        let radius = bounds.width() / 6.0;
        let rounded = gsk::RoundedRect::from_rect(bounds, radius);
        snapshot.push_rounded_clip(&rounded);
        snapshot.append_color(&palette.inverse().with_alpha(0.92), &bounds);
        snapshot.pop();
        let border = palette.faint(0.3);
        snapshot.append_border(&rounded, &[1.0; 4], &[border, border, border, border]);
        // The glyph, in a square of side `s` at the button's centre.
        let s = bounds.width() * 0.5;
        let (x0, y0) = (
            bounds.x() + bounds.width() / 4.0,
            bounds.y() + bounds.height() / 4.0,
        );
        let path = gsk::PathBuilder::new();
        let color = if copied {
            // A tick, in the colour GitHub gives success.
            path.move_to(x0 + s * 0.1, y0 + s * 0.55);
            path.line_to(x0 + s * 0.4, y0 + s * 0.85);
            path.line_to(x0 + s * 0.95, y0 + s * 0.2);
            palette.alert(AlertKind::Tip)
        } else {
            // Two sheets, one behind the other.
            let sheet = s * 0.66;
            let corner = s * 0.1;
            path.add_rounded_rect(&gsk::RoundedRect::from_rect(
                gtk::graphene::Rect::new(x0 + s - sheet, y0, sheet, sheet),
                corner,
            ));
            path.add_rounded_rect(&gsk::RoundedRect::from_rect(
                gtk::graphene::Rect::new(x0, y0 + s - sheet, sheet, sheet),
                corner,
            ));
            palette.faint(0.75)
        };
        let stroke = gsk::Stroke::new(bounds.width() / 16.0);
        snapshot.append_stroke(&path.to_path(), &stroke, &color);
    }

    /// What the menu offers at a point of the view.
    fn menu_choices(&self, x: f64, y: f64) -> Vec<Choice> {
        let hit = self.hit_at(x, y);
        let Some((top, _page)) = self.viewport() else {
            return Vec::new();
        };
        let context = self.obj().pango_context();
        let path = self.path.borrow().clone();
        let mut document = self.document.borrow_mut();
        let Some(doc) = document.as_mut() else {
            return Vec::new();
        };
        let margin = doc.margin();
        let y = top - margin + units::round_px(y);
        let Some(paint) = doc.positions.visible(y, 1).next() else {
            return Vec::new();
        };
        if !(doc.positions.start(paint)..doc.positions.start(paint + 1)).contains(&y) {
            return Vec::new();
        }
        let local = (units::round_px(x) - margin, y - doc.positions.start(paint));
        let found = doc
            .hit_layout(&context, paint)
            .map(|(measured, geometry)| text::text_at(measured, geometry, local))
            .unwrap_or_default();
        let link = match &hit {
            Some(Hit::Link(target)) => link_address(target, path.as_deref(), &doc.anchors),
            Some(Hit::Image(_)) | None => None,
        };
        let image = match hit {
            Some(Hit::Image(image)) => doc
                .gallery
                .table()
                .info(image)
                .and_then(|info| info.source.as_ref())
                .map(|source| image_address(source, path.as_deref())),
            Some(Hit::Link(_)) | None => None,
        };
        let under = Under {
            point: found,
            link,
            image,
        };
        copying::choices(&doc.paints, paint, &under, &doc.code_texts)
    }

    /// The menu's labels: what is at the point, then the whole document's Markdown (if it has
    /// a file). Remembers the choices for `copy_choice`.
    fn prepare_menu(&self, x: f64, y: f64) -> Vec<&'static str> {
        let choices = self.menu_choices(x, y);
        let mut labels: Vec<&'static str> = choices.iter().map(|choice| choice.label).collect();
        if self.path.borrow().is_some() {
            labels.push(COPY_DOCUMENT);
        }
        self.copying.borrow_mut().choices = choices;
        labels
    }

    /// Opens the menu at a point (a right-click).
    pub(in crate::document_view) fn show_menu(&self, x: f64, y: f64) {
        if self.in_motion() {
            return;
        }
        let labels = self.prepare_menu(x, y);
        let separated = labels.len() > 1 && labels.last() == Some(&COPY_DOCUMENT);
        self.open_menu(&labels, separated, (x, y));
    }

    /// Copies the menu's item `index`: a choice, or past them the whole document. Whether
    /// there was such an item.
    pub(in crate::document_view) fn copy_item(&self, index: usize) -> bool {
        let choices = self.copying.borrow().choices.len();
        if index < choices {
            self.copy_choice(index);
            true
        } else if index == choices && self.path.borrow().is_some() {
            self.copy_document();
            true
        } else {
            false
        }
    }

    /// Copies the menu's choice `index`.
    fn copy_choice(&self, index: usize) {
        let text = self
            .copying
            .borrow()
            .choices
            .get(index)
            .map(|choice| choice.text.clone());
        if let Some(text) = text {
            self.obj().clipboard().set_text(&text);
        }
    }

    /// Copies the document's Markdown, as the file has it now.
    fn copy_document(&self) {
        let Some(path) = self.path.borrow().clone() else {
            return;
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                self.obj()
                    .clipboard()
                    .set_text(&String::from_utf8_lossy(&bytes));
            }
            Err(error) => self.notice(&Message::Copy(path.display().to_string(), error.kind())),
        }
    }

    /// For --menu-at: opens the menu at a point, as a right-click does; its labels (spaces as
    /// `_`) and the centre of its first item.
    pub(in crate::document_view) fn probe_menu(&self, x: f64, y: f64) -> String {
        self.show_menu(x, y);
        let labels: Vec<String> = self
            .copying
            .borrow()
            .choices
            .iter()
            .map(|choice| choice.label)
            .chain(self.path.borrow().is_some().then_some(COPY_DOCUMENT))
            .map(|label| label.replace(' ', "_"))
            .collect();
        let first = self
            .menu_item_centre(0)
            .map_or_else(|| "none".to_owned(), |(x, y)| format!("{x},{y}"));
        if labels.is_empty() {
            "none menu_item0=none".to_owned()
        } else {
            format!("{} menu_item0={first}", labels.join("|"))
        }
    }

    /// For --copy: chooses the open menu's item `index`, as the pointer or keys do.
    pub(in crate::document_view) fn probe_copy(&self, index: usize) -> bool {
        self.close_menu();
        self.copy_item(index)
    }

    /// For the probe: the centre of the copy button of the first code block in view.
    pub(in crate::document_view) fn probe_copy_button(&self) -> Option<(i64, i64)> {
        let blocks: Vec<CodeBlockId> = {
            let (top, page) = self.viewport()?;
            let document = self.document.borrow();
            let doc = document.as_ref()?;
            doc.positions
                .visible(top - doc.margin(), page)
                .filter_map(|paint| doc.paints.get(paint)?.code_block)
                .collect()
        };
        blocks.into_iter().find_map(|block| {
            let rect = self.copy_button(block)?;
            Some((rect.x + rect.width / 2, rect.y + rect.height / 2))
        })
    }
}

/// A link's address, ready to paste: a URL as it is, a relative path made absolute (from the
/// document, as following it would), a fragment as `#name`.
fn link_address(
    target: &Target,
    document: Option<&Path>,
    anchors: &std::collections::HashMap<String, galley_core::units::LeafId>,
) -> Option<String> {
    match target {
        Target::Uri(uri) => Some(uri.clone()),
        Target::Relative { path, fragment } => {
            let resolved = document.map_or_else(
                || path.clone(),
                |document| {
                    let is_repository = |folder: &Path| folder.join(".git").exists();
                    links::resolve_path(document, path, &is_repository)
                        .display()
                        .to_string()
                },
            );
            Some(match fragment {
                Some(fragment) => format!("{resolved}#{fragment}"),
                None => resolved,
            })
        }
        Target::Missing(name) => Some(format!("#{name}")),
        // The name it goes by (the shortest, if several name the same place).
        Target::Leaf(leaf) => anchors
            .iter()
            .filter(|(_name, at)| *at == leaf)
            .map(|(name, _at)| name)
            .min_by_key(|name| (name.len(), name.as_str()))
            .map(|name| format!("#{name}")),
    }
}

/// An image's address: a URL as it is, a path made absolute.
fn image_address(source: &ImageSource, document: Option<&Path>) -> String {
    match source {
        ImageSource::Url(url) => url.clone(),
        ImageSource::Path(path) => document.map_or_else(
            || path.clone(),
            |document| {
                let is_repository = |folder: &Path| folder.join(".git").exists();
                links::resolve_path(document, path, &is_repository)
                    .display()
                    .to_string()
            },
        ),
    }
}
