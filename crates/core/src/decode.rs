//! The first boundary: a file's bytes become text.

use std::borrow::Cow;

/// Text decoded from a file, and why it differs from the file's bytes, if it does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded<'a> {
    pub text: Cow<'a, str>,
    pub notice: Option<DecodeNotice>,
}

/// Why decoded text is not exactly the file's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeNotice {
    /// The file was not valid UTF-8; invalid sequences were replaced with U+FFFD.
    InvalidUtf8Replaced,
}

/// Decodes a file's bytes as UTF-8, dropping a leading byte-order mark.
///
/// Valid UTF-8 is borrowed, not copied. Invalid UTF-8 is not refused: the viewer shows what it
/// can, with replacement characters, and the notice says so.
#[must_use]
pub fn decode(bytes: &[u8]) -> Decoded<'_> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => Decoded {
            text: Cow::Borrowed(text),
            notice: None,
        },
        Err(_invalid) => Decoded {
            text: String::from_utf8_lossy(bytes),
            notice: Some(DecodeNotice::InvalidUtf8Replaced),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_utf8_is_borrowed_unchanged() {
        let decoded = decode("# Héading\n".as_bytes());
        assert_eq!(decoded.text, "# Héading\n");
        assert!(matches!(decoded.text, Cow::Borrowed(_)));
        assert_eq!(decoded.notice, None);
    }

    #[test]
    fn byte_order_mark_is_dropped() {
        let decoded = decode(b"\xEF\xBB\xBFtext");
        assert_eq!(decoded.text, "text");
        assert_eq!(decoded.notice, None);
    }

    #[test]
    fn invalid_utf8_is_replaced_with_a_notice() {
        let decoded = decode(b"a\xFFb");
        assert_eq!(decoded.text, "a\u{FFFD}b");
        assert_eq!(decoded.notice, Some(DecodeNotice::InvalidUtf8Replaced));
    }

    #[test]
    fn a_byte_order_mark_alone_is_empty_text() {
        let decoded = decode(b"\xEF\xBB\xBF");
        assert_eq!(decoded.text, "");
        assert_eq!(decoded.notice, None);
    }

    #[test]
    fn empty_input_is_empty_text() {
        let decoded = decode(b"");
        assert_eq!(decoded.text, "");
        assert_eq!(decoded.notice, None);
    }
}
