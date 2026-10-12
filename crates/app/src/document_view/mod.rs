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
    /// A view of `paints`, the document at `path` (for its relative links), with each
    /// `<details>` open or not as `details` says, scrolled to paint `start` if given.
    pub(crate) fn new(
        paints: Vec<LeafPaint>,
        anchors: HashMap<String, usize>,
        details: Vec<bool>,
        path: Option<PathBuf>,
        start: Option<usize>,
        probe: Rc<Probe>,
    ) -> Self {
        let view: Self = glib::Object::new();
        view.imp()
            .set_document(paints, anchors, details, path, start, probe);
        view
    }

    /// The link at a point of the view, as `Target::label` gives it ("none" if there is none).
    pub(crate) fn link_label_at(&self, x: f64, y: f64) -> String {
        self.imp()
            .link_at(x, y)
            .map_or_else(|| "none".to_owned(), |target| target.label())
    }

    /// For --click-at: clicks at a point as the pointer would, except that links leaving the
    /// document are not opened. Describes what the click did: a link's label, `summary:N`, or
    /// "none".
    pub(crate) fn probe_click(&self, x: f64, y: f64) -> String {
        self.imp().probe_click(x, y)
    }

    /// The document's height and the paint at the top of the view.
    pub(crate) fn probe_state(&self) -> (i64, usize) {
        self.imp().probe_state()
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
