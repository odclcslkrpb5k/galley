//! Classifying link and image destinations (docs/plan.md §5).

use crate::model::LinkTarget;

/// Classifies a destination as written in Markdown or HTML.
pub(crate) fn classify(destination: &str) -> LinkTarget {
    if let Some(fragment) = destination.strip_prefix('#') {
        return LinkTarget::Fragment(percent_decode(fragment));
    }
    if destination.starts_with("//") {
        // Protocol-relative: GitHub pages are served over https.
        return LinkTarget::Absolute(format!("https:{destination}"));
    }
    if has_scheme(destination) {
        return LinkTarget::Absolute(destination.to_owned());
    }
    let (rest, fragment) = match destination.split_once('#') {
        Some((rest, fragment)) => (rest, Some(percent_decode(fragment))),
        None => (destination, None),
    };
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query.to_owned())),
        None => (rest, None),
    };
    LinkTarget::Relative {
        path: percent_decode(path),
        query,
        fragment,
    }
}

/// Whether `destination` starts with a URI scheme (RFC 3986: a letter, then letters, digits,
/// `+`, `-` or `.`, then `:`). `C:\path` counts as a scheme too, which is harmless here: it is
/// handed to the system to open, like any other absolute URL.
fn has_scheme(destination: &str) -> bool {
    let Some((scheme, _rest)) = destination.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Decodes `%XX` escapes. Malformed escapes are kept as written; decoded bytes that are not
/// UTF-8 become U+FFFD.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'%'
            && let [high, low, tail @ ..] = after
            && let (Some(high), Some(low)) = (hex_value(*high), hex_value(*low))
        {
            out.push(high << 4 | low);
            rest = tail;
            continue;
        }
        out.push(byte);
        rest = after;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative(path: &str, query: Option<&str>, fragment: Option<&str>) -> LinkTarget {
        LinkTarget::Relative {
            path: path.to_owned(),
            query: query.map(str::to_owned),
            fragment: fragment.map(str::to_owned),
        }
    }

    #[test]
    fn fragments() {
        assert_eq!(classify("#usage"), LinkTarget::Fragment("usage".to_owned()));
        assert_eq!(
            classify("#h%C3%A9ading"),
            LinkTarget::Fragment("héading".to_owned())
        );
        assert_eq!(classify("#"), LinkTarget::Fragment(String::new()));
    }

    #[test]
    fn absolute_urls() {
        for url in [
            "https://example.com/a%20b",
            "mailto:me@example.com",
            "ftp://x",
            "a+b.c-d:x",
        ] {
            assert_eq!(classify(url), LinkTarget::Absolute(url.to_owned()));
        }
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            classify("docs/my%20doc.md#Section"),
            relative("docs/my doc.md", None, Some("Section"))
        );
        assert_eq!(
            classify("/root.md?plain=1#L5"),
            relative("/root.md", Some("plain=1"), Some("L5"))
        );
        assert_eq!(classify("img.png"), relative("img.png", None, None));
        assert_eq!(
            classify("//example.com/x"),
            LinkTarget::Absolute("https://example.com/x".to_owned())
        );
        // Not schemes: a digit first, or nothing before the colon.
        assert_eq!(classify("1x:y"), relative("1x:y", None, None));
        assert_eq!(classify(":y"), relative(":y", None, None));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%FF"), "\u{FFFD}");
        assert_eq!(percent_decode("%2f%2F%e2%82%ac"), "//€");
        for (digit, value) in [
            (b'0', 0),
            (b'9', 9),
            (b'a', 10),
            (b'f', 15),
            (b'A', 10),
            (b'F', 15),
        ] {
            assert_eq!(hex_value(digit), Some(value));
        }
        assert_eq!(hex_value(b'g'), None);
    }
}
