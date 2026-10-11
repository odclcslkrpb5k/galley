# gbk-markdown: implementation plan

Status: draft 3, 2026-10-11. Draft 2 was revised after a three-way red-team (performance; GFM and
renderer design; build, rules and process); §13 lists those changes. Draft 3 applies the user's
answers in §11.

A lightweight GUI Markdown viewer for Linux. Target host: Xubuntu 26.04, **X11**, xfwm4 4.20
compositing, Intel UHD 620 (Mesa 26), 1080p at 60 Hz, 8 cores, 659 fonts.

## 1. Goals and budgets

In priority order:

1. **Low time to first rendered paint (TTFP).**
2. **Fast scrolling.**
3. **Good GFM (GitHub Flavored Markdown) support.**

GTK's own startup (loading 112 shared libraries, GL realise, theme CSS, fontconfig, session-bus
and accessibility-bus connections) happens before any of our code draws, and may be 100-250 ms on
this host. So the budget is split: the **floor** (an empty GTK window, measured in M0) and the
**added cost** (our code), which is the number we own.

| Metric | Budget | Measured by |
|---|---|---|
| Floor: empty window, frame submitted | measured in M0, then minimised (§4.4) | §6 |
| Added cost, 20 KB README | ≤ 30 ms over the floor | §6 |
| Added cost, 1 MB document (list- and table-heavy) | ≤ 60 ms over the floor | §6 |
| Work before first frame | only leaves within viewport + one screen are laid out | counter, gated (§6) |
| Scroll | no dropped frames at 60 Hz; 0 Pango layouts created per frame during steady scroll | counters gated; frame gaps reported |
| GFM | text, structure and heading ids match the oracles in §9 | gated tests |

Wall-clock numbers are reports (noisy). Work counters are deterministic and gate the build.

## 2. Stack (decided)

- Rust **1.99.0**, pinned in `rust-toolchain.toml`.
- GTK **4.22** via `gtk4` **0.11.x**, feature `v4_22`. The host and `ubuntu:26.04` both have
  `libgtk-4 4.22.4` and Pango 1.57.
- comrak **=0.56.0**, exact pin, `default-features = false`. Exact because we call
  `comrak::Anchorizer`, which is `#[doc(hidden)]` and so not covered by semver.
- `thiserror` 2.x.
- Later: `syntect` 5.x with `default-fancy` (pure-Rust regex).

## 3. Repository layout

```
Cargo.toml              workspace; [workspace.lints]; every member has [lints] workspace = true
rust-toolchain.toml     channel = "1.99.0", components = clippy, rustfmt
clippy.toml             test-only allowances
deny.toml               cargo-deny (§7)
Dockerfile              build image (§8)
scripts/check           the gate (§8)
scripts/bench-startup   TTFP report, host only (§6)
.githooks/pre-push      refuses a push whose tree has no gate stamp (§8)
crates/core/            gbk-markdown-core: library; no GTK, no I/O
crates/app/             gbk-markdown: binary; GTK adapter layer
crates/lint-canary/     one line per configured lint; the gate checks each one fires (§7)
crates/core/tests/data/ spec.txt + extensions.txt (cmark-gfm, CC-BY-SA 4.0, with LICENSE and
                        attribution), README corpus + GitHub-rendered HTML (§9)
fuzz/                   cargo-fuzz targets (nightly; outside the workspace)
docs/                   plan.md, claude-notes.md, perf-baseline.md
```

### Crate boundaries

- `core` takes bytes, returns a `Document`. No GTK, Pango, GLib or file I/O.
- comrak is confined to **one private module** (`core::parse`). The gate fails if `comrak`
  appears in any other file under `crates/core/src` or anywhere in `crates/app` (Engineer;
  cargo-deny `wrappers = ["gbk-markdown-core"]` additionally refuses a direct comrak dependency
  from `app`). The red-team showed the earlier "Eliminate" label was overstated: `wrappers` only
  checks direct dependents, and `core` could re-export comrak types.
