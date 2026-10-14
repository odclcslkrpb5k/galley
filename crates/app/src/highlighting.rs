//! Syntax highlighting (docs/plan.md §4.5, "As built in M5"): the main thread's record of what
//! each code block needs and has, and the thread that reads blocks with syntect. The only module
//! that uses syntect (`scripts/check-boundaries`).
//!
//! The thread starts after the first frame, when the first code has been drawn: loading the
//! grammars and compiling a language's patterns (20-40 ms) never delays it. A grammar reads a
//! block from its start, line by line, so a chunk deep in a block is highlighted only once
//! everything before it is. The view asks for the code it lays out (the viewport and the screens
//! either side), and `READ_AHEAD` beyond, so colours are usually there before a paint is laid
//! out. The thread works on the request with least left to read first, a step of at most
//! `CHUNK_LINES` lines or `STEP_TIME` at a time, so no request waits behind a whole long block.
//! Each block is read once, as far as it has been asked for. Tokens come back to the main thread
//! as data (`render::highlight`), which colours them; no layout is made here.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use galley_core::units::CodeBlockId;
use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};

use crate::render::highlight::{CodeSource, Highlights, Token, TokenClass};
use crate::render::paint::LeafPaint;

/// Code asked for beyond the end of a paint being laid out: about 400 lines, several screens.
/// Asking only for what prefetch lays out had every scrolled-to code paint laid out twice,
/// plain and then coloured (M5 review: 1148 layouts in a code-1mb scroll against 573). More is
/// asked for once a paint comes within half of it of the end of what was asked, so steady
/// scrolling finds its colours already there (asking only at the end still recoloured one paint
/// per 16 KiB, and dropped frames in code-1mb that M4 did not).
const READ_AHEAD: usize = 16 * 1024;
/// Lines read, at most, before the thread looks for newer requests and sends what it found...
const CHUNK_LINES: usize = 100;
/// ...or this long, whichever comes first: lines of a few KiB take 20 ms each (M5 review).
const STEP_TIME: Duration = Duration::from_millis(15);
/// A block whose reading has taken longer than `SLOW_BASE`, plus `SLOW_PER_KIB` for each KiB
/// read, stops there: some grammars backtrack for hundreds of milliseconds on lines of ordinary
/// length (Perl alternations, M5 review), and every other block would wait behind them. Not a
/// limit per line: a language's first lines include compiling its patterns (20-40 ms). Ordinary
/// code reads at about 3 ms a KiB (code-1mb).
const SLOW_BASE: Duration = Duration::from_millis(500);
const SLOW_PER_KIB: Duration = Duration::from_millis(10);
/// A block is highlighted no further than a line longer than this (minified code): nobody
/// reads such a line, and a grammar's patterns can take very long over it.
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

/// Why a block is shown plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Plain {
    /// No grammar for its language (PowerShell).
    NoGrammar,
    /// Stopped at a line over `MAX_LINE_BYTES`.
    LongLine,
    /// Stopped where reading it had taken too long (`SLOW_BASE`).
    TooSlow,
    /// The grammar failed on a line (a pattern it cannot run, an unbalanced scope).
    Unparsed,
    /// The thread could not start, or stopped.
    ThreadGone,
}

/// Blocks highlighted to the end, with no grammar, stopped before the end, and being read, for
/// the probe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HighlightCounts {
    pub(crate) highlighted: usize,
    pub(crate) no_grammar: usize,
    pub(crate) stopped: usize,
    pub(crate) pending: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// More may come, as far as it is asked for.
    Reading,
    /// Highlighted to the end.
    Highlighted,
    /// Plain from `done` on, and nothing more will come.
    Plain(Plain),
}

/// The main thread's record of one block.
#[derive(Debug)]
struct Block {
    len: usize,
    state: State,
    /// Bytes highlighted, from the start.
    done: usize,
    /// Bytes asked for, sent or still to send.
    asked: usize,
    /// Bytes asked of the thread.
    sent: usize,
    /// The paints showing it: index, and the bytes of the block each shows.
    paints: Vec<(usize, usize, usize)>,
    /// Its language and text, hashed: a reloaded file's unchanged code keeps its colours.
    key: u64,
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
    /// `SLOW_BASE`, or more for tests that run the thread on a loaded machine in a debug build
    /// (compiling a grammar's patterns there can take 500 ms).
    slow_base: Duration,
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

/// Tokens of `block` found up to byte `until`, and why nothing more will come, if so.
#[derive(Debug)]
struct Reply {
    block: CodeBlockId,
    until: usize,
    tokens: Vec<Token>,
    stopped: Option<Plain>,
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
                let len = source.text.len();
                let block = Block {
                    len,
                    // An empty block has nothing to read.
                    state: if len == 0 {
                        State::Highlighted
                    } else {
                        State::Reading
                    },
                    done: 0,
                    asked: 0,
                    sent: 0,
                    paints: Vec::new(),
                    key: key(source),
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
            slow_base: SLOW_BASE,
        }
    }

