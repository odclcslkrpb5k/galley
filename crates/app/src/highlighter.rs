//! Syntax highlighting on its own thread (docs/plan.md §4.6). The only module that uses syntect
//! (`scripts/check-boundaries`).
//!
//! The thread starts after the first frame, when the first code is drawn: loading the grammars
//! and compiling a language's patterns (20-40 ms) never delays it. A grammar reads a block from
//! its start, line by line, so a chunk deep in a block is highlighted only once everything
//! before it is; the view asks for the code it lays out (the viewport and the screens either
//! side), and the thread works on the request with least left to read first, in chunks of
//! `CHUNK_LINES`, so visible code comes first and no request waits behind a whole long block.
//! Each block is read once, as far as it has been asked for. Tokens come back to the main
//! thread as data (`render::highlight`), which colours them; no layout is made here.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

use gbk_markdown_core::units::CodeBlockId;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

use crate::render::highlight::{CodeSource, Highlights, Token, TokenClass};
use crate::render::paint::LeafPaint;

/// Lines read before the thread looks for newer requests and sends what it found: about 15 ms
/// of work at the slowest measured rate (7 000 lines a second, Rust, M5).
const CHUNK_LINES: usize = 100;
/// A block is highlighted no further than a line longer than this: a grammar's patterns can
/// take very long over one long line (minified code), and nobody reads such a line.
const MAX_LINE_BYTES: usize = 4096;
/// The prompts that start a command in a shell session (` ```console `).
const PROMPTS: [&str; 2] = ["$ ", "% "];

/// Scopes and their colours, most specific first: a token takes the class of the innermost
/// scope it is in that has one. After GitHub's own theme.
const CLASSES: [(&str, TokenClass); 31] = [
    ("comment", TokenClass::Comment),
    ("punctuation.definition.comment", TokenClass::Comment),
    ("string.regexp", TokenClass::Escape),
    ("constant.character.escape", TokenClass::Escape),
    ("string", TokenClass::String),
    ("punctuation.definition.string", TokenClass::String),
    ("constant", TokenClass::Constant),
    ("support.constant", TokenClass::Constant),
    ("variable.language", TokenClass::Constant),
    ("variable.other.constant", TokenClass::Constant),
    ("markup.heading", TokenClass::Constant),
    ("keyword", TokenClass::Keyword),
    ("storage", TokenClass::Keyword),
    ("entity.name.tag", TokenClass::Tag),
    ("entity.name.function", TokenClass::Function),
    ("support.function", TokenClass::Function),
    ("variable.function", TokenClass::Function),
    ("entity.other.attribute-name", TokenClass::Function),
    ("meta.diff.range", TokenClass::Function),
    ("meta.diff.header", TokenClass::Function),
    ("entity.name", TokenClass::Type),
    ("entity.other.inherited-class", TokenClass::Type),
    ("support.type", TokenClass::Type),
    ("support.class", TokenClass::Type),
    ("variable.parameter", TokenClass::Variable),
    ("markup.inserted", TokenClass::Inserted),
    ("punctuation.definition.inserted", TokenClass::Inserted),
    ("markup.deleted", TokenClass::Deleted),
    ("punctuation.definition.deleted", TokenClass::Deleted),
    ("invalid", TokenClass::Deleted),
    ("markup.changed", TokenClass::Type),
];

/// Blocks fully highlighted, shown plain (no grammar, or stopped at a long line), and still
/// being read, for the probe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HighlightCounts {
    pub(crate) highlighted: usize,
    pub(crate) plain: usize,
    pub(crate) pending: usize,
}

/// The main thread's record of one block.
#[derive(Debug)]
struct Block {
    len: usize,
    /// Bytes highlighted, from the start.
    done: usize,
    /// Bytes asked for.
    asked: usize,
    /// Nothing more will come: highlighted to the end, or stopped.
    finished: bool,
    /// Stopped before the end, or never started: no grammar, a long line, or the thread gone.
    plain: bool,
    /// The paints showing it: index, and the bytes of the block each shows.
    paints: Vec<(usize, usize, usize)>,
}