- `anyhow` banned in `deny.toml` (Engineer).

## 4. Architecture

### 4.1 Pipeline

```
main: spawn worker ─► read file ─► core::load(bytes) ─► Document ─┐
      gtk init, window (main thread)                              ├─► first viewport layout ─► paint
                                                                  ┘
idle / worker: measure remaining leaf heights ─► Fenwick tree ─► exact scrollbar
```

`core::load`: decode (UTF-8; otherwise lossy with a `Notice`), parse (comrak with `gfm_options()`),
lower (AST → `Document`; comrak types end here).

### 4.2 Document model: a flat list of leaves

Virtualising per top-level block failed for exactly the documents the 1 MB budget exists for:
an awesome-list is a few huge `List` blocks, a log is one 10 000-line code fence, a generated table
has thousands of rows. Both reviewers flagged this as the blocker. So the model is **flat**:

```rust
struct Document { leaves: Vec<Leaf>, links: Vec<Link>, footnotes: Vec<Footnote>,
                  anchors: Vec<(Anchor, LeafId)>, notices: Vec<Notice> }

struct Leaf { kind: LeafKind, ctx: Context }

// Container context, drawn per leaf: quote bars, alert colour, list indent and marker.
struct Context { quote_depth: u8, alert: Option<AlertKind>, list: SmallVec<[ListLevel; 4]>,
                 first_in_item: Option<ItemMarker>, align: Align, details: Option<DetailsId> }

enum LeafKind {
    Heading { level: u8, inlines: Inlines, anchor: AnchorId },
    Paragraph { inlines: Inlines },
    CodeChunk { block: CodeBlockId, lines: Range<LineNo> },   // ≤ 200 lines per chunk
    TableRows { table: TableId, rows: Range<RowIdx> },        // ≤ 50 rows per chunk
    Math { literal: String, display: bool },
    ThematicBreak,
    DetailsSummary { details: DetailsId, inlines: Inlines },
    FootnoteDefStart { footnote: FootnoteId },
    Notice(NoticeKind),            // e.g. "raw HTML omitted", "invalid UTF-8 replaced"
}

enum ItemMarker { Bullet, Ordered(u64), Task { checked: bool } }   // + nested task items
```

- Tables keep one `Table { aligns, header: Row, rows: Vec<Row> }` referenced by chunks. Column
  widths start from the header and the first 50 rows, then are refined in idle work (§4.3).
- Code blocks keep one `CodeBlock { info, text, line_starts }` referenced by chunks.
- Footnotes are stored once (`Footnote { label, ix, leaves: Range<LeafId> }`); definitions render
  at the end, as on GitHub.

### 4.3 Inlines

```rust
struct Inlines { text: String, spans: Vec<Span>, objects: Vec<(ByteOffset, InlineObject)> }
struct Span { range: Range<ByteOffset>, flags: StyleFlags, link: Option<LinkId> }
enum InlineObject { Image(ImageId), Checkbox(bool) }
```

- Spans are coalesced and non-overlapping; `StyleFlags` is a bitset (emph, strong, strike, code,
  kbd, sub, sup).
- Inline objects are U+FFFC in `text` plus a `pango::AttrShape`, drawn at positions from
  `LayoutIter`. A badge image inside a link is covered by the link span.
- Image alt text is flattened to plain text. Footnote references are their number in the text
  (selectable, findable). Hard break → `\n`; soft break → space.
- Hit-testing: `Layout::xy_to_index`, then a binary search over spans.

### 4.4 Startup path

- A worker thread is started at the top of `main`; it reads and loads the file while the main
  thread runs `gtk::init` and builds the window. Joined before first layout.
