//! Markdown loading for gbk-markdown: bytes in, document model out.
//!
//! This crate has no GTK, Pango, `GLib` or file I/O dependency, so everything in it is tested
//! headless (docs/plan.md §3).

mod decode;

pub use decode::{DecodeNotice, Decoded, decode};
