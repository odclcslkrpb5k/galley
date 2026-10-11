//! The first boundary: a file's bytes become text.

use std::borrow::Cow;

use crate::model::Notice;

/// Decodes a file's bytes as UTF-8, dropping a leading byte-order mark and turning CRLF and
/// lone CR line endings into LF (a CR left in text is a paragraph break to Pango).
///
/// Valid UTF-8 with LF endings is borrowed, not copied. Invalid UTF-8 is not refused: the viewer
/// shows what it can, with replacement characters, and returns a notice saying so.
pub(crate) fn decode(bytes: &[u8]) -> (Cow<'_, str>, Option<Notice>) {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let (text, notice) = match std::str::from_utf8(bytes) {
        Ok(text) => (Cow::Borrowed(text), None),
        Err(_invalid) => (
            String::from_utf8_lossy(bytes),
            Some(Notice::InvalidUtf8Replaced),
        ),
    };
    if text.contains('\r') {
        (
            Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n")),
            notice,
        )
    } else {
        (text, notice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_utf8_is_borrowed_unchanged() {
        let (text, notice) = decode("# Héading\n".as_bytes());
        assert_eq!(text, "# Héading\n");
        assert!(matches!(text, Cow::Borrowed(_)));
        assert_eq!(notice, None);
    }

    #[test]
    fn byte_order_mark_is_dropped() {
        assert_eq!(decode(b"\xEF\xBB\xBFtext"), (Cow::Borrowed("text"), None));
    }

    #[test]
    fn invalid_utf8_is_replaced_with_a_notice() {
        let (text, notice) = decode(b"a\xFFb");
        assert_eq!(text, "a\u{FFFD}b");
        assert_eq!(notice, Some(Notice::InvalidUtf8Replaced));
    }

    #[test]
    fn line_endings_become_lf() {
        assert_eq!(decode(b"a\r\nb\rc\n").0, "a\nb\nc\n");
    }

    #[test]
    fn empty_input_is_empty_text() {
        assert_eq!(decode(b""), (Cow::Borrowed(""), None));
        assert_eq!(decode(b"\xEF\xBB\xBF"), (Cow::Borrowed(""), None));
    }
}