- **Renderer.** On X11, GTK 4.22 uses the GL renderer (Vulkan is Wayland-only; `ngl` is now an
  alias). GDK prepares GL on display open regardless of `GSK_RENDERER`, loading libgallium and
  libLLVM. The candidate saving is `GDK_DISABLE=gl,vulkan GSK_RENDERER=cairo`, at the price of
  CPU-repainting the window each scroll frame. M0 measures both TTFP and scroll for GL and that
  configuration.
  - If cairo wins, the env must be set before GTK starts, from inside the binary (a `.desktop`
    `Exec=env` misses terminal launches). `std::env::set_var` is `unsafe` in edition 2024, so
    `unsafe_code` would become `deny` (not `forbid`) with exactly one
    `#[expect(unsafe_code, reason = ...)]` as the first statement of `main`, before any thread
    exists, and a gate check that it is the only `unsafe` in the tree (Engineer). Decided only on
    M0 numbers.
- `gtk::Application` versus plain `gtk::init` + window + main loop: the latter skips the session-bus
  connection (`NON_UNIQUE` still connects synchronously). Measure in M0; take it if it is not
  noise.
- No syntax highlighting, image decoding or font enumeration before first paint.
- Release profile: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `overflow-checks = true`,
  `strip = "symbols"`. A `profiling` profile inherits release with `debug = "line-tables-only"` and
  no strip, for sysprof.

### 4.5 Rendering

**Custom widget** (`DocumentView`: `gtk::Widget` + `gtk::Scrollable` inside a
`gtk::ScrolledWindow`).

A single `GtkTextView` was considered: it gives selection, find and accessibility for free, at the
cost of a whole-document tagged insert before first paint and child widgets for tables and code.
With selection and find out of v1 (§11), that advantage no longer pays for its cost to goals 1
and 2, so it is not spiked. Accessibility for the custom widget is planned through
`subclass::AccessibleTextImpl` (gtk4 0.11, GTK ≥ 4.14) after v1.

Positions are still addressed as (leaf, `ByteOffset`) everywhere, so selection and find can be
added later without changing the model.

- **Fenwick tree over leaf heights** maps scroll offset → leaf in O(log n).
- **First paint** lays out only leaves within the viewport plus one screen. Everything else is
  estimated.
- **Exact heights off the main thread.** Pango layouts are `!Send`, but heights are numbers. A
  worker with its own `pangocairo::FontMap` and a context matching the widget's settings
  (font description, resolution, grey antialias, hint metrics off, slight hinting, unrounded glyph
  positions) measures every leaf and sends heights back (Substitute). An equality test asserts
  worker heights equal main-thread heights for every fixture (Engineer). Fallback if they cannot be
  made equal: main-thread idle slices of ≤ 4 ms.
- **Scroll anchoring.** When a leaf above the viewport changes height, the offset is corrected so
  visible content does not move. GtkScrolledWindow's kinetic deceleration overwrites outside
  adjustment changes, so **height changes above the viewport are queued while scrolling is in
  motion and committed when it stops** (Engineer: a headless test of the commit logic).
- **Caches.** Pango layouts and `gsk::RenderNode`s are kept for about ±3 screens (LRU); heights
  are kept for everything. Key: (width, Pango context serial, style generation), so live Xfce DPI
  or font changes invalidate correctly.
- **No work during scroll.** Idle work pauses while scrolling is in motion; steady-state scroll
  frames append cached nodes only.

### 4.6 Deferred work

- Syntax highlighting (M5): syntect loads on a worker after first paint; visible code chunks first.
- Images (M4): a placeholder (alt text, sized from `width`/`height` attributes when present) is
  painted first. An image is fetched and decoded only once its placeholder has been painted in
  the viewport, on a worker; the leaf is re-measured when it arrives, through the normal anchoring
  path. Local files are read; remote `http(s)` images are fetched with a small blocking HTTP
  client on the worker pool (candidate: `ureq` with rustls; vetted by cargo-deny), with a timeout
  and a size cap. A failed fetch leaves the placeholder with a notice. No fetch ever runs before
  first paint.

## 5. GFM coverage

`core::gfm_options()` is the single place the comrak options are set, with a fixture per option
asserting its effect (Engineer).

