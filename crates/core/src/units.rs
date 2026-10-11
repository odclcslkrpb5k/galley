//! Quantities that are all `usize` underneath and must not be mixed up (docs/plan.md §4.2).
//!
//! Each is a newtype with one way in and one way out, so a byte offset cannot be passed where a
//! line number or a table index is expected.

/// Defines an index newtype: built from a `usize` inside this crate, read with `index()`.
macro_rules! index_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(usize);

        impl $name {
            pub(crate) const fn new(index: usize) -> Self {
                Self(index)
            }

            /// The position this stands for, for indexing the matching `Vec` in a `Document`.
            #[must_use]
            pub const fn index(self) -> usize {
                self.0
            }
        }
    };
}

index_type!(
    /// A byte offset into one `Inlines::text` (or a code block's text).
    ByteOffset
);
index_type!(
    /// A line of a code block, counted from 0.
    LineNo
);
index_type!(
    /// A row of a table body, counted from 0.
    RowNo
);
index_type!(
    /// A leaf in `Document::leaves`.
    LeafId
);
index_type!(
    /// A link in `Document::links`.
    LinkId
);
index_type!(
    /// An image in `Document::images`.
    ImageId
);
index_type!(
    /// A footnote in `Document::footnotes`.
    FootnoteId
);
index_type!(
    /// A table in `Document::tables`.
    TableId
);
index_type!(
    /// A code block in `Document::code_blocks`.
    CodeBlockId
);
index_type!(
    /// A heading anchor in `Document::anchors`.
    AnchorId
);
index_type!(
    /// A `<details>` element; leaves inside it carry the same id.
    DetailsId
);

impl ByteOffset {
    /// The offset just past `text`: where the next text appended to it starts.
    pub(crate) const fn end_of(text: &str) -> Self {
        Self(text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_round_trips() {
        assert_eq!(LeafId::new(7).index(), 7);
        assert_eq!(ByteOffset::end_of("héllo").index(), 6);
    }
}
