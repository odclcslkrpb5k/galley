//! Classifying link and image destinations (docs/plan.md §5).

use crate::model::LinkTarget;

/// What a destination is for: GitHub's sanitiser allows different schemes for each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Use {
    Link,
    Image,
}

impl Use {
    fn allows(self, scheme: &str) -> bool {
        let scheme = scheme.to_ascii_lowercase();
        match self {
            Self::Link => matches!(scheme.as_str(), "http" | "https" | "mailto"),
            Self::Image => matches!(scheme.as_str(), "http" | "https"),
        }
    }
}

/// Classifies a destination as written in Markdown or HTML, or `None` where GitHub's sanitiser
/// would drop it: a protocol it does not allow for this use (`javascript:`, `data:`, `file:`,
/// or something unparseable like `[https://x](y)`). As there, the protocol is whatever comes
/// before the first `:` that has no `/` or `#` before it.
pub(crate) fn classify(destination: &str, used_for: Use) -> Option<LinkTarget> {
    let trimmed = destination.trim_start();
    let colon = trimmed.find(':');
    let path_start = trimmed.find(['/', '#']);
    if let Some(colon) = colon
        && path_start.is_none_or(|start| colon < start)
        && !used_for.allows(trimmed.get(..colon).unwrap_or_default())
    {
        return None;
    }
    Some(target(destination))
}

fn target(destination: &str) -> LinkTarget {
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
/// `+`, `-` or `.`, then `:`). Only allowed schemes reach here (see `classify`).
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

    fn link(destination: &str) -> LinkTarget {
        classify(destination, Use::Link).expect("allowed")
    }

    #[test]
    fn fragments() {
        assert_eq!(link("#usage"), LinkTarget::Fragment("usage".to_owned()));
        assert_eq!(
            link("#h%C3%A9ading"),
            LinkTarget::Fragment("héading".to_owned())
        );
        assert_eq!(link("#"), LinkTarget::Fragment(String::new()));
        assert_eq!(link("#a:b"), LinkTarget::Fragment("a:b".to_owned()));
    }

    #[test]
    fn absolute_urls() {
        for url in [
            "https://example.com/a%20b",
            "HTTP://example.com",
            "mailto:me@example.com",
        ] {
            assert_eq!(link(url), LinkTarget::Absolute(url.to_owned()));
        }
        assert_eq!(
            classify("http://x/y.png", Use::Image),
            Some(LinkTarget::Absolute("http://x/y.png".to_owned()))
        );
    }

    #[test]
    fn protocols_github_drops_are_not_links() {
        for url in [
            "javascript:alert(1)",
            " JavaScript:alert(1)",
            "ftp://x",
            "file:///etc/passwd",
            "data:text/html,x",
            "a+b.c-d:x",
            "[https://x](https://y)",
            "1x:y",
            ":y",
            "a?b:c",
        ] {
            assert_eq!(classify(url, Use::Link), None, "{url}");
        }
        assert_eq!(classify("mailto:a@b", Use::Image), None);
        assert_eq!(classify("data:image/png;base64,x", Use::Image), None);
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            link("docs/my%20doc.md#Section"),
            relative("docs/my doc.md", None, Some("Section"))
        );
        assert_eq!(
            link("/root.md?plain=1#L5"),
            relative("/root.md", Some("plain=1"), Some("L5"))
        );
        assert_eq!(link("img.png"), relative("img.png", None, None));
        assert_eq!(
            link("//example.com/x"),
            LinkTarget::Absolute("https://example.com/x".to_owned())
        );
        // A colon after a slash or hash is part of the path, not a protocol.
        assert_eq!(link("a/b:c"), relative("a/b:c", None, None));
        assert_eq!(link("a#b:c"), relative("a", None, Some("b:c")));
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