- **On:** `extension.strikethrough`, `table`, `autolink`, `tasklist`, `footnotes`, `alerts`,
  `math_dollars`, `math_code`, `front_matter_delimiter = Some("---")`; `shortcodes` (crate feature
  `shortcodes`) from M3.
- **Off**, because GitHub does not render them: `superscript`, `subscript`, `underline`, `spoiler`,
  `highlight`, `insert`, `wikilinks_*`, `description_lists`, `multiline_block_quotes`,
  `greentext`, `subtext`, `inline_footnotes`, `relaxed_autolinks`, `relaxed_tasklist_matching`,
  `tasklist_in_table`, `smart`, `render.hardbreaks`.

| Feature | Plan | M |
|---|---|---|
| Headings, paragraphs, breaks, emphasis, strong, code spans, strikethrough | native | M2 |
| Code blocks | chunked monospace; horizontal scroll | M2 |
| Quotes, lists (nested, tight/loose), thematic break | leaf context | M2 |
| Task lists | `ItemMarker::Task`; comrak's separate `TaskItem` node | M3 |
| Tables, alignment | chunked rows; widths from the first rows, refined | M3 |
| Links, autolinks | clickable; fragment → anchor (`#x` and `#user-content-x`); relative `.md` opens in viewer; others via `gtk::UriLauncher` | M3 |
| Relative paths | `{path, query, fragment}`, percent-decoded; `/x` resolves from the repo root (walk up to `.git`); images resolve from the document's directory | M3 |
| Footnotes | number in text; definitions at end; click to jump | M3 |
| Alerts | coloured bar + title. comrak accepts `> [!NOTE] Custom title`; GitHub renders it as a plain quote (verified in the corpus), so an alert with a title is demoted to a quote. Lower-case `[!note]` is an alert on GitHub | M3 |
| Heading anchors | `comrak::Anchorizer` per document during lowering (`header_ids` no longer exists; ids are not in the AST) | M3 |
| **Raw HTML** | lowered into the model, not shown as source (§5.1) | M3 |
| Front matter | GitHub renders a key/value table (verified in the corpus); v1 renders a code block, recorded as a known deviation | M3 |
| Math | parsed so `$a_b$` is not mangled; shown monospace | M3 |
| Emoji shortcodes | comrak `shortcodes` → Unicode; measure binary size. GitHub renders `:octocat:` as an image; we leave it as text. Unknown shortcodes stay literal | M3 |
| Images | §4.6 | M4 |
| Mermaid | code block | — |
| `@mention`, `#123`, SHA links | need repo context; out of scope | — |

### 5.1 Raw HTML

GitHub sanitises HTML then renders it; READMEs depend on it (`<p align="center"><img>`, badge
rows, `<details>`, `<picture>`, `<!-- comments -->`). Showing source would put tag soup on the
first screen. comrak 0.56 removed `tagfilter`, so filtering is ours. Lowering in `core`:

- `img` → image object; `a` → link; `br` → hard break; `hr` → thematic break.
- `p`, `div`, `h1`-`h6` with `align` → `Context.align`.
- `details`/`summary` → `DetailsSummary` leaf plus `Context.details` on the following leaves.
  comrak emits these as separate `HtmlBlock`s around the Markdown body, so lowering keeps a stack
  across sibling blocks.
- `picture` → its `img` fallback.
- `kbd`, `sub`, `sup`, `b`, `strong`, `i`, `em`, `code`, `del`, `s` → style flags. Inline HTML arrives
  one tag per node; lowering pairs them with a stack.
- Comments, `script`, `style` → dropped. Unknown tags → stripped, text kept.
- An HTML tokeniser is needed. Candidate: `html5gum` or `tl` (to evaluate; size and licence via
  cargo-deny).

## 6. Measurement

### TTFP (report)

- `scripts/bench-startup` passes its spawn time (`CLOCK_REALTIME`, ns) as `--t0=<ns>`. On the
  first frame-clock `after-paint` after the document is drawn, the app prints `now − t0` and calls
  `std::process::exit` immediately, so teardown is not counted. The metric is **frame submitted**:
  xfwm4 sends no frame-timing messages, so GTK cannot know when the frame was shown.
