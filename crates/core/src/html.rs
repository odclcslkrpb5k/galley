//! Tokenising raw HTML found in Markdown (docs/plan.md §5.1).
//!
//! The tokens are our own plain types, so html5gum stays an implementation detail of this file.
//! What the tags mean (styles, links, alignment, details) is decided in `builder`.

use html5gum::{Token, Tokenizer};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HtmlToken {
    Start {
        /// Lower case.
        name: String,
        attributes: Vec<(String, String)>,
        self_closing: bool,
    },
    End {
        /// Lower case.
        name: String,
    },
    /// Text, with character references already decoded.
    Text(String),
}

impl HtmlToken {
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
}

/// Splits HTML into tags and text. Comments, doctypes and tokenizer errors are dropped: none of
/// them is ever shown.
pub(crate) fn tokenize(html: &str) -> Vec<HtmlToken> {
    let mut tokens = Vec::new();
    for token in Tokenizer::new(html) {
        let Ok(token) = token;
        match token {
            Token::StartTag(tag) => tokens.push(HtmlToken::Start {
                name: lossy(&tag.name).to_ascii_lowercase(),
                attributes: tag
                    .attributes
                    .iter()
                    .map(|(name, value)| (lossy(name).to_ascii_lowercase(), lossy(value)))
                    .collect(),
                self_closing: tag.self_closing,
            }),
            Token::EndTag(tag) => tokens.push(HtmlToken::End {
                name: lossy(&tag.name).to_ascii_lowercase(),
            }),
            Token::String(text) => {
                let text = lossy(&text);
                // The tokenizer may split text; join adjacent pieces.
                if let Some(HtmlToken::Text(previous)) = tokens.last_mut() {
                    previous.push_str(&text);
                } else {
                    tokens.push(HtmlToken::Text(text));
                }
            }
            Token::Comment(_) | Token::Doctype(_) | Token::Error(_) => {}
        }
    }
    tokens
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(name: &str, attributes: &[(&str, &str)]) -> HtmlToken {
        HtmlToken::Start {
            name: name.to_owned(),
            attributes: attributes
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            self_closing: false,
        }
    }

    #[test]
    fn tags_text_and_entities() {
        assert_eq!(
            tokenize(r#"<P Align="center">a &amp; b</p>"#),
            vec![
                start("p", &[("align", "center")]),
                HtmlToken::Text("a & b".to_owned()),
                HtmlToken::End {
                    name: "p".to_owned()
                },
            ]
        );
    }

    #[test]
    fn comments_are_dropped() {
        assert_eq!(
            tokenize("a<!-- hidden -->b"),
            vec![HtmlToken::Text("ab".to_owned())]
        );
    }

    #[test]
    fn self_closing_and_attributes() {
        let tokens = tokenize(r#"<img src="x.png" alt="X"/>"#);
        assert_eq!(tokens.len(), 1);
        let img = tokens.first().expect("one token");
        assert_eq!(img.attribute("src"), Some("x.png"));
        assert_eq!(img.attribute("alt"), Some("X"));
        assert_eq!(img.attribute("width"), None);
        assert!(matches!(
            img,
            HtmlToken::Start {
                self_closing: true,
                ..
            }
        ));
    }
}
