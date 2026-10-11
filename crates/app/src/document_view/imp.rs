use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, pango};

use crate::extent::Extent;
use crate::probe::Probe;
use crate::units;

/// Space around and between paragraphs, in logical pixels.
const MARGIN: f64 = 24.0;
const PARAGRAPH_GAP: f64 = 12.0;
/// How far below the viewport to lay out ahead, in viewport heights.
const LAY_OUT_AHEAD: f64 = 1.0;

/// The span of extent coordinates shown by a viewport scrolled to `top` with height `page`,
/// extended `ahead` viewport heights below it.
///
/// Extent coordinates start at the first paragraph; the view draws them `MARGIN` lower, so the
/// visible view `[0, page)` is the extent span `[top - MARGIN, top - MARGIN + page)`. Drawing,
/// layout and the probe all use this one conversion.
fn extent_span(top: f64, page: f64, ahead: f64) -> Range<f64> {
    let start = top - MARGIN;
    start..start + page * (1.0 + ahead)
}

#[derive(Debug, glib::Properties)]
#[properties(wrapper_type = super::DocumentView)]
pub(crate) struct DocumentView {
    #[property(override_interface = gtk::Scrollable, get, set = Self::set_hadjustment)]
    hadjustment: RefCell<Option<gtk::Adjustment>>,
    #[property(override_interface = gtk::Scrollable, get, set = Self::set_vadjustment)]
    vadjustment: RefCell<Option<gtk::Adjustment>>,
    #[property(override_interface = gtk::Scrollable, get, set, builder(gtk::ScrollablePolicy::Minimum))]
    hscroll_policy: Cell<gtk::ScrollablePolicy>,
    #[property(override_interface = gtk::Scrollable, get, set, builder(gtk::ScrollablePolicy::Minimum))]
    vscroll_policy: Cell<gtk::ScrollablePolicy>,
    vadjustment_handler: RefCell<Option<glib::SignalHandlerId>>,
    document: RefCell<Option<Document>>,
}

// By hand: gtk::ScrollablePolicy has no Default.
impl Default for DocumentView {
    fn default() -> Self {
        Self {
            hadjustment: RefCell::default(),
            vadjustment: RefCell::default(),
            hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
            vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
            vadjustment_handler: RefCell::default(),
            document: RefCell::default(),
        }
    }
}

#[derive(Debug)]
struct Document {
    paragraphs: Vec<Paragraph>,
    extent: Extent,
    /// The text width the layouts and estimates were made for, in logical pixels.
    text_width: f64,
    probe: Rc<Probe>,
}

#[derive(Debug)]
struct Paragraph {
    text: String,
    /// Characters, for height estimates.
    chars: usize,
    layout: Option<pango::Layout>,
}

#[glib::object_subclass]
impl ObjectSubclass for DocumentView {
    const NAME: &'static str = "GbkDocumentView";
    type Type = super::DocumentView;
    type ParentType = gtk::Widget;
    type Interfaces = (gtk::Scrollable,);
}

#[glib::derived_properties]
impl ObjectImpl for DocumentView {}

impl WidgetImpl for DocumentView {
    fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
        (0, 0, -1, -1)
    }

    fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
        self.set_text_width(f64::from(width));
        self.configure_vadjustment(f64::from(height));
        self.lay_out_near_viewport();
    }

    fn snapshot(&self, snapshot: &gtk::Snapshot) {
        let Some(vadjustment) = self.vadjustment.borrow().clone() else {
            return;
        };
        let document = self.document.borrow();
        let Some(document) = document.as_ref() else {
            return;
        };
        let top = vadjustment.value();
        let span = extent_span(top, vadjustment.page_size(), 0.0);
        let color = self.obj().color();
        for leaf in document.extent.overlapping(span.start, span.end) {
            let (
                Some(start),
                Some(Paragraph {
                    layout: Some(layout),
                    ..
                }),
            ) = (document.extent.start(leaf), document.paragraphs.get(leaf))
            else {
                continue;
            };
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                units::to_f32(MARGIN),
                units::to_f32(MARGIN + start - top),
            ));
            snapshot.append_layout(layout, &color);
            snapshot.restore();
        }
        document.probe.document_drawn(document.extent.len());
    }
}

impl ScrollableImpl for DocumentView {}

impl DocumentView {
    pub(super) fn set_document(&self, paragraphs: Vec<String>, probe: Rc<Probe>) {
        let paragraphs: Vec<Paragraph> = paragraphs
            .into_iter()
            .map(|text| Paragraph {
                chars: text.chars().count(),
                text,
                layout: None,
            })
            .collect();
        let extent = Extent::new(vec![0.0; paragraphs.len()]);
        *self.document.borrow_mut() = Some(Document {
            paragraphs,
            extent,
            text_width: 0.0,
            probe,
        });
        self.obj().queue_allocate();
    }

    /// Lays out the first screen for an expected size, before GTK allocates the real one.
    ///
    /// Called right after the window is presented: the layout work then overlaps the window
    /// manager mapping the window instead of following it. If the real width differs, the
    /// width-reset path lays out again (the probe counts that).
    pub(super) fn prepare(&self, width: f64, height: f64) {
        self.set_text_width(width);
        let mut document = self.document.borrow_mut();
        if let Some(document) = document.as_mut() {
            let span = extent_span(0.0, height, LAY_OUT_AHEAD);
            document.lay_out_span(&self.obj(), span);
        }
    }