- One-time calibration (Administer, deliberate; there is no Engineer route without compositor
  feedback): a 240 fps phone video of a launch gives submitted-to-visible.
- Runs N=20 per fixture: empty window (the floor), 20 KB, 200 KB, 1 MB list-heavy, 1 MB
  table-heavy, 1 MB code. Reports min/median/p90 and added cost over the floor. A separate
  first-launch-after-login run, since GTK4 and LLVM may not be in the page cache on an Xfce
  (GTK3) desktop.
- `docs/perf-baseline.md` holds the numbers; a median worse by > 20% is flagged in the report.
  Wall-clock regression stays Protect (noise would make a gate fail randomly); the gated
  counters below are the Engineer control.

### Scroll (report)

- `--bench-scroll` drives scenarios: steady scroll, a simulated kinetic fling, End key, scrollbar
  jump, heading-link jump, live resize, and scrolling straight after open while height
  measurement is still running.
- Dropped frames = gaps between successive `frame_time` values over 1.5× the refresh interval
  (presentation times are always 0 on xfwm4).
- sysprof captures with GDK's profiler marks for breakdowns (`profiling` build).

### Work counters (gated)

The app counts, in all builds: leaves laid out before first frame, Pango layouts created per
frame, render nodes built per frame. The gate's Xvfb smoke run (§8) asserts: laid out before
first frame ≤ leaves in viewport + one screen; layouts created during steady scroll = 0.

## 7. Paranoid Rust rules

### Lints

`[workspace.lints]`, every member `[lints] workspace = true`.