/// The document's highlighting, on the main thread: what each block needs, the tokens found so
/// far, and the thread finding them.
#[derive(Debug)]
pub(crate) struct Highlighting {
    table: Rc<Highlights>,
    blocks: HashMap<CodeBlockId, Block>,
    /// The code, until the thread starts and takes it.
    sources: Option<HashMap<CodeBlockId, CodeSource>>,
    /// Asked for since the last dispatch.
    wanted: Vec<Request>,
    thread: Option<Thread>,
    /// Off while `--check-heights` compares layouts without `--wait-highlights`.
    enabled: bool,
}

#[derive(Debug)]
struct Thread {
    requests: Sender<Request>,
    replies: Receiver<Reply>,
}

/// Highlight block `block` as far as byte `until`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Request {
    block: CodeBlockId,
    until: usize,
}

/// Tokens of `block` found up to byte `until`; `stopped` if nothing more will come before its
/// end.
#[derive(Debug)]
struct Reply {
    block: CodeBlockId,
    until: usize,
    tokens: Vec<Token>,
    stopped: bool,
}

impl Highlighting {
    pub(crate) fn new(
        sources: HashMap<CodeBlockId, CodeSource>,
        paints: &[LeafPaint],
        enabled: bool,
    ) -> Self {
        let mut blocks: HashMap<CodeBlockId, Block> = sources
            .iter()
            .map(|(&id, source)| {
                let block = Block {
                    len: source.text.len(),
                    done: 0,
                    asked: 0,
                    finished: false,
                    plain: false,
                    paints: Vec::new(),
                };
                (id, block)
            })
            .collect();
        for (index, paint) in paints.iter().enumerate() {
            if let Some(code) = paint.code
                && let Some(block) = blocks.get_mut(&code.block)
            {
                block
                    .paints
                    .push((index, code.start, code.start + paint.text.len()));
            }
        }
        Self {
            table: Rc::default(),
            blocks,
            sources: Some(sources),
            wanted: Vec::new(),
            thread: None,
            enabled,
        }
    }

    /// The tokens found so far.
    pub(crate) fn table(&self) -> &Rc<Highlights> {
        &self.table
    }

    /// Asks for a paint's code to be highlighted, if it is not already (sent at the next
    /// `dispatch`). Returns whether anything was asked for.
    pub(crate) fn want(&mut self, paint: &LeafPaint) -> bool {
        let Some(code) = paint.code else {
            return false;
        };
        let Some(block) = self.blocks.get_mut(&code.block) else {
            return false;
        };
        let until = code.start + paint.text.len();
        if !self.enabled || block.finished || until <= block.done || until <= block.asked {
            return false;
        }
        block.asked = until;
        self.wanted.push(Request {
            block: code.block,
            until,
        });
        true
    }

    /// Asks for every block in full (`--wait-highlights`).
    pub(crate) fn want_all(&mut self) {
        if !self.enabled {
            return;
        }
        for (&id, block) in &mut self.blocks {
            if !block.finished && block.asked < block.len {
                block.asked = block.len;
                self.wanted.push(Request {
                    block: id,
                    until: block.len,
                });
            }
        }
    }

    /// Sends what was asked for, starting the thread the first time. Never call this inside a
    /// frame: starting a thread is not free.
    pub(crate) fn dispatch(&mut self) {
        if self.wanted.is_empty() {
            return;
        }
        if self.thread.is_none() {
            let Some(sources) = self.sources.take() else {
                // The thread started once and is gone.
                self.wanted.clear();
                return;
            };
            match start(sources) {
                Ok(thread) => self.thread = Some(thread),
                Err(_no_thread) => {
                    self.gone();
                    return;
                }
            }
        }
        let sent = self.thread.as_ref().is_some_and(|thread| {
            self.wanted
                .drain(..)
                .all(|request| thread.requests.send(request).is_ok())
        });
        if !sent {
            self.gone();
        }
    }

