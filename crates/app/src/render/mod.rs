//! Drawing the document (docs/plan.md §4.5): what each leaf looks like (`paint`, no GTK), how
//! its text is laid out (`text`), its height measured off the main thread (`heights`), and
//! where every leaf starts (`fenwick`).

pub(crate) mod fenwick;
pub(crate) mod heights;
pub(crate) mod paint;
pub(crate) mod text;
