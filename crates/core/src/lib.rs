//! Markdown loading for gbk-markdown: bytes in, document model out.
//!
//! This crate has no GTK, Pango, `GLib` or file I/O dependency, so everything in it is tested
//! headless (docs/plan.md §3). comrak is used only inside the private `parse` module.

mod builder;
mod decode;
mod front_matter;
mod html;
mod link;
pub mod model;
mod parse;
pub mod units;

pub use model::Document;

/// Loads a Markdown file's bytes into a document. Never fails: invalid UTF-8 is replaced and
/// noted in `Document::notices`.
#[must_use]
pub fn load(bytes: &[u8]) -> Document {
    let (text, notice) = decode::decode(bytes);
    let mut builder = builder::Builder::new(parse::github_slugger());
    if let Some(notice) = notice {
        builder.notice(notice);
    }
    parse::parse(&text, builder)
}
