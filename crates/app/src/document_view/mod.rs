//! The document widget: a scrollable view that lays out only what is near the viewport.
//!
//! M0 spike (docs/plan.md §10): paragraphs of plain text. A paragraph's height is estimated until
//! it is laid out, and only paragraphs within the viewport plus one screen are laid out.

mod imp;

use std::rc::Rc;

use gtk::glib;
use gtk::subclass::prelude::*;

use crate::probe::Probe;

glib::wrapper! {
    pub(crate) struct DocumentView(ObjectSubclass<imp::DocumentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl DocumentView {
    pub(crate) fn new(paragraphs: Vec<String>, probe: Rc<Probe>) -> Self {
        let view: Self = glib::Object::new();
        view.imp().set_document(paragraphs, probe);
        view
    }

    /// Lays out the first screen for an expected widget size; see `imp::DocumentView::prepare`.
    pub(crate) fn prepare(&self, width: f64, height: f64) {
        self.imp().prepare(width, height);
    }
}