- rust: `unknown_lints = { level = "deny", priority = -100 }` (clippy appends `-D warnings` after
  Cargo's lint flags, so `-D warnings` alone does **not** make unknown lints fatal; verified),
  `unsafe_code = "forbid"` (see §4.4 for the one possible exception), `unused_must_use = "deny"`,
  `missing_debug_implementations`, `unreachable_pub`, `rust_2018_idioms = { level, priority = -1 }`.
- clippy: `pedantic = { level = "warn", priority = -1 }`, then the restriction lints:
  `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`,
  `panic_in_result_fn`, `unwrap_in_result`, `indexing_slicing`, `string_slice`, `as_conversions`,
  `wildcard_enum_match_arm`, `let_underscore_must_use`, `map_err_ignore`, `allow_attributes`,
  `allow_attributes_without_reason`, `dbg_macro`, `print_stdout`, `print_stderr`.
- `-D warnings` on clippy makes all warnings fatal.
- **Lint canary** (Engineer): `crates/lint-canary` holds one violating line per configured lint;
  the gate runs clippy on it and fails if any expected lint does not fire. This catches a
  misspelled, renamed or unprioritised lint, and a member crate missing `workspace = true`.
- `indexing_slicing`: iterate instead of index (Eliminate at those sites); `#[expect(..., reason)]`
  for the rest.
- **Numeric conversions** live only in a `units` module behind the newtypes (`LogicalPx`,
  `DevicePx`, `PangoUnits`, `ByteOffset`, `CharOffset`, `LineNo`), with one module-level
  `#![expect(clippy::as_conversions, clippy::cast_possible_truncation, reason = ...)]`. Std has no
  non-`as` f64 → i32 conversion, and GTK/Pango are all f64/i32 (Eliminate everywhere else).
- `clippy.toml`: `allow-unwrap-in-tests`, `allow-expect-in-tests`, `allow-panic-in-tests`,
  `allow-indexing-slicing-in-tests` (they exempt `#[test]` fns and `#[cfg(test)]` modules only).

### Test style

`panic_in_result_fn` fires on a `#[test]` returning `Result` that uses `assert!` (verified; no
exemption). So tests return `()` and use `.expect("what")` and `assert_eq!`. Shared helpers in
`tests/` live inside `#[cfg(test)]` modules or carry `#[expect]` with a reason.

### Errors and panics

- `thiserror` enums in our own words; adapters classify foreign errors by kind (`io::ErrorKind`),
  never by message text.
- One panic hook in `app` logs payload and location to stderr. Panics in GTK callbacks abort; the
  hook is the only record (Protect, deliberate; the lints above are the Engineer controls that aim
  to make panics unreachable).

### cargo-deny (0.20 syntax)

Licence allow-list (MIT, Apache-2.0, BSD-2/3, Zlib, Unicode-3.0, MPL-2.0; a new one fails the gate
until reviewed), advisories with the current syntax (the removed `vulnerability = "deny"` key
errors) and `unmaintained = "workspace"` so an unmaintained transitive GTK crate does not block
every push, `anyhow` banned, comrak wrapped to core, duplicate versions warn.

## 8. Build environment and gate

### Docker

- `FROM ubuntu:26.04@sha256:<pinned>`. apt: `build-essential pkg-config libgtk-4-dev curl
  ca-certificates xvfb`.
- `RUSTUP_HOME=/rustup`, `CARGO_HOME=/cargo`; `rustup-init` downloaded with a pinned sha256;
  toolchain from the copied `rust-toolchain.toml`. `/cargo/{registry,git,advisory-dbs}` created in
  the image with mode 1777 and `/rustup` world-readable, so the build can run as the host UID.
- cargo-deny 0.20.2 (musl tarball), cargo-mutants 27.1.0 and cargo-nextest as **prebuilt
  binaries with pinned sha256s**, not `cargo install` (Substitute: minutes faster per image
  rebuild).
- Run as `--user $(id -u):$(id -g)`; repo at `/work`; named volume on `/cargo` (registry, git,
  advisory database); `CARGO_TARGET_DIR=/work/target/docker` so host tools (rust-analyzer) do not
  contend for the same lock.
- Image tag = hash of `Dockerfile` + `rust-toolchain.toml`; `scripts/check` builds it if missing.

### The gate: `scripts/check`

1. Refuse to run if `git status --porcelain` is non-empty (the gate checks a commit, not a
   working tree).
2. `cargo fmt --all --check`
3. `cargo clippy --workspace --all-targets --locked -- -D warnings`
4. Lint canary (§7) and the boundary grep (§3), plus the `unsafe` count check if §4.4 needs it.
5. `cargo nextest run --workspace --locked` with `INSTA_UPDATE=no`. nextest fails when it finds
   no tests; a committed minimum test count per binary catches a test suite that shrinks
   silently.
6. `cargo deny check`
7. `cargo build --release --locked`
8. Xvfb smoke: `--exit-after-first-frame` on every fixture; asserts exit 0 and the work counters
   (§6); saves a screenshot per fixture to `target/docker/screens/` for before/after comparison
   at each milestone.
9. Assert the tree is still clean (insta and proptest write files into the repo).
10. Write a stamp `target/gate/<tree-hash>` where tree-hash = `git rev-parse HEAD^{tree}`.

### Push

- `.githooks/pre-push` reads the pushed refs from stdin and refuses any sha whose tree has no
  stamp (Engineer). `core.hooksPath` is set by `scripts/check` on every run, so a fresh clone
  gets it after its first gate.
- Claude runs the gate in the background (it can exceed the 10-minute tool timeout on first
  image build), then pushes.
- Remaining bypasses: `--no-verify`, `-c core.hooksPath=...`. Proposed Engineer control: a Claude
  Code `PreToolUse` hook in `.claude/settings.json` that denies `git push` with either. Needs the
  user's approval (§11).

### Not in the gate

`cargo mutants` (slow; see §9), `cargo fuzz` (nightly), `criterion` benches, wall-clock benches.

## 9. Testing

### Fidelity oracles (gated)

"No panic plus a well-formed model" only re-tests comrak's robustness. Lowering errors (dropped
nodes, lost tightness, wrong link targets) need oracles:

