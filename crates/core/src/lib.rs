//! Markdown loading for gbk-markdown: bytes in, document model out.
//!
//! This crate has no GTK, Pango, `GLib` or file I/O dependency, so everything in it is tested
//! headless (docs/plan.md §3). comrak is used only inside the private `parse` module.

mod builder;
mod decode;
mod html;
mod link;
pub mod model;
mod parse;
pub mod units;

pub use decode::{DecodeNotice, Decoded, decode};
pub use model::Document;

/// Loads a Markdown file's bytes into a document. Never fails: invalid UTF-8 is replaced and
/// noted in `Document::notices`.
#[must_use]
pub fn load(bytes: &[u8]) -> Document {
    let Decoded { text, notice } = decode(bytes);
    let mut builder = builder::Builder::new(parse::github_slugger());
    match notice {
        Some(DecodeNotice::InvalidUtf8Replaced) => {
            builder.notice(model::Notice::InvalidUtf8Replaced);
        }
        None => {}
    }
    parse::parse(&text, builder)
}
