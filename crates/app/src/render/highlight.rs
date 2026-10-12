//! Syntax highlighting as plain data (docs/plan.md §4.5, "As built in M5"): which language a code block is in,
//! the kinds of token the highlighter finds, and the tokens found so far, which the widget's
//! typesetter turns into colours. Pure, so it is unit-tested; reading code with syntect, on its
//! own thread, and the main thread's record of it are `crate::highlighting`.
//!
//! Tokens are colours only, never a weight or a slant: a colour cannot change a layout's size,
//! so a block's height is the same before and after it is highlighted, and the height worker
//! measures without highlights (`--check-heights --wait-highlights` checks this).

use std::collections::HashMap;

use gbk_markdown_core::Document;
use gbk_markdown_core::units::CodeBlockId;

use super::paint::{AttrKind, TextAttr};

/// What a token is, as GitHub's colours tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TokenClass {
    Comment,
    Keyword,
    String,
    /// Escapes and regular expressions.
    Escape,
    /// Numbers, named constants, `true`, `self`.
    Constant,
    /// Function names, and attribute names in markup.
    Function,
    /// Type and class names.
    Type,
    /// Tag names in markup.
    Tag,
    /// Parameters.
    Variable,
    /// Lines a diff adds, and removes (or text the grammar marks invalid).
    Inserted,
    Deleted,
}

/// A run of a code block's text, `start..end` in bytes of the block's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) class: TokenClass,
}

/// Where a code paint's text is in its block: it starts `start` bytes into the block's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodeSpan {
    pub(crate) block: CodeBlockId,
    pub(crate) start: usize,
}

/// How to highlight a block: the name to look its grammar up by, and whether it is a shell
/// session (only the commands after a `$ ` or `% ` prompt are shell; the rest is output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Language {
    pub(crate) name: String,
    pub(crate) session: bool,
}

/// A code block to highlight: its language and text.
#[derive(Debug, Clone)]
pub(crate) struct CodeSource {
    pub(crate) language: Language,
    pub(crate) text: String,
}

/// Most tokens coloured in one paint; the rest of it is plain. Each is a Pango attribute, and
/// a 2 KiB paint of 1366 (a Perl grammar on alternations) took 2-6 ms to lay out and draw, more
/// than a prefetch slice (M5 review); ordinary code has about 230.
pub(crate) const MAX_PAINT_TOKENS: usize = 512;

/// Info strings GitHub shows plain.
const PLAIN: [&str; 8] = [
    "",
    "text",
    "plaintext",
    "plain",
    "txt",
    "nohighlight",
    "math",
    "log",
];
/// Info strings for a shell session.
pub(crate) const SESSIONS: [&str; 4] = ["console", "shellsession", "shell-session", "sh-session"];
/// Names GitHub knows that the grammars do not, and the grammars' names for them.
pub(crate) const ALIASES: [(&str, &str); 15] = [
    ("shell", "bash"),
    ("shell-script", "bash"),
    ("golang", "go"),
    ("python3", "py"),
    ("py3", "py"),
    ("jsx", "tsx"),
    ("objc", "h"),
    ("objective-c", "h"),
    ("csharp", "cs"),
    ("fsharp", "f#"),
    ("docker", "dockerfile"),
    ("containerfile", "dockerfile"),
    ("jsonc", "json"),
    ("json5", "json"),
    ("html+erb", "erb"),
];

/// The language of a block whose info string starts with `info`, if it names one: GitHub
/// highlights only a block that says what it is, and so does this. A name with no grammar
/// (PowerShell) is shown plain; the highlighter says so.
pub(crate) fn language(info: &str) -> Option<Language> {
    let lower = info.to_lowercase();
    if PLAIN.contains(&lower.as_str()) {
        return None;
    }
    let session = SESSIONS.contains(&lower.as_str());
    let name = if session {
        "bash"
    } else {
        ALIASES
            .iter()
            .find(|(alias, _)| *alias == lower)
            .map_or(lower.as_str(), |(_, name)| name)
    };
    Some(Language {
        name: name.to_owned(),
        session,
    })
}

/// The code blocks that name a language, with their text. Made on the loader thread (a copy
/// of the code's text: the document is not kept).
pub(crate) fn code_sources(doc: &Document) -> HashMap<CodeBlockId, CodeSource> {
    doc.code_blocks_with_ids()
        .filter_map(|(id, block)| {
            let language = language(block.info())?;
            Some((
                id,
                CodeSource {
                    language,
                    text: block.text().to_owned(),
                },
            ))
        })
        .collect()
}

/// The tokens found so far in each block, in order: a block is highlighted from its start.
#[derive(Debug, Clone, Default)]
pub(crate) struct Highlights {
    blocks: HashMap<CodeBlockId, Vec<Token>>,
}

impl Highlights {
    /// Adds tokens found after those already there.
    pub(crate) fn extend(&mut self, block: CodeBlockId, tokens: Vec<Token>) {
        let found = self.blocks.entry(block).or_default();
        let after = found.last().map_or(0, |last| last.end);
        found.extend(tokens.into_iter().filter(|token| token.start >= after));
    }