    /// Takes the tokens found since the last call. Returns the paints showing them.
    pub(crate) fn collect(&mut self) -> Vec<usize> {
        let mut affected = Vec::new();
        loop {
            let reply = match self.thread.as_ref().map(|thread| thread.replies.try_recv()) {
                None | Some(Err(TryRecvError::Empty)) => break,
                Some(Err(TryRecvError::Disconnected)) => {
                    self.gone();
                    break;
                }
                Some(Ok(reply)) => reply,
            };
            let Some(block) = self.blocks.get_mut(&reply.block) else {
                continue;
            };
            let from = block.done;
            block.done = block.done.max(reply.until);
            if reply.stopped {
                block.finished = true;
                block.plain = true;
            } else if block.done >= block.len {
                block.finished = true;
            }
            affected.extend(
                block
                    .paints
                    .iter()
                    .filter(|&&(_, start, end)| start < block.done && end > from)
                    .map(|&(paint, _, _)| paint),
            );
            if !reply.tokens.is_empty() {
                Rc::make_mut(&mut self.table).extend(reply.block, reply.tokens);
            }
        }
        affected
    }

    /// Whether anything asked for has not come back yet.
    pub(crate) fn busy(&self) -> bool {
        self.blocks.values().any(Block::pending)
    }

    pub(crate) fn counts(&self) -> HighlightCounts {
        let mut counts = HighlightCounts::default();
        for block in self.blocks.values() {
            if block.pending() {
                counts.pending += 1;
            } else if block.finished && block.plain {
                counts.plain += 1;
            } else if block.finished {
                counts.highlighted += 1;
            }
        }
        counts
    }

    /// The thread could not start, or stopped: what is not highlighted stays plain.
    fn gone(&mut self) {
        self.thread = None;
        self.wanted.clear();
        self.enabled = false;
        for block in self.blocks.values_mut() {
            if !block.finished {
                block.finished = true;
                block.plain = true;
            }
        }
    }
}

impl Block {
    fn pending(&self) -> bool {
        !self.finished && self.asked > self.done
    }
}

fn start(sources: HashMap<CodeBlockId, CodeSource>) -> std::io::Result<Thread> {
    let (requests, requests_in) = mpsc::channel();
    let (replies_out, replies) = mpsc::channel();
    thread::Builder::new()
        .name("highlighter".to_owned())
        .spawn(move || run(&sources, &requests_in, &replies_out))?;
    Ok(Thread { requests, replies })
}