    /// Takes the colours of code `old` highlighted to the end, for the same language and text
    /// here: a reloaded file's unchanged code is neither read again nor shown plain meanwhile.
    pub(crate) fn adopt(&mut self, old: &Self) {
        let done: HashMap<u64, CodeBlockId> = old
            .blocks
            .iter()
            .filter(|(_, block)| block.state == State::Highlighted)
            .map(|(&id, block)| (block.key, id))
            .collect();
        for (&id, block) in &mut self.blocks {
            let Some(&from) = done.get(&block.key) else {
                continue;
            };
            // An empty block is highlighted from the start.
            if block.state != State::Reading {
                continue;
            }
            Rc::make_mut(&mut self.table).extend(id, old.table.tokens(from).to_vec());
            block.state = State::Highlighted;
            (block.done, block.asked, block.sent) = (block.len, block.len, block.len);
        }
    }

    /// The tokens found so far. `collect` adds to them through `Rc::make_mut`, which copies the
    /// whole table if anything else holds it: typesetters hold it only while laying out.
    pub(crate) fn table(&self) -> &Rc<Highlights> {
        &self.table
    }

    /// Asks for a paint's code, and `READ_AHEAD` beyond, unless at least half that is already
    /// asked for (sent at the next `dispatch`).
    pub(crate) fn want(&mut self, paint: &LeafPaint) {
        let Some(code) = paint.code else {
            return;
        };
        let Some(block) = self.blocks.get_mut(&code.block) else {
            return;
        };
        let end = code.start + paint.text.len();
        if !self.enabled
            || block.state != State::Reading
            || block.asked >= block.len
            || end + READ_AHEAD / 2 <= block.asked
        {
            return;
        }
        block.asked = (end + READ_AHEAD).min(block.len);
        self.wanted.push(Request {
            block: code.block,
            until: block.asked,
        });
    }

    /// Asks for every block in full (`--wait-highlights`).
    pub(crate) fn want_all(&mut self) {
        if !self.enabled {
            return;
        }
        for (&id, block) in &mut self.blocks {
            if block.state == State::Reading && block.asked < block.len {
                block.asked = block.len;
                self.wanted.push(Request {
                    block: id,
                    until: block.len,
                });
            }
        }
    }