    fn set_text_width(&self, widget_width: f64) {
        let mut document = self.document.borrow_mut();
        if let Some(document) = document.as_mut() {
            let text_width = (widget_width - 2.0 * MARGIN).max(1.0);
            if (text_width - document.text_width).abs() > f64::EPSILON {
                document.reset_for_width(&self.obj(), text_width);
            }
        }
    }

    fn set_hadjustment(&self, adjustment: Option<gtk::Adjustment>) {
        // Paragraphs wrap to the width, so there is nothing to scroll horizontally.
        self.hadjustment.replace(adjustment);
    }

    fn set_vadjustment(&self, adjustment: Option<gtk::Adjustment>) {
        if let (Some(old), Some(handler)) = (
            self.vadjustment.borrow().as_ref(),
            self.vadjustment_handler.take(),
        ) {
            old.disconnect(handler);
        }
        if let Some(adjustment) = adjustment.as_ref() {
            let handler = adjustment.connect_value_changed(glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                move |_adjustment| {
                    view.imp().lay_out_near_viewport();
                    view.queue_draw();
                }
            ));
            self.vadjustment_handler.replace(Some(handler));
        }
        self.vadjustment.replace(adjustment);
        self.obj().queue_allocate();
    }

    fn configure_vadjustment(&self, page: f64) {
        let Some(vadjustment) = self.vadjustment.borrow().clone() else {
            return;
        };
        let total = self
            .document
            .borrow()
            .as_ref()
            .map_or(0.0, |document| document.extent.total() + 2.0 * MARGIN);
        let upper = total.max(page);
        vadjustment.configure(
            vadjustment.value().min(upper - page).max(0.0),
            0.0,
            upper,
            page * 0.1,
            page * 0.9,
            page,
        );
    }

    /// Lays out the paragraphs within the viewport and `LAY_OUT_AHEAD` screens below it.
    fn lay_out_near_viewport(&self) {
        let Some(vadjustment) = self.vadjustment.borrow().clone() else {
            return;
        };
        let page = vadjustment.page_size();
        let span = extent_span(vadjustment.value(), page, LAY_OUT_AHEAD);
        let changed = {
            let mut document = self.document.borrow_mut();
            let Some(document) = document.as_mut() else {
                return;
            };
            document.lay_out_span(&self.obj(), span)
        };
        if changed {
            self.configure_vadjustment(page);
        }
    }
}

impl Document {
    /// Drops every layout and estimates every height for a new text width.
    fn reset_for_width(&mut self, widget: &super::DocumentView, text_width: f64) {
        self.probe.width_reset();
        let metrics = widget.pango_context().metrics(None, None);
        let char_width = units::from_pango_units(metrics.approximate_char_width()).max(1.0);
        let line_height = units::from_pango_units(metrics.height()).max(1.0);
        let chars_per_line = (text_width / char_width).max(1.0);
        let heights = self
            .paragraphs
            .iter_mut()
            .map(|paragraph| {
                paragraph.layout = None;
                let lines = (units::count_to_f64(paragraph.chars) / chars_per_line)
                    .ceil()
                    .max(1.0);
                lines * line_height + PARAGRAPH_GAP
            })
            .collect();
        self.extent = Extent::new(heights);
        self.text_width = text_width;
    }

    /// Lays out paragraphs from the first one overlapping `span` down to its end, one at a time,
    /// so each measured height places the next paragraph exactly. Estimates that run short would
    /// otherwise pull in paragraphs that end up below the span. Returns whether any was laid out.
    fn lay_out_span(&mut self, widget: &super::DocumentView, span: Range<f64>) -> bool {
        let mut changed = false;
        let mut leaf = self.extent.overlapping(span.start, span.end).start;
        while leaf < self.paragraphs.len() {
            match self.extent.start(leaf) {
                Some(start) if start < span.end => {
                    changed |= self.lay_out_one(widget, leaf);
                    leaf += 1;
                }
                Some(_) | None => break,
            }
        }
        changed
    }

    /// Lays out one paragraph if it has no layout yet. Returns whether it did.
    fn lay_out_one(&mut self, widget: &super::DocumentView, leaf: usize) -> bool {
        let Some(paragraph) = self.paragraphs.get_mut(leaf) else {
            return false;
        };
        if paragraph.layout.is_some() {
            return false;
        }
        let layout = new_layout(widget, &paragraph.text, &self.probe);
        layout.set_width(units::to_pango_units(self.text_width));
        layout.set_wrap(pango::WrapMode::WordChar);
        let (_width, height) = layout.pixel_size();
        self.extent
            .set_height(leaf, f64::from(height) + PARAGRAPH_GAP);
        paragraph.layout = Some(layout);
        true
    }
}

/// The one way the viewer creates a Pango layout, so the probe sees all layout work.
fn new_layout(widget: &super::DocumentView, text: &str, probe: &Probe) -> pango::Layout {
    probe.layout_created(text.len());
    #[expect(
        clippy::disallowed_methods,
        reason = "the counted constructor; everything else must call this"
    )]
    widget.create_pango_layout(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_visible_span_is_shifted_by_the_margin() {
        // At the top, the first MARGIN pixels of the view are margin, not document.
        assert_eq!(extent_span(0.0, 100.0, 0.0), -MARGIN..100.0 - MARGIN);
        // Scrolled to 100, view row 0 shows extent row 100 - MARGIN.
        assert_eq!(
            extent_span(100.0, 50.0, 0.0),
            100.0 - MARGIN..150.0 - MARGIN
        );
    }

    #[test]
    fn lay_out_ahead_extends_only_the_bottom() {
        assert_eq!(
            extent_span(100.0, 50.0, 1.0),
            100.0 - MARGIN..200.0 - MARGIN
        );
    }
}