1. **Spec HTML.** A test-only `Document → HTML` serialiser; output compared with the expected HTML
   of every example in `spec.txt` (v0.29, 672 examples) and `extensions.txt` (tables,
   strikethrough, autolinks, tag filter, footnotes, task lists), after cmark-style normalisation.
   Known, intended deviations are listed in a checked-in file, each with a reason.
2. **Text content.** The visible text of our model equals the spec HTML's text content (tags
   stripped, entities decoded, whitespace collapsed).
3. **GitHub corpus** (fetched 2026-10-11 by `scripts/fetch-corpus`, in
   `crates/core/tests/data/corpus/`). 21 READMEs under MIT, Apache-2.0, BSD-3, Unlicense or CC0,
   each pinned to a commit with its licence and GitHub's own repo-view HTML; plus two synthetic
   inputs of ours (`_synthetic-features`, `_synthetic-front-matter`) rendered by GitHub's
   repository file view. Compared on text and heading ids. `POST /markdown` is **not** used: it
   skips alerts, math and emoji and renders front matter as a heading. Signed image-URL tokens are
   stripped at fetch.
- A count floor on each oracle: finding fewer examples than expected fails.
- `insta` snapshots for regression on top, not as the oracle.

### Robustness

- `proptest`: arbitrary bytes and Markdown-shaped strings → `core::load` never panics; invariants:
  offsets on char boundaries, spans in bounds and non-overlapping, every id resolves.
- `cargo-fuzz` (dated nightly pinned in a separate image stage): the same targets, plus comrak's
  `arbitrary` feature for structured input. The `fuzz/` crate carries its own copy of the lint
  table, checked by the canary.

### Headless app tests

Fenwick tree, height estimation, anchoring and its queued commit during motion, cache keys, and
worker/main height equality live in display-free modules or run under Xvfb.

### Mutation testing

`cargo mutants` runs on demand and before each milestone closes. For every test written for a
review finding, the real bug is also put back by hand (`cargo-mutants` mutants are generic), and
equivalent mutants are recorded with the guard that makes them equivalent (Administer: the tool
automates what it can; the rest is judgement).

## 10. Milestones

**Definition of done for every milestone:** the gate is green; independent reviews by separate
agents, each briefed on one concern (correctness, performance against §1, coherence with the
model); mutation testing as in §9; screenshots compared with the previous milestone; perf
baseline updated; commit, push, `claude-notes.md` updated. (Administer: no machine check
replaces review.)

- **M0 scaffold and measurement.** Workspace, lints, canary, `deny.toml`, Dockerfile, gate, push
  hook. App spikes: empty window; a minimal custom widget drawing pre-laid-out paragraphs.
  `--t0`, `--exit-after-first-frame`, work counters, Xvfb smoke. Measurements: floor, renderer (GL
  versus cairo with GL disabled), `gtk::Application` versus plain init, LTO on/off, build time.
  **Decides the renderer and the startup path.**
- **M1 core model.** Decode, `gfm_options`, lowering to leaves, inlines, newtypes, HTML lowering
  skeleton; oracles 1 and 2; proptest.
- **M2 renderer v1.** Fenwick tree, first-viewport layout, worker heights, anchoring, caches;
  M2 features; scroll scenarios; counters within budget.
- **M3 GFM completeness.** Tables, tasks, links, footnotes, alerts, anchors, HTML lowering in
  full, front matter, math, emoji; oracle 3.
- **M4 images.** Placeholders first; local and remote fetched lazily once visible (§4.6).
- **M5 syntax highlighting.** Lazy syntect.
- **M6 viewer features.** Reload on
  change (`gio::FileMonitor`); keyboard navigation; zoom; dark mode; `.desktop` file and MIME
  association; install to `~/.local`.
- **After v1:** text selection and copy, find (Ctrl+F), accessibility (`AccessibleTextImpl`).

## 11. Decisions from the user

Answered 2026-10-11:

