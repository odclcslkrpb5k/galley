//! Reading a file into what the view shows (docs/plan.md §4.1): on a thread, at startup while
//! GTK starts, and again when the file changes (`document_view` reloads it).

use std::collections::HashMap;
use std::path::Path;

use galley_core::units::{CodeBlockId, LeafId};

use crate::render;

/// Why the file could not be shown.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LoadError {
    #[error("cannot read the file ({0})")]
    Read(std::io::ErrorKind),
    #[error("the file loader stopped unexpectedly")]
    LoaderPanicked,
}

/// What the loader thread makes of the file.
pub(crate) struct Loaded {
    pub(crate) paints: Vec<render::paint::LeafPaint>,
    /// Anchor names and the leaves they name.
    pub(crate) anchors: HashMap<String, LeafId>,
    /// Whether each `<details>` starts open.
    pub(crate) details: Vec<bool>,
    pub(crate) images: Vec<render::image::ImageInfo>,
    pub(crate) code: HashMap<CodeBlockId, render::highlight::CodeSource>,
    /// Every code block's text, for copying.
    pub(crate) code_texts: Vec<String>,
}

/// Reads, parses and paints the document: everything up to Pango, off the main thread.
pub(crate) fn load(path: &Path) -> Result<Loaded, LoadError> {
    let bytes = std::fs::read(path).map_err(|error| LoadError::Read(error.kind()))?;
    let doc = galley_core::load(&bytes);
    let paints = render::paint::paint_document(&doc);
    let anchors = render::paint::anchor_leaves(&doc);
    let details = render::paint::details_open(&doc);
    let images = render::paint::image_infos(&doc);
    let code = render::highlight::code_sources(&doc);
    let code_texts = crate::copying::code_texts(&doc);
    Ok(Loaded {
        paints,
        anchors,
        details,
        images,
        code,
        code_texts,
    })
}