/// The thread: takes requests, reads blocks a chunk at a time, nearest-to-done first.
fn run(
    sources: &HashMap<CodeBlockId, CodeSource>,
    requests: &Receiver<Request>,
    replies: &Sender<Reply>,
) {
    let syntaxes = two_face::syntax::extra_newlines();
    let classes = classes();
    let mut jobs: HashMap<CodeBlockId, Job<'_>> = HashMap::new();
    loop {
        if jobs.values().all(Job::idle) {
            let Ok(request) = requests.recv() else {
                return;
            };
            if !take(&mut jobs, request, sources, &syntaxes, replies) {
                return;
            }
        }
        loop {
            match requests.try_recv() {
                Ok(request) => {
                    if !take(&mut jobs, request, sources, &syntaxes, replies) {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let next = nearest(
            jobs.iter()
                .filter(|(_, job)| !job.idle())
                .map(|(&block, job)| (block, job.until - job.at)),
        );
        let Some((block, job)) = next.and_then(|block| Some((block, jobs.get_mut(&block)?))) else {
            continue;
        };
        let (tokens, stopped) = job.step(&syntaxes, &classes);
        let reply = Reply {
            block,
            until: job.at,
            tokens,
            stopped,
        };
        if replies.send(reply).is_err() {
            return;
        }
    }
}

/// Takes a request: a block's first starts its job (or, with no grammar, is answered plain at
/// once); a later one asks for more of it. Returns false once the main thread is gone.
fn take<'a>(
    jobs: &mut HashMap<CodeBlockId, Job<'a>>,
    request: Request,
    sources: &'a HashMap<CodeBlockId, CodeSource>,
    syntaxes: &'a SyntaxSet,
    replies: &Sender<Reply>,
) -> bool {
    let job = match jobs.entry(request.block) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            let Some(source) = sources.get(&request.block) else {
                return true;
            };
            let Some(job) = Job::new(source, syntaxes) else {
                return replies
                    .send(Reply {
                        block: request.block,
                        until: 0,
                        tokens: Vec::new(),
                        stopped: true,
                    })
                    .is_ok();
            };
            entry.insert(job)
        }
    };
    job.until = job.until.max(request.until.min(job.text.len()));
    true
}

/// The block with least left to read; the first in the document of those with as little.
fn nearest(remaining: impl Iterator<Item = (CodeBlockId, usize)>) -> Option<CodeBlockId> {
    remaining
        .min_by_key(|&(block, left)| (left, block))
        .map(|(block, _)| block)
}

/// The scopes of `CLASSES`, parsed.
fn classes() -> Vec<(Scope, TokenClass)> {
    CLASSES
        .iter()
        .filter_map(|&(name, class)| Some((Scope::new(name).ok()?, class)))
        .collect()
}

/// The class of the innermost scope on `stack` that has one.
fn class_of(stack: &ScopeStack, classes: &[(Scope, TokenClass)]) -> Option<TokenClass> {
    stack.as_slice().iter().rev().find_map(|&scope| {
        classes
            .iter()
            .find(|(prefix, _)| prefix.is_prefix_of(scope))
            .map(|&(_, class)| class)
    })
}

/// A grammar failed on a line (a pattern it cannot run, or an unbalanced scope).
#[derive(Debug)]
struct Unparsed;

/// One block being read.
struct Job<'a> {
    text: &'a str,
    syntax: &'a SyntaxReference,
    session: bool,
    state: ParseState,
    stack: ScopeStack,
    /// The next line starts here.
    at: usize,
    until: usize,
    /// Stopped before the end.
    stopped: bool,
}

impl std::fmt::Debug for Job<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("at", &self.at)
            .field("until", &self.until)
            .finish_non_exhaustive()
    }
}

impl<'a> Job<'a> {
    /// A job for `source`, if there is a grammar for its language.
    fn new(source: &'a CodeSource, syntaxes: &'a SyntaxSet) -> Option<Self> {
        let syntax = syntaxes.find_syntax_by_token(&source.language.name)?;
        Some(Self {
            text: &source.text,
            syntax,
            session: source.language.session,
            state: ParseState::new(syntax),
            stack: ScopeStack::new(),
            at: 0,
            until: 0,
            stopped: false,
        })
    }

    /// Nothing more to do until asked for more.
    fn idle(&self) -> bool {
        self.stopped || self.at >= self.until
    }

    /// Reads up to `CHUNK_LINES` lines towards `until`. Returns the tokens found, and whether
    /// it stopped for good.
    fn step(
        &mut self,
        syntaxes: &SyntaxSet,
        classes: &[(Scope, TokenClass)],
    ) -> (Vec<Token>, bool) {
        let mut tokens = Vec::new();
        let mut lines = 0;
        while self.at < self.until && lines < CHUNK_LINES {
            let rest = self.text.get(self.at..).unwrap_or_default();
            let line = rest.split_inclusive('\n').next().unwrap_or_default();
            if line.is_empty() || line.len() > MAX_LINE_BYTES {
                self.stopped = true;
                break;
            }
            // The grammars match line ends: a block's last line, which has none, is read with
            // one (its tokens end at the text's end).
            let ended: Cow<'_, str> = if line.ends_with('\n') {
                Cow::Borrowed(line)
            } else {
                Cow::Owned(format!("{line}\n"))
            };
            let read = if self.session {
                self.read_session_line(&ended, syntaxes, classes, &mut tokens)
            } else {
                read_line(
                    &mut self.state,
                    &mut self.stack,
                    (&ended, self.at),
                    syntaxes,
                    classes,
                    &mut tokens,
                )
            };
            if read.is_err() {
                self.stopped = true;
                break;
            }
            self.at += line.len();
            lines += 1;
        }
        let end = self.at;
        for token in &mut tokens {
            token.end = token.end.min(end);
        }
        tokens.retain(|token| token.start < token.end);
        (tokens, self.stopped)
    }

    /// A line of a shell session: after a prompt, a command, read on its own; else output,
    /// left plain.
    fn read_session_line(
        &self,
        line: &str,
        syntaxes: &SyntaxSet,
        classes: &[(Scope, TokenClass)],
        tokens: &mut Vec<Token>,
    ) -> Result<(), Unparsed> {
        let Some(command) = PROMPTS.iter().find_map(|prompt| line.strip_prefix(prompt)) else {
            return Ok(());
        };
        let offset = self.at + (line.len() - command.len());
        read_line(
            &mut ParseState::new(self.syntax),
            &mut ScopeStack::new(),
            (command, offset),
            syntaxes,
            classes,
            tokens,
        )
    }
}

