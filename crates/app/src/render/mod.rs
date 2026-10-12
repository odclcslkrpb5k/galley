//! Drawing the document (docs/plan.md §4.5): what each leaf looks like (`paint`, no GTK), where
//! its parts go (`geometry`), how its text is laid out (`text`), its height measured off the
//! main thread (`heights`), and where every leaf starts and how the view stays anchored while
//! heights change (`positions`, over `fenwick`), tables as grids (`table`), images' sizes and
//! formats (`image`, `sniff`) and the scroll indicator (`indicator`).

pub(crate) mod fenwick;
pub(crate) mod geometry;
pub(crate) mod heights;
pub(crate) mod image;
pub(crate) mod indicator;
pub(crate) mod paint;
pub(crate) mod positions;
pub(crate) mod sniff;
pub(crate) mod table;
pub(crate) mod text;
