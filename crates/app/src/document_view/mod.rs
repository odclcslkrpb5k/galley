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

use crate::highlighting::HighlightCounts;
use crate::images::ImageCounts;
use crate::probe::Probe;
use crate::render::highlight::CodeSource;
use crate::render::image::ImageInfo;
use crate::render::paint::LeafPaint;
use galley_core::model::anchor_name;
use galley_core::units::{CodeBlockId, LeafId};

/// A loaded document, as the view takes it.
#[derive(Debug)]
pub(crate) struct Opened {
    pub(crate) paints: Vec<LeafPaint>,
    /// Anchor names and the leaves they name.
    pub(crate) anchors: HashMap<String, LeafId>,
    /// Whether each `<details>` starts open.
    pub(crate) details: Vec<bool>,
    pub(crate) images: Vec<ImageInfo>,
    /// The code to highlight.
    pub(crate) code: HashMap<CodeBlockId, CodeSource>,
    /// Every code block's text, for copying.
    pub(crate) code_texts: Vec<String>,
    /// The document's file, which relative links and images start from.
    pub(crate) path: Option<PathBuf>,
    /// What to show at the top first.
    pub(crate) start: Option<Start>,
    /// Fetch no remote images.
    pub(crate) offline: bool,
    /// Text size, as a factor.
    pub(crate) zoom: f64,
    /// Options a document opened from a link is started with (`--offline`, `--theme`).
    pub(crate) viewer_args: Vec<String>,
}

/// Where a document opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Start {
    /// A heading or `<a id>` (`--anchor`).
    Leaf(LeafId),
    /// A paint, by position: where a reloaded document was.
    Paint(usize),
}

glib::wrapper! {
    pub(crate) struct DocumentView(ObjectSubclass<imp::DocumentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl DocumentView {
    pub(crate) fn new(document: Opened, probe: Rc<Probe>) -> Self {
        let view: Self = glib::Object::new();
        view.imp().set_document(document, probe);
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

    /// For --press: presses a key (`Page_Down`, `ctrl+plus`) as the keyboard would; describes
    /// what it did.
    pub(crate) fn probe_press(&self, key: &str) -> String {
        self.imp().probe_press(key)
    }

    /// For --menu-at: the copy menu's labels at a point.
    pub(crate) fn probe_menu(&self, x: f64, y: f64) -> String {
        self.imp().probe_menu(x, y)
    }

    /// For --copy: copies the menu's item `index`; whether there was one.
    pub(crate) fn probe_copy(&self, index: usize) -> bool {
        self.imp().probe_copy(index)
    }

    /// The centre of the first code block's copy button in view.
    pub(crate) fn probe_copy_button(&self) -> Option<(i64, i64)> {
        self.imp().probe_copy_button()
    }

    /// The document's height and the paint at the top of the view.
    pub(crate) fn probe_state(&self) -> (i64, usize) {
        self.imp().probe_state()
    }

    /// Images shown, failed, and still loading.
    pub(crate) fn probe_images(&self) -> ImageCounts {
        self.imp().probe_images()
    }

    /// Code blocks highlighted, plain, and still being read; and paints waiting to be drawn
    /// again in colour.
    pub(crate) fn probe_highlights(&self) -> (HighlightCounts, usize) {
        self.imp().probe_highlights()
    }

    /// The file shown, if any.
    pub(crate) fn path(&self) -> Option<PathBuf> {
        self.imp().path.borrow().clone()
    }

    /// Shows another file in this view (dropped, or chosen with Ctrl+O), from its top.
    pub(crate) fn open(&self, path: PathBuf) {
        self.imp().open_file(path);
    }

    /// Lays out the first screen for an expected widget size; see `imp::DocumentView::prepare`.
    pub(crate) fn prepare(&self, width: f64, height: f64) {
        self.imp().prepare(width, height);
    }
}

/// The leaf a `#fragment` names, by the same rule as `Document::anchor_leaf`.
/// Opens a file in a new viewer window, with these options (`--offline`, `--theme`).
pub(crate) fn open_in_new_window(path: &std::path::Path, args: &[String]) -> Result<(), String> {
    imp::open_in_viewer(path, None, args).map_err(|error| error.to_string())
}

pub(crate) fn anchor_leaf(anchors: &HashMap<String, LeafId>, fragment: &str) -> Option<LeafId> {
    anchors.get(anchor_name(fragment)).copied()
}
