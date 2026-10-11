//! Tokenising raw HTML found in Markdown (docs/plan.md §5.1).
//!
//! The tokens are our own plain types, so html5gum stays an implementation detail of this file.
//! What the tags mean (styles, links, alignment, details) is decided in `builder`.

use html5gum::{DefaultEmitter, Token, Tokenizer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HtmlToken<'a> {
    Start {
        /// Lower case (html5gum lowercases names).
        name: String,
        attributes: Vec<(String, String)>,
        self_closing: bool,
        /// The tag as written, for tags GitHub shows as text.
        source: &'a str,
    },
    End {
        /// Lower case.
        name: String,
        /// The tag as written.
        source: &'a str,
    },
    /// Text, with character references already decoded.
    Text(String),
}

impl HtmlToken<'_> {
    /// An attribute of a start tag, by lower-case name.
    pub(crate) fn attribute(&self, wanted: &str) -> Option<&str> {
        match self {
            Self::Start { attributes, .. } => attributes
                .iter()
                .find(|(name, _value)| name == wanted)
                .map(|(_name, value)| value.as_str()),
            Self::End { .. } | Self::Text(_) => None,
        }
    }

    pub(crate) fn has_attribute(&self, wanted: &str) -> bool {
        match self {
            Self::Start { attributes, .. } => attributes.iter().any(|(name, _)| name == wanted),
            Self::End { .. } | Self::Text(_) => false,
        }
    }
}

/// Splits HTML into tags and text, streaming. Comments, doctypes and tokenizer errors are
/// dropped: none of them is ever shown.
pub(crate) fn tokens(html: &str) -> impl Iterator<Item = HtmlToken<'_>> {
    Tokenizer::new_with_emitter(html, DefaultEmitter::<usize>::new_with_span()).filter_map(
        move |token| {
            let Ok(token) = token;
            let source = |start: usize, end: usize| html.get(start..end).unwrap_or("");
            match token {
                Token::StartTag(tag) => Some(HtmlToken::Start {
                    name: lossy(&tag.name),
                    attributes: tag
                        .attributes
                        .iter()
                        .map(|(name, value)| (lossy(name), lossy(value)))
                        .collect(),
                    self_closing: tag.self_closing,
                    source: source(tag.span.start, tag.span.end),
                }),
                Token::EndTag(tag) => Some(HtmlToken::End {
                    name: lossy(&tag.name),
                    source: source(tag.span.start, tag.span.end),
                }),
                Token::String(text) => Some(HtmlToken::Text(lossy(&text))),
                Token::Comment(_) | Token::Doctype(_) | Token::Error(_) => None,
            }
        },
    )
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(html: &str) -> Vec<HtmlToken<'_>> {
        tokens(html).collect()
    }

    #[test]
    fn tags_text_and_entities() {
        let tokens = collect(r#"<P Align="center">a &amp; b</p>"#);
        assert_eq!(tokens.len(), 3);
        assert!(matches!(
            tokens.first(),
            Some(HtmlToken::Start { name, source, .. }) if name == "p" && *source == r#"<P Align="center">"#
        ));
        assert_eq!(
            tokens.first().and_then(|t| t.attribute("align")),
            Some("center")
        );
        assert_eq!(tokens.get(1), Some(&HtmlToken::Text("a & b".to_owned())));
        assert!(matches!(
            tokens.get(2),
            Some(HtmlToken::End { name, source }) if name == "p" && *source == "</p>"
        ));
    }

    #[test]
    fn comments_are_dropped() {
        let texts: String = tokens("a<!-- hidden -->b")
            .map(|token| match token {
                HtmlToken::Text(text) => text,
                HtmlToken::Start { .. } | HtmlToken::End { .. } => "<tag>".to_owned(),
            })
            .collect();
        assert_eq!(texts, "ab");
    }

    #[test]
    fn self_closing_and_attributes() {
        let tokens = collect(r#"<img src="x.png" alt="X" hidden/>"#);
        let img = tokens.first().expect("one token");
        assert_eq!(img.attribute("src"), Some("x.png"));
        assert_eq!(img.attribute("alt"), Some("X"));
        assert_eq!(img.attribute("width"), None);
        assert!(img.has_attribute("hidden"));
        assert!(matches!(
            img,
            HtmlToken::Start {
                self_closing: true,
                ..
            }
        ));
    }
}
