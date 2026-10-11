use std::cell::{Cell, RefCell};
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
        let width = f64::from(width);
        let height = f64::from(height);
        {
            let mut document = self.document.borrow_mut();
            if let Some(document) = document.as_mut() {
                let text_width = (width - 2.0 * MARGIN).max(1.0);
                if (text_width - document.text_width).abs() > f64::EPSILON {
                    document.reset_for_width(&self.obj(), text_width);
                }
            }
        }
        self.configure_vadjustment(height);
        self.lay_out_near_viewport();
    }

    fn snapshot(&self, snapshot: &gtk::Snapshot) {
        let started = std::time::Instant::now();
        let Some(vadjustment) = self.vadjustment.borrow().clone() else {
            return;
        };
        let document = self.document.borrow();
        let Some(document) = document.as_ref() else {
            return;
        };
        let top = vadjustment.value();
        let height = vadjustment.page_size();
        let color = self.obj().color();
        for leaf in document.extent.overlapping(top, top + height) {
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
        let first_screen = document.extent.overlapping(top, top + 2.0 * height);
        document
            .probe
            .document_drawn(first_screen.len(), document.extent.len());
        document.probe.snapshot_took(started.elapsed());
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

    /// Lays out the paragraphs within the viewport plus one screen below it.
    fn lay_out_near_viewport(&self) {
        let Some(vadjustment) = self.vadjustment.borrow().clone() else {
            return;
        };
        let top = vadjustment.value();
        let page = vadjustment.page_size();
        let changed = {
            let mut document = self.document.borrow_mut();
            let Some(document) = document.as_mut() else {
                return;
            };
            document.lay_out_window(&self.obj(), top, top + 2.0 * page)
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

    /// Lays out paragraphs from the first one overlapping `top` down to `bottom`, one at a time,
    /// so each measured height places the next paragraph exactly. Estimates that run short would
    /// otherwise pull in paragraphs that end up below `bottom`. Returns whether any was laid out.
    fn lay_out_window(&mut self, widget: &super::DocumentView, top: f64, bottom: f64) -> bool {
        let mut changed = false;
        let mut leaf = self.extent.overlapping(top, bottom).start;
        while leaf < self.paragraphs.len() {
            match self.extent.start(leaf) {
                Some(start) if start < bottom => {
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
        let layout = widget.create_pango_layout(Some(&paragraph.text));
        layout.set_width(units::to_pango_units(self.text_width));
        layout.set_wrap(pango::WrapMode::WordChar);
        self.probe.layout_created();
        let (_width, height) = layout.pixel_size();
        self.extent
            .set_height(leaf, f64::from(height) + PARAGRAPH_GAP);
        paragraph.layout = Some(layout);
        true
    }
}
