//! The document widget: a scrollable view that lays out only what is near the viewport.
//!
//! Only leaves near the viewport are laid out; every other leaf's height is estimated until the
//! height worker measures it (docs/plan.md §4.5).

mod imp;

use std::rc::Rc;

use gtk::glib;
use gtk::subclass::prelude::*;

use crate::probe::Probe;
use crate::render::paint::LeafPaint;

glib::wrapper! {
    pub(crate) struct DocumentView(ObjectSubclass<imp::DocumentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl DocumentView {
    pub(crate) fn new(paints: Vec<LeafPaint>, probe: Rc<Probe>) -> Self {
        let view: Self = glib::Object::new();
        view.imp().set_document(paints, probe);
        view
    }

    /// Lays out the first screen for an expected widget size; see `imp::DocumentView::prepare`.
    pub(crate) fn prepare(&self, width: f64, height: f64) {
        self.imp().prepare(width, height);
    }
}