    /// Colour attributes for `len` bytes of text at `code`, relative to that text: at most
    /// `MAX_PAINT_TOKENS`.
    pub(crate) fn attrs(&self, code: CodeSpan, len: usize) -> Vec<TextAttr> {
        let Some(tokens) = self.blocks.get(&code.block) else {
            return Vec::new();
        };
        let (from, to) = (code.start, code.start + len);
        let first = tokens.partition_point(|token| token.end <= from);
        tokens
            .get(first..)
            .unwrap_or_default()
            .iter()
            .take_while(|token| token.start < to)
            .take(MAX_PAINT_TOKENS)
            .map(|token| TextAttr {
                start: token.start.clamp(from, to) - from,
                end: token.end.clamp(from, to) - from,
                kind: AttrKind::Token(token.class),
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn token(start: usize, end: usize, class: TokenClass) -> Token {
        Token { start, end, class }
    }

    #[test]
    fn languages_are_named_by_the_info_string() {
        let named = |info: &str| language(info).map(|language| (language.name, language.session));
        assert_eq!(named("rust"), Some(("rust".to_owned(), false)));
        assert_eq!(named("Rust"), Some(("rust".to_owned(), false)));
        assert_eq!(named("shell"), Some(("bash".to_owned(), false)));
        assert_eq!(named("console"), Some(("bash".to_owned(), true)));
        assert_eq!(named("jsx"), Some(("tsx".to_owned(), false)));
        assert_eq!(named("nonesuch"), Some(("nonesuch".to_owned(), false)));
        // No info string, plain text and math are not highlighted.
        assert_eq!(named(""), None);
        assert_eq!(named("text"), None);
        assert_eq!(named("math"), None);
        assert_eq!(named("LOG"), None);
    }

    /// The ids of a document's code blocks, in order.
    pub(crate) fn block_ids(doc: &Document) -> Vec<CodeBlockId> {
        doc.code_blocks_with_ids().map(|(id, _block)| id).collect()
    }

    #[test]
    fn only_blocks_with_a_language_are_sources() {
        let doc = gbk_markdown_core::load(b"```rust\nfn x() {}\n```\n\n```\nplain\n```\n");
        let ids = block_ids(&doc);
        let sources = code_sources(&doc);
        assert_eq!((ids.len(), sources.len()), (2, 1));
        let rust = ids.first().and_then(|id| sources.get(id)).expect("rust");
        assert_eq!(rust.language.name, "rust");
        assert_eq!(rust.text, "fn x() {}");
    }

    #[test]
    fn attrs_are_the_tokens_over_a_piece_relative_to_it() {
        let doc = gbk_markdown_core::load(b"```a\nx\n```\n\n```b\ny\n```\n");
        let ids = block_ids(&doc);
        let (block, other) = (*ids.first().expect("a"), *ids.get(1).expect("b"));
        let mut highlights = Highlights::default();
        highlights.extend(
            block,
            vec![
                token(0, 2, TokenClass::Keyword),
                token(3, 8, TokenClass::Function),
                token(10, 12, TokenClass::String),
            ],
        );
        // A later chunk overlapping what is there keeps only its new tokens.
        highlights.extend(
            block,
            vec![
                token(10, 12, TokenClass::String),
                token(20, 22, TokenClass::Comment),
            ],
        );
        let kinds = |start, len| {
            highlights
                .attrs(CodeSpan { block, start }, len)
                .into_iter()
                .map(|attr| (attr.start, attr.end, attr.kind))
                .collect::<Vec<_>>()
        };
        // Bytes 5..11: the end of the function name and the start of the string.
        assert_eq!(
            kinds(5, 6),
            vec![
                (0, 3, AttrKind::Token(TokenClass::Function)),
                (5, 6, AttrKind::Token(TokenClass::String)),
            ]
        );
        // Exactly one token; touching ones are not included.
        assert_eq!(
            kinds(3, 5),
            vec![(0, 5, AttrKind::Token(TokenClass::Function))]
        );
        assert_eq!(kinds(12, 8), Vec::new());
        assert_eq!(
            kinds(12, 9),
            vec![(8, 9, AttrKind::Token(TokenClass::Comment))]
        );
        // Past the cap, the rest of a paint is plain.
        let mut dense = Highlights::default();
        dense.extend(
            block,
            (0..MAX_PAINT_TOKENS + 5)
                .map(|index| token(index * 2, index * 2 + 1, TokenClass::Keyword))
                .collect(),
        );
        let attrs = dense.attrs(CodeSpan { block, start: 0 }, 4 * MAX_PAINT_TOKENS);
        assert_eq!(attrs.len(), MAX_PAINT_TOKENS);
        assert_eq!(
            dense
                .attrs(CodeSpan { block, start: 0 }, 2 * MAX_PAINT_TOKENS - 1)
                .len(),
            MAX_PAINT_TOKENS
        );
        // Another block has none.
        assert_eq!(
            highlights.attrs(
                CodeSpan {
                    block: other,
                    start: 0
                },
                5
            ),
            Vec::new()
        );
    }
}