    /// Sends what was asked for, starting the thread the first time. Returns whether anything
    /// was sent. Never call this inside a frame: starting a thread is not free (the probe counts
    /// sends inside frames, and the smoke requires none).
    pub(crate) fn dispatch(&mut self) -> bool {
        if self.wanted.is_empty() {
            return false;
        }
        if self.thread.is_none() {
            let Some(sources) = self.sources.take() else {
                // The thread started once and is gone.
                self.wanted.clear();
                return false;
            };
            match start(sources, self.slow_base) {
                Ok(thread) => self.thread = Some(thread),
                Err(_no_thread) => {
                    self.gone();
                    return false;
                }
            }
        }
        let wanted = std::mem::take(&mut self.wanted);
        let sent = self.thread.as_ref().is_some_and(|thread| {
            wanted
                .iter()
                .all(|&request| thread.requests.send(request).is_ok())
        });
        if !sent {
            self.gone();
            return false;
        }
        for request in wanted {
            if let Some(block) = self.blocks.get_mut(&request.block) {
                block.sent = block.sent.max(request.until);
            }
        }
        true
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
            if let Some(why) = reply.stopped {
                block.state = State::Plain(why);
            } else if block.done >= block.len {
                block.state = State::Highlighted;
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

    /// Whether anything sent to the thread has not come back yet.
    pub(crate) fn busy(&self) -> bool {
        self.blocks.values().any(Block::pending)
    }

    pub(crate) fn counts(&self) -> HighlightCounts {
        let mut counts = HighlightCounts::default();
        for block in self.blocks.values() {
            match block.state {
                State::Reading if block.pending() => counts.pending += 1,
                State::Reading => {}
                State::Highlighted => counts.highlighted += 1,
                State::Plain(Plain::NoGrammar) => counts.no_grammar += 1,
                State::Plain(
                    Plain::LongLine | Plain::TooSlow | Plain::Unparsed | Plain::ThreadGone,
                ) => counts.stopped += 1,
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
            if block.state == State::Reading {
                block.state = State::Plain(Plain::ThreadGone);
            }
        }
    }
}

impl Block {
    fn pending(&self) -> bool {
        self.state == State::Reading && self.sent > self.done
    }
}

/// A block's language and text, hashed.
fn key(source: &CodeSource) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (&source.language.name, source.language.session, &source.text).hash(&mut hasher);
    hasher.finish()
}

fn start(
    sources: HashMap<CodeBlockId, CodeSource>,
    slow_base: Duration,
) -> std::io::Result<Thread> {
    let (requests, requests_in) = mpsc::channel();
    let (replies_out, replies) = mpsc::channel();
    thread::Builder::new()
        .name("highlighter".to_owned())
        .spawn(move || run(&sources, &requests_in, &replies_out, slow_base))?;
    Ok(Thread { requests, replies })
}

/// The thread: takes requests, reads blocks a step at a time, least left to read first.
fn run(
    sources: &HashMap<CodeBlockId, CodeSource>,
    requests: &Receiver<Request>,
    replies: &Sender<Reply>,
    slow_base: Duration,
) {
    // A set that does not load leaves every block plain (the main thread sees the thread gone).
    let Some(syntaxes) = syntaxes() else {
        return;
    };
    let classes = classes();
    let mut jobs: HashMap<CodeBlockId, Job<'_>> = HashMap::new();
    loop {
        if jobs.values().all(Job::idle) {
            let Ok(request) = requests.recv() else {
                return;
            };
            if !take(&mut jobs, request, (sources, &syntaxes), replies, slow_base) {
                return;
            }
        }
        loop {
            match requests.try_recv() {
                Ok(request) => {
                    if !take(&mut jobs, request, (sources, &syntaxes), replies, slow_base) {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        let next = next_job(&jobs);
        let Some((block, job)) = next.and_then(|block| Some((block, jobs.get_mut(&block)?))) else {
            continue;
        };
        let (tokens, stopped) = job.step(&syntaxes, &classes, &Instant::now);
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
    (sources, syntaxes): (&'a HashMap<CodeBlockId, CodeSource>, &'a SyntaxSet),
    replies: &Sender<Reply>,
    slow_base: Duration,
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
                        stopped: Some(Plain::NoGrammar),
                    })
                    .is_ok();
            };
            entry.insert(Job { slow_base, ..job })
        }
    };
    job.until = job.until.max(request.until.min(job.text.len()));
    true
}

/// Whether `took` is too long for reading the first `read` bytes of a block.
/// `base` is `SLOW_BASE` but in tests.
fn too_slow(took: Duration, read: usize, base: Duration) -> bool {
    let kib = u32::try_from(read / 1024).unwrap_or(u32::MAX);
    took > base.saturating_add(SLOW_PER_KIB.saturating_mul(kib))
}

/// The job to step next: of those with work, the one with least left to read.
fn next_job(jobs: &HashMap<CodeBlockId, Job<'_>>) -> Option<CodeBlockId> {
    nearest(
        jobs.iter()
            .filter(|(_, job)| !job.idle())
            .map(|(&block, job)| (block, job.until - job.at)),
    )
}

/// The grammars: two-face's, less those whose licences do not allow shipping them in this
/// binary (`build.rs`, `grammars-excluded.txt`).
fn syntaxes() -> Option<SyntaxSet> {
    syntect::dumps::from_uncompressed_data(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/syntaxes.packdump"
    )))
    .ok()
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

/// A grammar failed on a line (a pattern it cannot run, or an unbalanced scope; fancy-regex
/// also gives up on a pattern past its backtracking limit). The block is plain from there.
#[derive(Debug)]
struct Unparsed;

/// One block being read.
#[derive(Debug)]
struct Job<'a> {
    text: &'a str,
    syntax: &'a SyntaxReference,
    session: bool,
    state: ParseState,
    stack: ScopeStack,
    /// The next line starts here.
    at: usize,
    until: usize,
    /// Why it stopped before the end, if it did.
    stopped: Option<Plain>,
    /// Time spent reading it so far, and the most it may take before `SLOW_PER_KIB`s.
    took: Duration,
    slow_base: Duration,
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
            stopped: None,
            took: Duration::ZERO,
            slow_base: SLOW_BASE,
        })
    }

    /// Nothing more to do until asked for more.
    fn idle(&self) -> bool {
        self.stopped.is_some() || self.at >= self.until
    }

    /// Reads towards `until`: up to `CHUNK_LINES` lines, or until `STEP_TIME` has passed by
    /// `now`. Returns the tokens found, and why it stopped for good, if it did.
    fn step(
        &mut self,
        syntaxes: &SyntaxSet,
        classes: &[(Scope, TokenClass)],
        now: &dyn Fn() -> Instant,
    ) -> (Vec<Token>, Option<Plain>) {
        let mut tokens = Vec::new();
        let mut lines = 0;
        let started = now();
        while self.at < self.until && lines < CHUNK_LINES && self.stopped.is_none() {
            let rest = self.text.get(self.at..).unwrap_or_default();
            let line = rest.split_inclusive('\n').next().unwrap_or_default();
            if line.len() > MAX_LINE_BYTES {
                self.stopped = Some(Plain::LongLine);
                break;
            }
            // The grammars match line ends: a block's last line, which has none, is read with
            // one (its tokens end at the text's end).
            let ended: Cow<'_, str> = if line.ends_with('\n') {
                Cow::Borrowed(line)
            } else {
                Cow::Owned(format!("{line}\n"))
            };
            let line_started = now();
            let read = if self.session {
                self.read_session_line(&ended, syntaxes, classes, &mut tokens)
            } else {
                read_line(
                    &mut self.state,
                    &mut self.stack,
                    (&ended, self.at, self.text.len()),
                    syntaxes,
                    classes,
                    &mut tokens,
                )
            };
            if read.is_err() {
                self.stopped = Some(Plain::Unparsed);
                break;
            }
            self.at += line.len();
            lines += 1;
            let ended_at = now();
            self.took += ended_at.duration_since(line_started);
            // The line is kept; the rest of the block is not read.
            if too_slow(self.took, self.at, self.slow_base) {
                self.stopped = Some(Plain::TooSlow);
            }
            if ended_at.duration_since(started) > STEP_TIME {
                break;
            }
        }
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
            (command, offset, self.text.len()),
            syntaxes,
            classes,
            tokens,
        )
    }
}

