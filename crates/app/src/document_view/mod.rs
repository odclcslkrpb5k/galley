//! The document widget: a scrollable view that lays out only what is near the viewport.
//!
//! Only leaves near the viewport are laid out; every other leaf's height is estimated until the
//! height worker measures it (docs/plan.md §4.5).

mod imp;

use std::collections::HashMap;
use std::path::PathBuf;
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
    /// A view of `paints`, the document at `path` (for its relative links), scrolled to paint
    /// `start` if given.
    pub(crate) fn new(
        paints: Vec<LeafPaint>,
        anchors: HashMap<String, usize>,
        path: Option<PathBuf>,
        start: Option<usize>,
        probe: Rc<Probe>,
    ) -> Self {
        let view: Self = glib::Object::new();
        view.imp().set_document(paints, anchors, path, start, probe);
        view
    }

    /// The link at a point of the view, as `Target::label` gives it ("none" if there is none).
    pub(crate) fn link_label_at(&self, x: f64, y: f64) -> String {
        self.imp()
            .link_at(x, y)
            .map_or_else(|| "none".to_owned(), |target| target.label())
    }

    /// Lays out the first screen for an expected widget size; see `imp::DocumentView::prepare`.
    pub(crate) fn prepare(&self, width: f64, height: f64) {
        self.imp().prepare(width, height);
    }
}

/// The paint an anchor name names: as written, or with GitHub's `user-content-` prefix.
pub(crate) fn find_anchor(anchors: &HashMap<String, usize>, name: &str) -> Option<usize> {
    let name = name.strip_prefix("user-content-").unwrap_or(name);
    anchors.get(name).copied()
}