- Remote images: render a placeholder, fetch asynchronously once the placeholder is visible.
- Text selection, copy and find: not in v1.
- Binary name: `gbk-markdown`.

- GitHub corpus: approved; fetched.

Still open:

1. **Claude Code hook** in `.claude/settings.json` denying `git push --no-verify` and hook-path
   overrides: approve?

## 12. Known risks

- Selection, find and accessibility in the custom widget are real work after v1; the (leaf,
  `ByteOffset`) addressing keeps the model ready for them.
- Remote image fetches contact third-party hosts (badge services, trackers) when a document is
  viewed; this is the chosen behaviour, and nothing is fetched before first paint.
- Worker heights may not match main-thread layout exactly; the equality test says so, and the
  idle-slice fallback exists.
- The renderer floor may dominate TTFP; §4.4's choices are the lever, decided on numbers.
- Theming: GTK4 defaults to Adwaita on Xfce; a small stylesheet aligns it.

## 13. Changes from draft 1 (red-team)

| Finding (reviewer) | Change | Rung |
|---|---|---|
| Per-block virtualisation fails on big lists, tables and code (perf + GFM, both) | Flat leaf model, chunked code and tables | Eliminate |
| HTML shown as source (GFM) | Lower HTML into the model | Substitute |
| Spec test tested no fidelity (GFM) | Three oracles with count floors | Engineer |
| `header_ids` gone; ids not in AST (GFM) | `Anchorizer` in lowering; comrak exact pin | Engineer |
| Inline model underspecified (GFM) | Concrete `Span`, `InlineObject`, U+FFFC + `AttrShape` | Eliminate |
| Block enum gaps: task items, footnotes twice, details, math (GFM) | In the leaf model | Eliminate |
| Selection must shape the model (GFM) | User: not in v1; (leaf, offset) addressing kept | Eliminate |
| comrak options unspecified; math mangled (GFM) | `gfm_options()` with per-option fixtures; math on | Engineer |
| TTFP measured submit + teardown (perf) | `--t0`, `exit` on after-paint, "frame submitted" | Engineer |
| Renderer list wrong; GL loads anyway on X11 (perf) | GL versus cairo with `GDK_DISABLE` | Engineer |
| GTK floor may eat the budget (perf) | Floor + added-cost budgets; sysprof profile | Engineer |
| "Pango !Send ⇒ main thread" did not follow (perf) | Worker heights + equality test | Substitute |
| Kinetic scroll overwrites anchoring (perf) | Queue corrections while in motion | Engineer |
| Scroll bench blind (perf) | Frame-gap detection, scenarios, sysprof | Engineer |
| Cache key misses DPI/font changes; memory (perf) | Key on Pango serial; ±3-screen LRU | Engineer |
| `-D warnings` does not make unknown lints fatal (build) | `unknown_lints` deny at priority -100 | Engineer |
| No group priorities; nothing checks lints apply (build) | Priorities; lint canary crate | Engineer |
| `panic_in_result_fn` fires in tests (build) | Tests return `()` | Eliminate |
| Pre-push hook checked the working tree (build) | Clean-tree gate + tree-hash stamp | Engineer |
| Docker permissions, offline advisory DB, removed deny key (build) | `/cargo` layout, volume, 0.20 syntax | Engineer |
| `cargo install` slow, unpinned (build) | Prebuilt binaries with sha256 | Substitute |
| Zero-test gate passes; no render before/after (build) | nextest + count floor; Xvfb smoke + screenshots | Substitute / Engineer |
| Perf was Protect-only (build) | Gated work counters | Engineer |
| Crate boundary overstated as Eliminate (build) | Private module + gate grep; relabelled | Engineer |
| `as` noise with GTK f64/i32 (build) | `units` module, one module-level expect | Eliminate |
| Gate leaves files behind (build) | `INSTA_UPDATE=no`, clean-tree check after | Engineer |
| Per-step multi-reviewer review dropped (build) | Milestone definition of done | Administer |