/// Reads one line (with its newline) that starts `offset` bytes into its block, and adds its
/// tokens.
fn read_line(
    state: &mut ParseState,
    stack: &mut ScopeStack,
    (line, offset): (&str, usize),
    syntaxes: &SyntaxSet,
    classes: &[(Scope, TokenClass)],
    tokens: &mut Vec<Token>,
) -> Result<(), Unparsed> {
    let ops = state
        .parse_line(line, syntaxes)
        .map_err(|_bad_pattern| Unparsed)?;
    let mut at = 0;
    for (index, op) in ops {
        push(
            tokens,
            offset + at,
            offset + index,
            class_of(stack, classes),
        );
        stack.apply(&op).map_err(|_unbalanced| Unparsed)?;
        at = index;
    }
    push(
        tokens,
        offset + at,
        offset + line.len(),
        class_of(stack, classes),
    );
    Ok(())
}

/// Adds a token, joining it to the last if they touch and match.
fn push(tokens: &mut Vec<Token>, start: usize, end: usize, class: Option<TokenClass>) {
    let Some(class) = class else {
        return;
    };
    if start >= end {
        return;
    }
    if let Some(last) = tokens.last_mut()
        && last.end == start
        && last.class == class
    {
        last.end = end;
        return;
    }
    tokens.push(Token { start, end, class });
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::render::highlight::{Language, code_sources, language};

    fn source(name: &str, text: &str) -> CodeSource {
        CodeSource {
            language: language(name).expect("a language"),
            text: text.to_owned(),
        }
    }

    /// Reads all of `source`, as the thread would.
    fn read_all(source: &CodeSource) -> Option<(Vec<Token>, bool)> {
        let syntaxes = two_face::syntax::extra_newlines();
        let mut job = Job::new(source, &syntaxes)?;
        job.until = source.text.len();
        let (mut tokens, mut stopped) = (Vec::new(), false);
        while !job.idle() {
            let (found, stop) = job.step(&syntaxes, &classes());
            tokens.extend(found);
            stopped = stop;
        }
        Some((tokens, stopped))
    }

    /// The class of the token covering exactly `text`'s first occurrence in `source`.
    fn class_at(source: &CodeSource, tokens: &[Token], text: &str) -> Option<TokenClass> {
        let start = source.text.find(text)?;
        let end = start + text.len();
        tokens
            .iter()
            .find(|token| token.start <= start && token.end >= end)
            .map(|token| token.class)
    }

    #[test]
    fn every_scope_parses() {
        assert_eq!(classes().len(), CLASSES.len());
    }

    #[test]
    fn every_language_alias_has_a_grammar() {
        let syntaxes = two_face::syntax::extra_newlines();
        for info in [
            "rust",
            "bash",
            "sh",
            "shell",
            "console",
            "zsh",
            "fish",
            "toml",
            "yaml",
            "json",
            "jsonc",
            "python",
            "python3",
            "js",
            "ts",
            "jsx",
            "tsx",
            "go",
            "golang",
            "c",
            "cpp",
            "objc",
            "csharp",
            "fsharp",
            "java",
            "kotlin",
            "swift",
            "ruby",
            "php",
            "html",
            "css",
            "diff",
            "dockerfile",
            "docker",
            "makefile",
            "sql",
            "nix",
            "lua",
            "erb",
            "html+erb",
            "batch",
            "markdown",
            "xml",
        ] {
            let name = language(info).map(|language| language.name).expect("named");
            assert!(syntaxes.find_syntax_by_token(&name).is_some(), "{info}");
        }
    }

    #[test]
    fn rust_tokens_take_github_classes() {
        let rust = source(
            "rust",
            "// note\nstruct Point;\nfn main() {\n    let s = \"hi\\n\";\n    let n: u32 = 42;\n}",
        );
        let (tokens, stopped) = read_all(&rust).expect("a grammar");
        assert!(!stopped);
        let class = |text| class_at(&rust, &tokens, text);
        assert_eq!(class("// note"), Some(TokenClass::Comment));
        assert_eq!(class("fn"), Some(TokenClass::Keyword));
        assert_eq!(class("main"), Some(TokenClass::Function));
        assert_eq!(class("\"hi"), Some(TokenClass::String));
        assert_eq!(class("\\n"), Some(TokenClass::Escape));
        assert_eq!(class("42"), Some(TokenClass::Constant));
        assert_eq!(class("Point"), Some(TokenClass::Type));
        // The last line has no newline: tokens end at the text's end.
        assert!(tokens.iter().all(|token| token.end <= rust.text.len()));
        assert_eq!(rust.text.get(rust.text.len() - 1..), Some("}"));
        // Tokens are in order, do not overlap, and are joined where they touch and match.
        assert!(tokens.windows(2).all(|pair| match pair {
            [a, b] => a.end <= b.start && !(a.end == b.start && a.class == b.class),
            _ => false,
        }));
    }

    #[test]
    fn a_last_line_without_a_newline_is_read_whole() {
        // The comment pattern needs a line end; core drops a block's last newline.
        let rust = source("rust", "let a = 1; // the end");
        let (tokens, stopped) = read_all(&rust).expect("a grammar");
        assert!(!stopped);
        assert_eq!(
            class_at(&rust, &tokens, "// the end"),
            Some(TokenClass::Comment)
        );
        assert_eq!(tokens.last().map(|token| token.end), Some(rust.text.len()));
    }

    #[test]
    fn a_session_highlights_commands_after_a_prompt_only() {
        let session = source("console", "$ echo \"hi\"\nhello \"world\"\n% ls # list\n");
        let (tokens, _) = read_all(&session).expect("a grammar");
        assert_eq!(
            class_at(&session, &tokens, "\"hi\""),
            Some(TokenClass::String)
        );
        assert_eq!(
            class_at(&session, &tokens, "# list"),
            Some(TokenClass::Comment)
        );
        // The output line and the prompts themselves are plain.
        let output = session.text.find("hello").expect("output");
        assert!(
            tokens
                .iter()
                .all(|token| token.end <= output || token.start >= output + 14)
        );
        assert!(tokens.iter().all(|token| token.start != 0));
    }

    #[test]
    fn a_long_line_stops_highlighting_there() {
        let long = format!(
            "let a = 1;\n{}\nlet b = 2;\n",
            "x".repeat(MAX_LINE_BYTES + 1)
        );
        let rust = source("rust", &long);
        let (tokens, stopped) = read_all(&rust).expect("a grammar");
        assert!(stopped);
        assert!(tokens.iter().all(|token| token.end <= 11));
        assert_eq!(class_at(&rust, &tokens, "let"), Some(TokenClass::Keyword));
        // A line of exactly the limit is read.
        let edge = format!("let a = \"{}\";\n", "x".repeat(MAX_LINE_BYTES - 12));
        assert_eq!(edge.len(), MAX_LINE_BYTES);
        assert_eq!(
            read_all(&source("rust", &edge)).map(|(_, stopped)| stopped),
            Some(false)
        );
    }

    #[test]
    fn a_language_with_no_grammar_has_no_job() {
        let syntaxes = two_face::syntax::extra_newlines();
        let unknown = CodeSource {
            language: Language {
                name: "nonesuch".to_owned(),
                session: false,
            },
            text: "x".to_owned(),
        };
        assert!(Job::new(&unknown, &syntaxes).is_none());
    }

    #[test]
    fn steps_are_chunks_of_lines_up_to_what_was_asked() {
        let text = "let x = 1;\n".repeat(CHUNK_LINES * 2 + 5);
        let rust = source("rust", &text);
        let syntaxes = two_face::syntax::extra_newlines();
        let mut job = Job::new(&rust, &syntaxes).expect("a grammar");
        job.until = 15 * 11;
        let (first, _) = job.step(&syntaxes, &classes());
        assert_eq!(job.at, 15 * 11);
        assert!(job.idle());
        assert!(first.iter().all(|token| token.end <= 15 * 11));
        job.until = text.len();
        job.step(&syntaxes, &classes());
        assert_eq!(job.at, (15 + CHUNK_LINES) * 11);
        assert!(!job.idle());
    }

    #[test]
    fn the_nearest_request_goes_first() {
        let doc = gbk_markdown_core::load(b"```a\nx\n```\n\n```b\ny\n```\n");
        let ids = crate::render::highlight::tests::block_ids(&doc);
        let (a, b) = (*ids.first().expect("a"), *ids.get(1).expect("b"));
        assert_eq!(nearest([(a, 900), (b, 30)].into_iter()), Some(b));
        assert_eq!(nearest([(b, 30), (a, 30)].into_iter()), Some(a));
        assert_eq!(nearest(std::iter::empty()), None);
    }

    /// Collects until nothing is pending, or fails after 20 s.
    fn settle(highlighting: &mut Highlighting) -> Vec<usize> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut affected = Vec::new();
        while highlighting.busy() {
            assert!(Instant::now() < deadline, "highlighting never finished");
            affected.extend(highlighting.collect());
            thread::sleep(Duration::from_millis(5));
        }
        affected
    }

    #[test]
    fn wanted_code_is_highlighted_and_its_paints_named() {
        let markdown = "```rust\nfn a() {}\n```\n\n```nonesuch\nx\n```\n\n```\nplain\n```\n\n\
                        ```toml\n[x]\ny = 1\n```\n";
        let doc = gbk_markdown_core::load(markdown.as_bytes());
        let paints = crate::render::paint::paint_document(&doc);
        let mut highlighting = Highlighting::new(code_sources(&doc), &paints, true);
        let rust = paints.first().expect("rust");
        assert!(highlighting.want(rust));
        // Asked once.
        assert!(!highlighting.want(rust));
        // Unnamed code is never asked for.
        assert!(!highlighting.want(paints.get(2).expect("plain")));
        assert!(highlighting.want(paints.get(1).expect("nonesuch")));
        highlighting.dispatch();
        let affected = settle(&mut highlighting);
        assert_eq!(affected, vec![0]);
        let code = rust.code.expect("a span");
        let attrs = highlighting.table().attrs(code, rust.text.len());
        assert_ne!(attrs, Vec::new());
        assert_eq!(
            highlighting.counts(),
            HighlightCounts {
                highlighted: 1,
                plain: 1,
                pending: 0
            }
        );
        // Done: not asked again. The rest, all at once.
        assert!(!highlighting.want(rust));
        highlighting.want_all();
        highlighting.dispatch();
        assert_eq!(settle(&mut highlighting), vec![3]);
        assert_eq!(highlighting.counts().highlighted, 2);
    }

    #[test]
    fn disabled_highlighting_asks_for_nothing() {
        let doc = gbk_markdown_core::load(b"```rust\nfn a() {}\n```\n");
        let paints = crate::render::paint::paint_document(&doc);
        let mut highlighting = Highlighting::new(code_sources(&doc), &paints, false);
        assert!(!highlighting.want(paints.first().expect("rust")));
        highlighting.want_all();
        highlighting.dispatch();
        assert!(!highlighting.busy());
        assert!(highlighting.thread.is_none());
    }
}