/// Reads one line (with its newline) that starts `offset` bytes into its block, and adds its
/// tokens, cut at `end`, the end of the block's text (its last line is read with a newline it
/// does not have).
fn read_line(
    state: &mut ParseState,
    stack: &mut ScopeStack,
    (line, offset, end): (&str, usize, usize),
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
            (offset + at).min(end),
            (offset + index).min(end),
            class_of(stack, classes),
        );
        stack.apply(&op).map_err(|_unbalanced| Unparsed)?;
        at = index;
    }
    push(
        tokens,
        (offset + at).min(end),
        (offset + line.len()).min(end),
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
    use std::cell::Cell;

    use super::*;
    use crate::render::highlight::{ALIASES, Language, SESSIONS, code_sources, language};

    fn source(name: &str, text: &str) -> CodeSource {
        CodeSource {
            language: language(name).expect("a language"),
            text: text.to_owned(),
        }
    }

    /// Reads all of `source`, as the thread would, on `now`'s clock.
    fn read_with(
        source: &CodeSource,
        now: &dyn Fn() -> Instant,
    ) -> Option<(Vec<Token>, Option<Plain>)> {
        let syntaxes = syntaxes()?;
        let mut job = Job::new(source, &syntaxes)?;
        job.until = source.text.len();
        let (mut tokens, mut stopped) = (Vec::new(), None);
        while !job.idle() {
            let (found, stop) = job.step(&syntaxes, &classes(), now);
            tokens.extend(found);
            stopped = stop;
        }
        Some((tokens, stopped))
    }

    /// Reads all of `source` with time standing still: no time limit applies.
    fn read_all(source: &CodeSource) -> Option<(Vec<Token>, Option<Plain>)> {
        let frozen = Instant::now();
        read_with(source, &move || frozen)
    }

    /// A clock that moves on by `tick` each time it is read.
    fn ticking(tick: Duration) -> impl Fn() -> Instant {
        let start = Instant::now();
        let reads = Cell::new(0_u32);
        move || {
            reads.set(reads.get() + 1);
            start + tick * reads.get()
        }
    }

    /// The class of the token covering `text`'s first occurrence in `source`.
    fn class_at(source: &CodeSource, tokens: &[Token], text: &str) -> Option<TokenClass> {
        let start = source.text.find(text)?;
        let end = start + text.len();
        tokens
            .iter()
            .find(|token| token.start <= start && token.end >= end)
            .map(|token| token.class)
    }

    /// The grammars left out, by name and scope (`grammars-excluded.txt`).
    fn excluded() -> Vec<(&'static str, &'static str)> {
        include_str!("../grammars-excluded.txt")
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .filter_map(|line| {
                let mut fields = line.split('\t');
                Some((fields.next()?, fields.next()?))
            })
            .collect()
    }

    #[test]
    fn grammars_whose_licences_forbid_shipping_are_left_out() {
        let ours = syntaxes().expect("the grammars load");
        let theirs = two_face::syntax::extra_newlines();
        let excluded = excluded();
        assert_eq!(excluded.len(), 20);
        for (name, scope) in &excluded {
            // In two-face's set (so the name is right), and not in ours.
            assert!(theirs.find_syntax_by_name(name).is_some(), "{name}");
            assert!(ours.find_syntax_by_name(name).is_none(), "{name}");
            let scope = Scope::new(scope).expect("a scope");
            assert!(ours.find_syntax_by_scope(scope).is_none(), "{name}");
            assert!(theirs.find_syntax_by_scope(scope).is_some(), "{name}");
        }
        assert_eq!(
            ours.syntaxes().len() + excluded.len(),
            theirs.syntaxes().len()
        );
    }

    #[test]
    fn every_grammar_kept_highlights_as_two_faces_does() {
        // References between grammars are renumbered in build.rs: a wrong one shows here as
        // different scopes, a wrong grammar or context.
        let ours = syntaxes().expect("the grammars load");
        let theirs = two_face::syntax::extra_newlines();
        let sample: Vec<&str> = [
            include_str!("../../core/tests/data/corpus/sharkdp__bat/input.md"),
            include_str!("../../core/tests/data/corpus/tauri-apps__tauri/input.md"),
        ]
        .iter()
        .flat_map(|text| text.lines().take(120))
        .collect();
        for syntax in ours.syntaxes() {
            let original = theirs
                .find_syntax_by_name(&syntax.name)
                .expect("in two-face");
            let (mut a, mut b) = (ParseState::new(syntax), ParseState::new(original));
            for line in &sample {
                let line = format!("{line}\n");
                let (ours_ops, theirs_ops) =
                    (a.parse_line(&line, &ours), b.parse_line(&line, &theirs));
                assert_eq!(
                    format!("{ours_ops:?}"),
                    format!("{theirs_ops:?}"),
                    "{}: {line:?}",
                    syntax.name
                );
            }
        }
    }

    /// Every scope a grammar puts `text` in, read with `set`.
    fn scopes_of(set: &SyntaxSet, name: &str, text: &str) -> Vec<String> {
        let syntax = set.find_syntax_by_name(name).expect(name);
        let mut state = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        let mut scopes = Vec::new();
        for line in text.lines() {
            let ops = state.parse_line(&format!("{line}\n"), set).expect(name);
            for (_, op) in ops {
                stack.apply(&op).expect(name);
                scopes.extend(stack.as_slice().iter().map(|scope| scope.build_string()));
            }
        }
        scopes
    }

    #[test]
    fn grammars_that_embedded_one_left_out_use_a_stand_in() {
        let ours = syntaxes().expect("the grammars load");
        let theirs = two_face::syntax::extra_newlines();
        // Less-only text (a variable): only the Less grammar's rules give it `.less` scopes.
        let less = "@c: red;\na { color: @c; }\n";
        let style = format!("<style lang=\"less\">\n{less}</style>\n");
        let slim = "less:\n  @c: red;\n  a { color: @c; }\n";
        // Each text reaches the grammar left out in two-face's set (scopes ending in its
        // suffix); in ours, none do, and the stand-in's rules apply where there is one.
        let cases = [
            ("Vue Component", style.as_str(), ".less", Some(".css")),
            ("Svelte", style.as_str(), ".less", Some(".css")),
            ("Ruby Slim", slim, ".less", Some(".css")),
            (
                "MediaWiki",
                "<syntaxhighlight lang=\"ini\">\n[a]\nb=c\n</syntaxhighlight>\n",
                ".ini",
                None,
            ),
        ];
        for (name, text, left_out, stand_in) in cases {
            let before = scopes_of(&theirs, name, text);
            let after = scopes_of(&ours, name, text);
            let any_ends =
                |scopes: &[String], suffix: &str| scopes.iter().any(|s| s.ends_with(suffix));
            assert!(
                any_ends(&before, left_out),
                "{name}: the text does not reach {left_out}: {before:?}"
            );
            assert!(!any_ends(&after, left_out), "{name}: {after:?}");
            if let Some(stand_in) = stand_in {
                assert!(
                    any_ends(&after, stand_in),
                    "{name}: no {stand_in} in {after:?}"
                );
            }
        }
    }

    #[test]
    fn every_scope_parses() {
        assert_eq!(classes().len(), CLASSES.len());
    }

    #[test]
    fn every_alias_and_common_name_has_a_grammar() {
        let syntaxes = syntaxes().expect("the grammars load");
        let common = [
            "rust",
            "bash",
            "sh",
            "zsh",
            "fish",
            "toml",
            "yaml",
            "json",
            "python",
            "js",
            "ts",
            "tsx",
            "go",
            "c",
            "cpp",
            "java",
            "kotlin",
            "swift",
            "ruby",
            "php",
            "html",
            "css",
            "diff",
            "dockerfile",
            "makefile",
            "sql",
            "nix",
            "lua",
            "markdown",
            "xml",
        ];
        let infos = ALIASES
            .iter()
            .map(|(alias, _)| *alias)
            .chain(SESSIONS)
            .chain(common);
        for info in infos {
            let name = language(info).map(|language| language.name).expect("named");
            assert!(syntaxes.find_syntax_by_token(&name).is_some(), "{info}");
        }
    }

    // These depend on two-face's grammars naming scopes as they do (Cargo.lock pins it). If a
    // version bump breaks one, check the mapping against GitHub again; do not loosen the test.
    #[test]
    fn rust_tokens_take_github_classes() {
        let rust = source(
            "rust",
            "// note\nstruct Point;\nfn main() {\n    let s = \"hi\\n\";\n    let n: u32 = 42;\n}",
        );
        let (tokens, stopped) = read_all(&rust).expect("a grammar");
        assert_eq!(stopped, None);
        let class = |text| class_at(&rust, &tokens, text);
        // Exactly where they are, on later lines too.
        let span = |text: &str| {
            let start = rust.text.find(text).expect("in the text");
            (start, start + text.len())
        };
        let spans = |class| -> Vec<(usize, usize)> {
            tokens
                .iter()
                .filter(|token| token.class == class)
                .map(|token| (token.start, token.end))
                .collect()
        };
        assert!(spans(TokenClass::Function).contains(&span("main")));
        assert!(spans(TokenClass::Constant).contains(&span("42")));
        assert_eq!(class("// note"), Some(TokenClass::Comment));
        assert_eq!(class("fn"), Some(TokenClass::Keyword));
        assert_eq!(class("main"), Some(TokenClass::Function));
        assert_eq!(class("\"hi"), Some(TokenClass::String));
        assert_eq!(class("\\n"), Some(TokenClass::Escape));
        assert_eq!(class("42"), Some(TokenClass::Constant));
        assert_eq!(class("Point"), Some(TokenClass::Type));
        // The last line has no newline: tokens end at the text's end.
        assert!(tokens.iter().all(|token| token.end <= rust.text.len()));
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
        assert_eq!(stopped, None);
        assert_eq!(
            class_at(&rust, &tokens, "// the end"),
            Some(TokenClass::Comment)
        );
        assert_eq!(tokens.last().map(|token| token.end), Some(rust.text.len()));
    }

    #[test]
    fn a_token_runs_on_across_lines() {
        // The string's line ends inside it: the run after the last scope change goes to the
        // end of the line, and joins the next line's.
        let rust = source("rust", "let a = 1;\nlet s = \"multi\nline\";\n");
        let (tokens, _) = read_all(&rust).expect("a grammar");
        let start = rust.text.find("\"multi").expect("string");
        let end = rust.text.find("\";").expect("end") + 1;
        assert!(
            tokens
                .iter()
                .any(|token| (token.start, token.end, token.class)
                    == (start, end, TokenClass::String)),
            "{tokens:?}"
        );
    }

    #[test]
    fn tokens_cut_at_the_text_end_are_never_empty() {
        // Last lines whose final token would run on into the newline the grammar was given.
        for (name, text) in [
            ("rust", "let a = 1; // c"),
            ("rust", "let s = \"open"),
            ("python", "x = 1 # c"),
            ("bash", "echo \"$HOME"),
            ("toml", "a = ["),
            ("diff", "+added"),
            ("yaml", "key: value"),
            ("json", "{\"a\": 1"),
        ] {
            let code = source(name, text);
            let (tokens, _) = read_all(&code).expect("a grammar");
            assert!(
                tokens
                    .iter()
                    .all(|token| token.start < token.end && token.end <= text.len()),
                "{name}: {tokens:?}"
            );
        }
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
        let second = session.text.find('%').expect("second prompt");
        assert!(
            tokens
                .iter()
                .all(|token| token.start != 0 && token.start != second)
        );
    }

    #[test]
    fn a_long_line_stops_highlighting_there() {
        let long = format!(
            "let a = 1;\n{}\nlet b = 2;\n",
            "x".repeat(MAX_LINE_BYTES + 1)
        );
        let rust = source("rust", &long);
        let (tokens, stopped) = read_all(&rust).expect("a grammar");
        assert_eq!(stopped, Some(Plain::LongLine));
        assert!(tokens.iter().all(|token| token.end <= 11));
        assert_eq!(class_at(&rust, &tokens, "let"), Some(TokenClass::Keyword));
        // A line of exactly the limit is read.
        let edge = format!("let a = \"{}\";\n", "x".repeat(MAX_LINE_BYTES - 12));
        assert_eq!(edge.len(), MAX_LINE_BYTES);
        assert_eq!(
            read_all(&source("rust", &edge)).map(|(_, stopped)| stopped),
            Some(None)
        );
    }

    #[test]
    fn a_block_too_slow_to_read_stops_there() {
        let rust = source("rust", "let a = 1;\nlet b = 2;\nlet c = 3;\nlet d = 4;\n");
        // Lines of 250 ms: 500 ms after two is not over the limit, 750 after three is. The
        // third line is kept; the fourth is not read.
        let (tokens, stopped) =
            read_with(&rust, &ticking(Duration::from_millis(250))).expect("a grammar");
        assert_eq!(stopped, Some(Plain::TooSlow));
        assert!(tokens.iter().all(|token| token.end <= 33));
        assert_eq!(tokens.last().map(|token| token.start >= 22), Some(true));
        // Every KiB read allows 10 ms more.
        let base = SLOW_BASE;
        assert!(!too_slow(base + SLOW_PER_KIB * 3, 3 * 1024, base));
        assert!(too_slow(base + SLOW_PER_KIB * 3, 3 * 1024 - 1, base));
        assert!(!too_slow(base, 0, base));
        assert!(too_slow(base + Duration::from_nanos(1), 1023, base));
        assert!(!too_slow(
            Duration::from_secs(10),
            0,
            Duration::from_secs(3600)
        ));
    }

    #[test]
    fn a_step_ends_after_its_time_or_its_lines() {
        let text = "let x = 1;\n".repeat(CHUNK_LINES * 2 + 5);
        let rust = source("rust", &text);
        let syntaxes = syntaxes().expect("the grammars load");
        let mut job = Job::new(&rust, &syntaxes).expect("a grammar");
        let frozen = Instant::now();
        let still = move || frozen;
        // Up to what was asked.
        job.until = 15 * 11;
        let (first, _) = job.step(&syntaxes, &classes(), &still);
        assert_eq!(job.at, 15 * 11);
        assert!(job.idle());
        assert!(first.iter().all(|token| token.end <= 15 * 11));
        // At most CHUNK_LINES lines.
        job.until = text.len();
        job.step(&syntaxes, &classes(), &still);
        assert_eq!(job.at, (15 + CHUNK_LINES) * 11);
        assert!(!job.idle());
        // A clock that moves 2 ms a read, read once at the start and twice a line: the step is
        // 16 ms in after the fourth line, past STEP_TIME, and stops there.
        let at = job.at;
        job.step(&syntaxes, &classes(), &ticking(Duration::from_millis(2)));
        assert_eq!(job.at, at + 4 * 11);
    }

    #[test]
    fn a_step_of_exactly_its_time_goes_on() {
        let text = "let x = 1;\n".repeat(20);
        let rust = source("rust", &text);
        let syntaxes = syntaxes().expect("the grammars load");
        let mut job = Job::new(&rust, &syntaxes).expect("a grammar");
        job.until = text.len();
        // 2.5 ms a read: 15 ms after three lines (not past STEP_TIME), 20 after four.
        job.step(&syntaxes, &classes(), &ticking(Duration::from_micros(2500)));
        assert_eq!(job.at, 4 * 11);
    }

    #[test]
    fn the_job_with_least_left_steps_next() {
        let doc = galley_core::load(b"```rust\na\n```\n\n```rust\nb\n```\n");
        let ids = crate::render::highlight::tests::block_ids(&doc);
        let (a, b) = (*ids.first().expect("a"), *ids.get(1).expect("b"));
        let long = source("rust", &"let x = 1;\n".repeat(1000));
        let syntaxes = syntaxes().expect("the grammars load");
        let job = |at, until| {
            let mut job = Job::new(&long, &syntaxes).expect("a grammar");
            job.at = at;
            job.until = until;
            job
        };
        // A is well on: 1000 left of 10 000 asked. B has 5000 left, though asked for less.
        let mut jobs = HashMap::from([(a, job(9000, 10_000)), (b, job(0, 5000))]);
        assert_eq!(next_job(&jobs), Some(a));
        // A job with nothing to do is never next.
        jobs.insert(a, job(10_000, 10_000));
        assert_eq!(next_job(&jobs), Some(b));
        jobs.insert(b, job(5000, 5000));
        assert_eq!(next_job(&jobs), None);
    }

    #[test]
    fn a_language_with_no_grammar_has_no_job() {
        let syntaxes = syntaxes().expect("the grammars load");
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
    fn the_request_with_least_left_goes_first() {
        let doc = galley_core::load(b"```a\nx\n```\n\n```b\ny\n```\n");
        let ids = crate::render::highlight::tests::block_ids(&doc);
        let (a, b) = (*ids.first().expect("a"), *ids.get(1).expect("b"));
        assert_eq!(nearest([(a, 900), (b, 30)].into_iter()), Some(b));
        assert_eq!(nearest([(b, 30), (a, 30)].into_iter()), Some(a));
        assert_eq!(nearest(std::iter::empty()), None);
    }

    /// A time limit no test's grammar reaches, however loaded the machine: the limit itself is
    /// tested with a fake clock.
    const UNHURRIED: Duration = Duration::from_secs(3600);

    /// Collects until nothing is pending, or fails after 20 s. Returns the paints named, once
    /// each (a block may come back in several steps, as fast as the clock allows).
    fn settle(highlighting: &mut Highlighting) -> Vec<usize> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut affected = Vec::new();
        while highlighting.busy() {
            assert!(Instant::now() < deadline, "highlighting never finished");
            affected.extend(highlighting.collect());
            thread::sleep(Duration::from_millis(5));
        }
        affected.sort_unstable();
        affected.dedup();
        affected
    }

    #[test]
    fn wanted_code_is_highlighted_and_its_paints_named() {
        let markdown = "```rust\nfn a() {}\n```\n\n```nonesuch\nx\n```\n\n```\nplain\n```\n\n\
                        ```toml\n[x]\ny = 1\n```\n\n```rust\n```\n";
        let doc = galley_core::load(markdown.as_bytes());
        let paints = crate::render::paint::paint_document(&doc);
        let mut highlighting = Highlighting::new(code_sources(&doc), &paints, true);
        highlighting.slow_base = UNHURRIED;
        // The empty block is done before anything is asked.
        assert_eq!(highlighting.counts().highlighted, 1);
        let rust = paints.first().expect("rust");
        highlighting.want(rust);
        // Asked once; unnamed code is never asked for.
        highlighting.want(rust);
        highlighting.want(paints.get(2).expect("plain"));
        assert_eq!(highlighting.wanted.len(), 1);
        highlighting.want(paints.get(1).expect("nonesuch"));
        // Nothing is in flight until it is sent.
        assert!(!highlighting.busy());
        assert!(highlighting.dispatch());
        assert!(!highlighting.dispatch());
        let affected = settle(&mut highlighting);
        assert_eq!(affected, vec![0]);
        let code = rust.code.expect("a span");
        let attrs = highlighting.table().attrs(code, rust.text.len());
        assert_ne!(attrs, Vec::new());
        assert_eq!(
            highlighting.counts(),
            HighlightCounts {
                highlighted: 2,
                no_grammar: 1,
                stopped: 0,
                pending: 0
            }
        );
        // Done: not asked again. The rest, all at once: only what is still to read (TOML),
        // and once.
        highlighting.want(rust);
        assert_eq!(highlighting.wanted.len(), 0);
        highlighting.want_all();
        highlighting.want_all();
        let toml = paints.get(3).and_then(|paint| paint.code).expect("toml");
        assert_eq!(
            highlighting
                .wanted
                .iter()
                .map(|request| request.block)
                .collect::<Vec<_>>(),
            vec![toml.block]
        );
        highlighting.dispatch();
        assert_eq!(settle(&mut highlighting), vec![3]);
        assert_eq!(highlighting.counts().highlighted, 3);
    }

    #[test]
    fn a_paint_asks_for_code_ahead_of_it() {
        let code = "let x = 1;\n".repeat(5000);
        let doc = galley_core::load(format!("```rust\n{code}```\n").as_bytes());
        let paints = crate::render::paint::paint_document(&doc);
        let mut highlighting = Highlighting::new(code_sources(&doc), &paints, true);
        assert_eq!(READ_AHEAD, 16 * 1024);
        let first = paints.first().expect("code");
        highlighting.want(first);
        let asked = first.text.len() + READ_AHEAD;
        assert_eq!(
            highlighting.wanted,
            vec![Request {
                block: first.code.expect("a span").block,
                until: asked
            }]
        );
        // A paint with half the read-ahead still asked for beyond it asks for nothing; a paint
        // with less asks for its end and the whole read-ahead again.
        let len = code.trim_end().len();
        for paint in &paints {
            let asked = highlighting
                .wanted
                .last()
                .map_or(0, |request| request.until);
            let count = highlighting.wanted.len();
            highlighting.want(paint);
            let end = paint.code.expect("a span").start + paint.text.len();
            if end + READ_AHEAD / 2 <= asked || asked == len {
                assert_eq!(highlighting.wanted.len(), count);
            } else {
                assert_eq!(
                    highlighting.wanted.last().map(|request| request.until),
                    Some((end + READ_AHEAD).min(len))
                );
            }
        }
        assert!(highlighting.wanted.len() > 1);
        // Never past the block's end.
        assert!(
            highlighting
                .wanted
                .iter()
                .all(|request| request.until <= len)
        );
        assert_eq!(
            highlighting.wanted.last().map(|request| request.until),
            Some(len)
        );
    }

    #[test]
    fn replies_name_the_paints_they_reach() {
        // Three paints of one block; a reply up to the middle of the second.
        let code = "let x = 1;\n".repeat(600);
        let doc = galley_core::load(format!("```rust\n{code}```\n").as_bytes());
        let paints = crate::render::paint::paint_document(&doc);
        assert!(paints.len() >= 3);
        let mut highlighting = Highlighting::new(code_sources(&doc), &paints, true);
        let (requests, _requests_in) = mpsc::channel();
        let (replies_out, replies) = mpsc::channel();
        highlighting.thread = Some(Thread { requests, replies });
        let span = |index: usize| {
            let paint = paints.get(index).expect("a paint");
            let code = paint.code.expect("a span");
            (code.block, code.start, code.start + paint.text.len())
        };
        let (block, _, first_end) = span(0);
        let (_, second_start, _) = span(1);
        let reply = |until| Reply {
            block,
            until,
            tokens: Vec::new(),
            stopped: None,
        };
        replies_out.send(reply(second_start + 5)).expect("sent");
        assert_eq!(highlighting.collect(), vec![0, 1]);
        // From there to the end of the second: only the second.
        let (_, _, second_end) = span(1);
        replies_out.send(reply(second_end)).expect("sent");
        assert_eq!(highlighting.collect(), vec![1]);
        assert!(first_end <= second_start);
        // A reply that ends exactly at a paint's start does not reach it.
        let (_, third_start, _) = span(2);
        assert_eq!(second_end + 1, third_start);
        replies_out.send(reply(third_start)).expect("sent");
        assert_eq!(highlighting.collect(), Vec::<usize>::new());
    }

    #[test]
    fn when_the_thread_is_gone_code_stays_plain() {
        let doc = galley_core::load(b"```rust\nfn a() {}\n```\n\n```toml\nx = 1\n```\n");
        let paints = crate::render::paint::paint_document(&doc);
        let mut highlighting = Highlighting::new(code_sources(&doc), &paints, true);
        let (requests, requests_in) = mpsc::channel();
        let (replies_out, replies) = mpsc::channel::<Reply>();
        highlighting.thread = Some(Thread { requests, replies });
        highlighting.want(paints.first().expect("rust"));
        assert!(highlighting.dispatch());
        assert!(highlighting.busy());
        assert_eq!(highlighting.counts().pending, 1);
        // The thread stops.
        drop((requests_in, replies_out));
        assert_eq!(highlighting.collect(), Vec::<usize>::new());
        assert!(!highlighting.busy());
        assert_eq!(
            highlighting.counts(),
            HighlightCounts {
                highlighted: 0,
                no_grammar: 0,
                stopped: 2,
                pending: 0
            }
        );
        // Nothing more is asked for.
        highlighting.want_all();
        assert!(!highlighting.dispatch());
    }

    #[test]
    fn a_reload_keeps_the_colours_of_unchanged_code() {
        let before = "```rust\nfn a() {}\n```\n\n```toml\nx = 1\n```\n";
        let doc = galley_core::load(before.as_bytes());
        let paints = crate::render::paint::paint_document(&doc);
        let mut old = Highlighting::new(code_sources(&doc), &paints, true);
        old.slow_base = UNHURRIED;
        old.want_all();
        old.dispatch();
        settle(&mut old);
        // Text added before, the Rust block unchanged, the TOML changed.
        let after = "Intro.\n\n```rust\nfn a() {}\n```\n\n```toml\nx = 2\n```\n";
        let doc = galley_core::load(after.as_bytes());
        let paints = crate::render::paint::paint_document(&doc);
        let mut new = Highlighting::new(code_sources(&doc), &paints, true);
        new.adopt(&old);
        assert_eq!(new.counts().highlighted, 1);
        let rust = paints.get(1).and_then(|paint| paint.code).expect("rust");
        let toml = paints.get(2).and_then(|paint| paint.code).expect("toml");
        let old_rust = old
            .blocks
            .iter()
            .find(|(_, block)| block.key == new.blocks.get(&rust.block).expect("block").key)
            .map(|(&id, _)| id)
            .expect("the same block before");
        assert_eq!(new.table().tokens(rust.block), old.table().tokens(old_rust));
        assert_ne!(new.table().tokens(rust.block), Vec::<Token>::new());
        assert_eq!(new.table().tokens(toml.block), Vec::<Token>::new());
        // The adopted block asks for nothing; the changed one does.
        new.want(paints.get(1).expect("rust"));
        assert_eq!(new.wanted, Vec::new());
        new.want(paints.get(2).expect("toml"));
        assert_eq!(new.wanted.len(), 1);
    }

    #[test]
    fn disabled_highlighting_asks_for_nothing() {
        let doc = galley_core::load(b"```rust\nfn a() {}\n```\n");
        let paints = crate::render::paint::paint_document(&doc);
        let mut highlighting = Highlighting::new(code_sources(&doc), &paints, false);
        highlighting.want(paints.first().expect("rust"));
        highlighting.want_all();
        assert!(!highlighting.dispatch());
        assert!(!highlighting.busy());
        assert!(highlighting.thread.is_none());
    }
}
