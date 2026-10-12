# gbk-markdown: implementation plan

Status: draft 3, 2026-10-11; M0-M5 complete (see §10, docs/perf-baseline.md). Draft 2 was revised after a three-way red-team (performance; GFM and
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
and accessibility-bus connections) happens before any of our code draws: measured in M0 at 190-250 ms
(Cairo, GL disabled) and 250-350 ms (GL) on
this host. So the budget is split: the **floor** (an empty GTK window, measured in M0) and the
**added cost** (our code), which is the number we own.

| Metric | Budget | Measured by |
|---|---|---|
| Floor: one line of text in a scrolled window, frame submitted | measured in M0: ~200 ms Cairo, ~300 ms GL; minimised via §4.4 | §6 |
| Added cost, 20 KB README | ≤ 30 ms over the floor (M0 spike: ~10-50 ms over an empty window, renderer-dependent; per-renderer budgets pending the renderer decision, §11) | §6 |
| Added cost, 1 MB document (list- and table-heavy) | ≤ 60 ms over the floor | §6 |
| Work before first frame | ≤ 32 KiB of text given to Pango (an independent bound: two screens of dense text are under 20 KiB) | counter, gated (§6) |
| Scroll | no dropped frames at 60 Hz; 0 Pango layouts created per frame during steady scroll | counter gated from M2; frame gaps reported |
| GFM | text, structure and heading ids match the oracles in §9 | gated tests |

Wall-clock numbers are reports (noisy). Work counters are deterministic and gate the build.

## 2. Stack (decided)

- Rust **1.99.0**, pinned in `rust-toolchain.toml`.
- GTK **4.22** via `gtk4` **0.11.x**, feature `v4_22`. The host and `ubuntu:26.04` both have
  `libgtk-4 4.22.4` and Pango 1.57.
- comrak **=0.56.0**, exact pin, `default-features = false`. Exact because we call
  `comrak::Anchorizer`, which is `#[doc(hidden)]` and so not covered by semver.
- `thiserror` 2.x.
- `ureq` 3.4 (`rustls-no-provider`, `platform-verifier`) and `rustls` 0.23 (`ring`) for remote
  images (M4): a small blocking client on worker threads, certificates checked against the
  system's store (no bundled roots).
- `syntect` 5.3 (`parsing`, `regex-fancy`: fancy-regex, pure Rust, rather than Oniguruma in C)
  with `two-face` 0.5.2, bat 0.26.1's 213 grammars (syntect's own 75 lack TOML, TypeScript,
  Dockerfile, fish, Nix), for syntax highlighting (M5). Parsing only: the colours are ours.

## 3. Repository layout

```
Cargo.toml              workspace; [workspace.lints]; every member has [lints] workspace = true
rust-toolchain.toml     channel = "1.99.0", components = clippy, rustfmt
clippy.toml             test-only allowances
deny.toml               cargo-deny (§7)
Dockerfile              build image (§8)
scripts/check           the gate (§8)
scripts/bench-startup   TTFP report, host only; interleaved configs, CI on differences (§6)
scripts/in-docker       runs a command in the build container (refuses under 5 GiB free)
scripts/quick           the working loop: cargo fmt, then gate-steps --quick (clippy, tests)
scripts/install         installs to ~/.local, default for Markdown (§4.5 M6); test-install
                        checks it in the gate; data/gbk-markdown.desktop is its desktop file
scripts/wait-for        polls a command with a deadline (§8)
.claude/hooks/          Claude Code hooks: guard-push and guard-wait, with table tests (§8)
scripts/gate-steps      the gate's container steps; check-boundaries, check-test-floor, smoke,
                        lint-canary are its helpers; gen-fixtures and fetch-corpus make test data
.githooks/pre-push      refuses a push whose tree has no gate stamp (§8)
crates/core/            gbk-markdown-core: library; no GTK, no I/O
crates/app/             gbk-markdown: binary; GTK adapter layer
crates/lint-canary/     one line per configured lint; the gate checks each one fires (§7)
crates/core/tests/data/ spec.txt + extensions.txt (from M1) (cmark-gfm, CC-BY-SA 4.0, with LICENSE and
                        attribution), README corpus + GitHub-rendered HTML (§9),
                        corpus-deviations.txt (oracle 3's known differences)
fuzz/                   cargo-fuzz targets (nightly; outside the workspace), later
docs/                   plan.md, claude-notes.md, perf-baseline.md
```

### Crate boundaries

- `core` takes bytes, returns a `Document`. No GTK, Pango, GLib or file I/O.
- comrak is confined to **one private module** (`core::parse`). The gate fails if `comrak`
  appears in any other file under `crates/core/src` or anywhere in `crates/app` (Engineer;
  cargo-deny `wrappers = ["gbk-markdown-core"]` additionally refuses a direct comrak dependency
  from `app`). The red-team showed the earlier "Eliminate" label was overstated: `wrappers` only
  checks direct dependents, and `core` could re-export comrak types.
- syntect and two-face are confined to `app/src/highlighting.rs`, the same way (Engineer:
  `scripts/check-boundaries`).
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

### 4.2 Document model: a flat list of leaves in a context tree

As built in M1 (`crates/core/src/model.rs`; read-only outside `core`: private fields with
accessors, `load` the only constructor, so the invariants below cannot be broken by hand).

- **Leaves.** A `Document` is a flat `Vec<Leaf>`. A leaf is one unit of layout:
  `Paragraph(Inlines)`, `Heading { level, inlines, anchor }`, `Code { block, lines }` (≤ 200
  lines and ≤ 16 KiB), `TableRows { table, rows, with_header }` (≤ 50 rows), `ThematicBreak`,
  `AlertTitle(kind)`, `DetailsSummary { details, inlines }`. Paragraphs over 16 KiB
  (`LEAF_TEXT_BYTES`) are split at a space into several leaves, so no leaf can stall layout.
- **Contexts.** Containers form a tree: `Context { container, parent, first_leaf }`, and each
  leaf points at its innermost context (`Leaf::context`). `Container` is `Quote`,
  `Alert(kind)`, `ListItem { marker, tight }`, `Footnote(id)`, `Details { id, open }`. A list
  marker or footnote number is drawn on its context's first leaf (`Context::starts_at`).
  `Document::containers(leaf)` walks innermost first.
- **Side tables.** `links`, `images` (source optional: GitHub's sanitiser drops other
  protocols), `footnotes` (label, number), `tables` (alignments, header (empty for a headerless
  HTML table or front matter), rows of `Inlines`), `code_blocks` (info, text, line starts),
  `anchors` (name and leaf: headings' slugs and raw `<a id>`/`<a name>`, stored without
  `user-content-`, indexed by name), `notices` (invalid UTF-8, HTML nested too deep). Ids are
  index newtypes (`units.rs`). `model::anchor_name` is the one rule turning a `#fragment` into
  an anchor name.
- **Markers.** `Marker::Bullet`, `Ordered { number, delimiter }`, `Task { checked }`.
- **Positions** are (leaf, `ByteOffset`); `Inlines::offset` turns a Pango byte index into one,
  refusing non-boundaries.
- **Exhaustive enums**, deliberately not `#[non_exhaustive]`: across crates that would force
  wildcard arms, which `wildcard_enum_match_arm` refuses, and an exhaustive `match` in the
  renderer is what turns a new variant into a compile error.

### 4.3 Inlines

`Inlines { text, spans, objects }`: one `String`; sorted, non-overlapping `Span { range, style,
link }` only where not plain; `objects` are images standing at U+FFFC. `Style` is a bitset:
emphasis, strong, strikethrough, code, keyboard, sub/superscript, inline and display math,
footnote reference, underline (`<u>`, `<ins>`), highlight (`<mark>`). Soft breaks are spaces,
hard breaks `\n`; a newline from a character reference is a space, as in a browser. Footnote
references are their number in the text. Image alt text is flattened to plain text.

The builder opens Markdown styles and links with `#[must_use]` tokens that close exactly what
they opened, separate from HTML's, so interleaved Markdown and HTML (`*a <b>b* c</b>`) style as
a browser would.

### Changed from the plan in M1, and why

- `Context` per leaf became a context tree with `first_leaf`: a stack copied into every leaf
  made memory quadratic in nesting depth (M1 performance review) and needed a `show_marker`
  flag kept in step by hand.
- `ItemMarker` became `Marker` with a `Delimiter`; `Notice` leaves became document notices
  (the viewer has nothing per-leaf to say yet); `Math { display }` leaves became the `MATH` /
  `DISPLAY_MATH` styles inside a paragraph, which is where comrak puts math.
- `InlineObject::Checkbox` was dropped: the checkbox is the list item's `Marker::Task`.
- Added `AlertTitle`, `TableRows.with_header`, `Footnote.number`, `Details.open`, and the 16 KiB
  leaf cap.
- `anchors` is `Vec<Anchor { slug, leaf }>`; `Document::anchor_leaf` resolves a fragment
  (with or without `user-content-`).

### 4.4 Startup path

- A worker thread is started at the top of `main`; it reads and loads the file while the main
  thread runs `gtk::init` and builds the window. Joined before first layout.
- **Renderer.** On X11, GTK 4.22 uses the GL renderer (Vulkan is Wayland-only; `ngl` is now an
  alias). GDK prepares GL on display open regardless of `GSK_RENDERER`, loading libgallium and
  libLLVM. The candidate saving is `GDK_DISABLE=gl,vulkan GSK_RENDERER=cairo`, at the price of
  CPU-repainting the window each scroll frame. M0 measured both TTFP and scroll for GL and that
  configuration.
  - **Decided 2026-10-12 (§11): Cairo by default, `--renderer=cairo|gl` to choose.** The
    environment is set from inside the binary (a `.desktop` `Exec=env` misses terminal
    launches): `renderer::env_changes` (pure, tested) decides the changes, and `main`'s
    `set_environment`, called before the loader thread or GTK exists, applies them.
    `std::env::set_var` is `unsafe` in edition 2024, so `unsafe_code` is `deny` (not `forbid`)
    with exactly one `#[expect(unsafe_code, reason = ...)]`; `scripts/check-boundaries` fails if
    there is any other `unsafe_code` exception or `unsafe` block outside `main.rs` (Engineer).
    The flag overrides `GSK_RENDERER`; without it a renderer set in `GSK_RENDERER` is kept;
    with neither, Cairo. The probe reports the renderer GTK actually used, and the smoke asserts
    Cairo by default and GL with `--renderer=gl`.
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
- **First paint** lays out only the leaves within the viewport. Everything else is estimated.
- **Exact heights off the main thread.** Pango layouts are `!Send`, but heights are numbers. A
  worker with its own `pangocairo::FontMap` and a context matching the widget's settings
  (font description, resolution, grey antialias, hint metrics off, slight hinting, unrounded glyph
  positions) measures every leaf and sends heights back (Substitute). An equality check
  (`--check-heights`) asserts worker heights equal main-thread heights for every leaf of four
  smoke fixtures (Engineer). Fallback if they cannot be made equal: main-thread idle slices of
  ≤ 4 ms.
- **Scroll anchoring.** When a leaf above the viewport changes height, the offset is corrected so
  visible content does not move. GtkScrolledWindow's kinetic deceleration overwrites outside
  adjustment changes, so **height changes above the viewport are queued while scrolling is in
  motion and committed when it stops** (Engineer: `render::positions` is pure and unit-tested).
- **Caches.** `gsk::RenderNode`s (not layouts) are kept for leaves within ±3 screens; heights
  are kept for everything. The cache is cleared on any change of width, palette (theme) or text
  setup (`gtk::Settings` font, DPI or hinting changes), so live Xfce DPI or font changes
  invalidate correctly.
- **No layout inside scroll frames.** Steady-state scroll frames append cached nodes only; layout
  happens between frames (an idle slice, and a 3 ms slice after each frame) for the 2 screens
  either side of the viewport.

### As built in M2, and why

- **Paint is pure data.** The loader thread turns the model into `LeafPaint`s (text, Pango-free
  attribute spans, font role, indents, gaps, decorations) while GTK starts; the widget and the
  height worker both lay them out through one function (`Typesetter::measure`), and one pure
  function (`Metrics::geometry`) places everything in a leaf and gives its height, so the two
  cannot disagree (Eliminate).
- **No paint is large.** `paint_document` cuts any paint over 2 KiB at a line break, and any
  over 16 KiB anyway (after a space, else at a character boundary), so one layout stays around
  a millisecond whatever the document holds: the M2 review found a 2 MB code line never reached
  a first frame, and 200-line code chunks took 10-48 ms each (Eliminate). Paints therefore do
  not map one to one onto leaves; each records its `source` leaf. A map from paint text offsets
  back to the leaf's `ByteOffset`s was thought needed for M3's link
  hit-testing; M3 resolved links at paint time instead, so it waits for selection and find.
- **Worker heights are equal, not close.** The widget's cairo font options cannot be read back
  (no pangocairo binding), so `TextSetup` derives them from `gtk::Settings` as GTK does.
  `--check-heights` lays out every leaf on the main thread and compares it with the height the
  worker sent for it; the smoke asserts 0 mismatches on four fixtures (Engineer). The idle-slice
  fallback was not needed. If the worker thread dies, the view says so once and keeps estimates.
- **One writer of heights.** `render::positions` holds heights, which are exact and which are
  held while scrolling; every change goes through it and returns the scroll shift. Prefetch,
  drawing, the worker and settling all use it (the first version let prefetch write heights
  directly, bypassing the hold while scrolling). A width change keeps the leaf at the top of the
  viewport and the fraction of it scrolled past. The view's own scroll corrections are not taken
  for user scrolling.
- **Own scroll indicator.** `GtkScrollbar` cost ~16 synchronous X round trips per scroll frame
  (its slider queues an allocation, so the window re-presents itself); with it, 25-131 dropped
  frames per run, without, 0-8. The view draws a thin indicator and handles dragging and
  clicking on it (`render::indicator`, pure and tested) (Substitute).
- **Fonts warm while the loader finishes.** A font's first use costs 3-15 ms; while the main
  thread would otherwise wait for the loader, it lays out a sample in each font a document uses.
- **No layout in scroll frames.** Idle prefetch alone lost to fast scrolling (124-1273 layouts in
  frames). A 3 ms slice after each frame plus a prefetch window of 2 screens either side brought
  every run to 0; the smoke asserts `layouts_in_scroll_frames=0` on three fixtures (Engineer).
- **Body font.** Body text is `sans-serif` and code `monospace`, at the desktop font's size. The
  desktop font here is Source Code Pro, which suits a terminal, not prose.
- **Not yet:** long code lines wrap instead of scrolling sideways. Leaf estimates for the first width are computed on the
  main thread (0.7-2.5 ms on 1 MiB fixtures); they could move to the loader. Rendering each leaf
  to a texture during prefetch, so scroll frames are blits, is the next scroll experiment (GSK
  rendering is now most of a scroll frame's 5-10 ms). A list marker before a heading (`- # x`)
  sits at the text's top in the body font, not on the heading's baseline.

### As built in M3, and why

- **Tables are grids** (`render/table.rs`, pure and tested). One Pango layout per cell; a
  1 px grid (one upright per column boundary per piece), bold header, every second body row
  shaded, per-column alignment. Column widths come from a `ColumnSample` per table (header and
  first rows, ≤ 50 rows, 128 cells, 8 KiB, each cell cut to 256 B): natural widths are measured
  once per text setup and fitted to each width (`table::column_widths`: natural when the table
  fits, else narrow columns keep theirs and wide ones share the rest, none under 3 em unless
  naturally narrower). Every piece of a table shares the sample, so columns line up and the
  worker and widget agree. Pieces are cut between rows at 2 KiB of cell text or 64 cells
  (a 100-column table was one uninterruptible piece of thousands of layouts); cells over 2 KiB
  are cut with "…" (a listed deviation, §5). A table narrower than the text follows the leaf's
  alignment. Estimates fit columns from the sample's longest lines, as measuring does.
- **Links are resolved at paint time** to a `Target`: a leaf (heading, raw anchor, footnote,
  or a footnote's number back to its first reference), a missing fragment, a URL, or a relative
  path. So clicking needs no document, and the paint-to-`ByteOffset` map M2 deferred to M3 was
  not needed; it stays deferred for selection and find. Hit testing lays out the one leaf under
  the pointer (cached until the pointer leaves it; counted; skipped for leaves without links
  and while scrolling). Hover shows a hand and the destination; a click jumps within the
  document (Alt+Left or the back button returns), opens URLs (`gtk::UriLauncher`), opens a
  relative Markdown file in a new viewer process at its fragment (`--anchor`, `--` before the
  path), and other files with `gtk::FileLauncher`: directly for images, audio, video, PDF, plain
  text, Markdown, CSV, HTML and folders, through the application chooser for anything else (a
  default handler could run an `.exe` or `.jar`), never for executables or `.desktop` files
  (`links.rs`, pure and tested). A jump puts its target at the top of the view, so it is the
  anchor and stays there as heights around it become exact. Link failures are reported on
  stderr only (a listed deviation; an in-window notice is M6).
- **`<details>` collapse**, closed unless written `open`, as on GitHub. Paints inside a closed
  one take no height; their laid-out heights are kept, so opening is immediate. A click on a
  summary toggles it (`Positions::estimate` sets the changed heights, inexact, so they are
  measured again when laid out); a link or `--anchor` into closed content opens it first. The
  summary's marker is a `Disclosure` decoration the view turns by its state.
- **Display math** (`$$…$$` alone in a paragraph, ```` ```math ````) is centred, and math blocks
  are not boxed, as GitHub shows them (still monospace source, not typeset).
- **Worker batches** end after 128 leaves or 20 ms, so a resize does not wait behind a batch
  of table pieces; held heights are a map (dropping one when a height is set now is O(1)).

### As built in M4, and why

- **Images stay in the text.** An image is the object character core put in its paint's text,
  marked `AttrKind::Image`; Pango reserves its box with a shape attribute, on the baseline. The
  box's size (`render/image.rs`, pure and tested): the `width`/`height` of an HTML `<img>`
  (clamped to 16384 px), the other side following the image's proportions once known; else
  its own size once decoded; else a placeholder sized from the same attributes or the alt text
  (with room for a "⚠", so a failure moves nothing); then scaled down to the text width. Sizes
  come from an `ImageTable` the height worker and the view share, so both measure alike
  (`--check-heights --wait-images` compares after images load: 0 mismatches).
- **Loading** (`images.rs`). An image is wanted once its placeholder is drawn while the view
  is still (a fling would queue everything it passed), and sent after the frame, never inside
  one, the first frame included. Four threads, started on the first request, read local files
  (relative to the document, `/path` from the repository's root; at most 10 MB, through the
  opened file) or fetch `http(s)` (ureq: 20 s, 5 redirects, 10 MB). Images with the same
  source load once. A load still going after 30 s is shown as failed.
- **Decoding.** The first bytes decide (`render/sniff.rs`, pure and tested): PNG, JPEG, GIF,
  WebP and BMP give their declared size, refused above 40 megapixels before any decoder sees
  them; SVG is drawn at a capped size; anything else is not shown. The decoder is the one
  gdk-pixbuf loader for the sniffed format, at most 2048 px a side, and pixels go to Cairo's
  own premultiplied BGRA on the loading thread. Asking gdk-pixbuf to stop instead (a 0 × 0
  size) was tried and dropped: on Ubuntu 26.04 it hands PNG, JPEG, GIF and SVG to glycin, out
  of process, which waits forever on that, and the formats it keeps in process (WebP, HEIF)
  decode in C.
- **Arrivals** are collected every 100 ms (a row of badges is laid out once, not per badge),
  and the paints showing them, plus every piece of a table whose column sample holds one, are
  laid out again then, between frames, if near the view, through the anchoring path; natural
  column widths are measured again only for those tables. Over 256 MB of decoded images, those
  far from the view are forgotten (their size kept, so nothing moves) and load again when next
  drawn.
- **Placeholders** are faint boxes with the alt text, "⚠" when the load failed; hovering an
  image not shown says why (loading, failed and the reason, offline, nothing to load it from).
  Images are not clickable unless inside a link; links and other attributes skip image
  characters, so a linked badge is not underlined. `--offline` fetches no remote images.
- **Startup**: unchanged within 1 ms (M3 and M4 binaries alternated); the binary grew from 2.7
  to 4.3 MB (TLS).

### As built in M5, and why

- **Only named code is highlighted**, as on GitHub: no guessing. The info string's first word,
  lowercased, names the grammar (`render/highlight.rs`, pure and tested): plain for none,
  `text`, `math`, `log` and the like; GitHub's names the grammars lack are mapped (`shell` is
  Bash, `jsx` TypeScriptReact, `golang` Go: a const table, every entry checked to have a
  grammar); `console` and other shell sessions colour only the commands after a `$ ` or `% `
  prompt. A name with no grammar (PowerShell) is plain.
- **Colour only.** Each token is a foreground colour, GitHub's (light and dark) for its class:
  comment, keyword, string, escape, constant, function, type, tag, variable, inserted,
  deleted; a token takes the class of the innermost scope that has one. No weight or slant,
  so highlighting cannot change a height: the height worker measures code uncoloured, and
  `--check-heights --wait-highlights` checks the coloured layouts agree (0 mismatches on
  code.md and on code-1mb fully highlighted). At most 512 tokens are coloured per paint.
- **A thread of its own** (`highlighting.rs`), started on the first request, which is sent
  after the first frame (never inside a frame: the probe counts sends inside frames and the
  smoke requires none). Grammars load in 1.5 ms there; a language's first line costs 20-40 ms
  (pattern compilation). A grammar reads a block from its start, so a paint asks for its
  block up to its end plus 16 KiB, and again once within 8 KiB of what it asked (read ahead:
  asking only for what prefetch lays out had each scrolled-to code paint laid out twice, and
  asking again only at the end still recoloured one paint per 16 KiB); the thread reads the request with least left first,
  a step at a time (100 lines or 15 ms), so no request waits behind a whole long block. It is
  not nearest-the-viewport first: deep in one long block, its start must be read anyway. A
  block stops being highlighted (plain from there) at a line over 4 KiB, at a line the grammar
  fails on, or once reading it has taken over 500 ms + 10 ms per KiB (a backtracking grammar;
  ordinary code is about 3 ms a KiB).
- **Arrivals** are collected every 30 ms while anything is in flight. Paints showing them that
  are drawn and within prefetch's reach are marked stale in the cache and drawn again by
  prefetch, between frames, nearest first; others are dropped and take their colours when next
  laid out. Unlike an image, a colour moves nothing, so the old plain node can stay on screen
  until then.
- **Cost**: startup unchanged (M4 and M5 alternated); the loader copies the named code (0.5 ms
  for 1 MB). The binary grew from 4.3 to 7.2 MB (the grammars). code-1mb's first screen is
  coloured about 60 ms after the first frame; its end, jumped to directly, after about 3 s
  (6 500 lines a second). Tokens take 24 bytes each, about 2.5 MB for 1 MB of code.

### As built in M6, and why

- **Keys** (`navigation.rs`, pure and tested; listed in `--help`): Up and Down scroll three
  lines, Page Up/Down and Space/Shift+Space a page less two lines, Home/End and Ctrl+Home/End
  the ends, Alt+Left and Backspace go back after a link inside the document, Ctrl+R and F5
  reload, Ctrl+W and Ctrl+Q close; with Super, Hyper or Meta held none is ours. A key scroll
  sets the adjustment as the wheel does, so it is user scrolling to anchoring (held height
  changes wait until it stops), then lays out where it landed at once (up to 8 ms, between
  frames): a held Page Down, 25 pages a second, would outrun prefetch's slices after frames.
  No smooth-scroll animation.
- **Zoom**: Ctrl+plus, Ctrl+minus, Ctrl+0 (Ctrl+Shift+0 where 0 needs Shift) and Ctrl+wheel
  step through browser-like sizes from 50% to 400%; the wheel adds its deltas up, a step per
  notch or per 50 px of touchpad (a step per event went to 400% in one swipe); `--zoom=PERCENT` starts there. Zoom multiplies the text setup's font size, so
  it goes through the path a DPI or font change takes: every size is in em, the height worker
  measures again, and the paint at the top stays there. Images keep their pixel size.
- **Light or dark** (`appearance.rs`, pure and tested): `--theme=system|light|dark`, default
  system. GTK's default theme follows its interface colour scheme (measured: Dark turns the
  text colour from 0.18 to 0.93 grey), which GTK reads from the desktop portal; Xfce's portal
  here states no preference, so with none a GTK theme named `*-dark` means dark (set after GTK
  starts: only then is the theme's name known, and only that case pays GTK's second theme
  load, 15-33 ms). `--theme=light|dark` sets `GTK_THEME` (`Default`, `Default:dark`) before
  GTK starts, in the same one `unsafe` step as the renderer, at no cost. The palette already
  followed the theme's text colour, so the document, code colours included (GitHub's dark
  set), follows with no other change. A document opened from a link gets the same `--theme`,
  `--offline` and zoom.
- **Reload on change** (`document_view/imp/reload.rs`): a `gio::FileMonitor` on the file's
  target (a monitor on a symlink sees nothing when its target is written), with `WATCH_MOVES`
  so a write-and-rename save is seen, started after the first frame. A reload comes 200 ms
  after an event that ends a save, 2 s after a bare write (a slow writer may still be at it),
  and never more than 1 s after the first change, so a file rewritten continuously still
  reloads (a pure, tested rule). It reads and parses on a thread with the startup loader,
  then goes through `set_document` like the first document, starting at the paint (by
  position) that was at the top, at the same zoom; link history is cleared, `<details>`
  reset, and remote images already shown and code already highlighted are kept (by address;
  by language and text): a save fetches no badge again and shows no code plain. An empty file
  where there was text is read once more before it is shown (a writer that truncates first);
  a file gone is noticed, its last version left on show, and loaded again if it comes back
  (an editor that deletes and recreates shows no notice). A change while reading reads once
  more.
- **Notices** (`document_view/imp/notice.rs`): a message at the bottom of the view for 6 s, and
  on stderr, from a typed `Message`: links that fail (a missing anchor, a program refused, a
  launch error), reloads that fail, a file removed, a file that cannot be watched. Drawn by the view
  like its scroll indicator; its layout is made when it is set, never inside a frame.
- **Install** (`scripts/install`): the release binary to `~/.local/bin`, `data/gbk-markdown.desktop`
  (validated in the gate) to `~/.local/share/applications`, and the default for `text/markdown`
  and `text/x-markdown` (unless `--no-default`). The installed desktop file names the binary by
  its full path (a first install makes `~/.local/bin`, on the session's PATH only from the next
  login). The defaults it replaces are recorded and given back by `--uninstall` where ours is
  still the default, which removes only its own entries from `mimeapps.list`.
  `scripts/test-install` runs it against a throwaway home with stub `xdg-mime` (the real one
  ignores entries whose desktop file is missing and falls back to the machine's defaults, so a
  test using it passed or failed by machine).

### 4.6 Deferred work

- Syntax highlighting: built in M5 (§4.5 "As built in M5").
- Images: built in M4 (§4.5 "As built in M4").

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
| Task lists | `Marker::Task`; comrak's separate `TaskItem` node | M3, done |
| Tables, alignment | grids; widths from a sample of the first rows (§4.5 "As built in M3") | M3, done |
| Links, autolinks | clickable; fragment → anchor (`#x` and `#user-content-x`); relative `.md` opens in a new viewer at its fragment; URLs via `gtk::UriLauncher`; only http, https and mailto are links (images: http, https), as GitHub's sanitiser has it | M3, done |
| Relative paths | `{path, query, fragment}`, percent-decoded; `/x` resolves from the repo root (walk up to `.git`); images resolve from the document's directory (M4) | M3, done |
| Footnotes | number in text; definitions at end; click to jump; the definition's number jumps back | M3, done |
| Alerts | coloured bar + title. comrak accepts `> [!NOTE] Custom title`; GitHub renders it as a plain quote (verified in the corpus), so it is a quote starting with the marker line as written. `>[!TIP]` without a space is an alert on GitHub (corpus: bat) though comrak misses it; the walker recognises it. Lower-case `[!note]` is an alert. Alerts nested in lists: comrak makes them alerts; whether GitHub does is unverified | M3 |
| Heading anchors | `comrak::Anchorizer` per document, over GitHub's text content (image alt left out, line breaks dropped, shortcodes by name); checked against every heading id in the corpus. Raw `<a id>`/`<a name>` are link targets too | M1, M3 |
| **Raw HTML** | lowered into the model, not shown as source (§5.1) | M3, done |
| Front matter | a key/value table, as GitHub shows it, for flat YAML (scalars, quoted with escapes, lists shown "a, b"); anything else (nested maps, block strings) stays YAML in a code block | M3, done |
| Math | parsed so `$a_b$` is not mangled; `MATH` / `DISPLAY_MATH` styles, shown monospace; display math centred | M3, done |
| Emoji shortcodes | comrak `shortcodes` → Unicode (the `emojis` crate). GitHub renders `:octocat:` as an image; we leave it as text. Unknown shortcodes stay literal | M3, done |
| Images | inline, sized, scaled to fit; local and remote, loaded once drawn (§4.5 "As built in M4") | M4, done |
| Mermaid | code block | — |

Known deviations from GitHub, as built in M3 (besides oracle 3's list, §9):

- A table wider than the view (many columns at their 3 em minimum) is cut at the right edge:
  there is no sideways scrolling, and cells past the edge are not laid out. GitHub scrolls the
  table sideways.
- Table cells over 2 KiB are cut with "…"; `rowspan` is ignored (`colspan` keeps columns in
  place); `<th>` in body rows is not bold, and a column takes the first `align` given in it.
- A list in front matter shows as "a, b"; GitHub shows a nested table.
- Link failures (missing file, refused program) are reported on stderr, not in the window.
- Table tags inside a Markdown paragraph line (`text</td><td>more`) are not cells.
- Headings inside `<summary>` or a table cell get no anchor (GitHub's behaviour unverified).
- Images (M4): animated GIFs show their first frame; TIFF, HEIF, AVIF, JPEG XL and `data:`
  URIs are not shown; `srcset` is ignored (`<picture>` shows its `<img>`); raster images over
  40 megapixels are refused, not downscaled; an image in the paint at the top of the view that
  arrives can move the text below it in that paint (anchoring is per paint); clicking an image
  that is not in a link does nothing.
| `@mention`, `#123`, SHA links | need repo context; out of scope | — |

### 5.1 Raw HTML

GitHub sanitises HTML, then renders it; READMEs depend on it (`<p align="center"><img>`, badge
rows, `<details>`, `<picture>`). Showing source would put tag soup on the first screen. As
built in M1 (`builder.rs`, tokens from `html5gum` via `html.rs`):

- **One tag stack per document.** Tags opened in an HTML block stay open across Markdown until
  closed (`<div align="center">` ... `</div>`, a link around a block of sponsor images); tags
  opened inside a Markdown paragraph close with it; a closing tag anywhere closes what it
  matches. Each entry caches the cumulative state, so every query is O(1); unmatched closers
  are rejected in O(1); depth is capped at 256 (`Notice::HtmlTooDeep`). Closing a Markdown
  container closes HTML opened inside it; an HTML container closed while Markdown containers
  inside it are open closes as soon as they do.
- `img` → image object (alt, size from `width`/`height`); `a href` → link; `br` → hard break;
  `hr` → thematic break; `picture` → its `img` fallback.
- `p`, `div`, `center`, `h1`-`h6` and other block tags end the text before them; `align` →
  `Leaf::align`. `h1`-`h6` → heading leaves with GitHub slugs (from the text as written).
- `details` → `Container::Details { open }`; `summary` → a `DetailsSummary` leaf collecting
  everything until `</summary>` (HTML text, Markdown paragraphs, headings).
- `blockquote` → `Container::Quote`; `pre` → a code block, text verbatim.
- `b`, `strong`, `i`, `em`, `code`, `kbd`, `sub`, `sup`, `del`, `s`, `u`, `ins`, `mark` → styles.
- **Tag filter**, as GitHub: `script`, `style`, `title`, `textarea`, `xmp`, `iframe`, `noembed`,
  `noframes`, `plaintext` are shown as literal text (checked against github.com's rendering).
- `video`/`audio` → a "▶ video" link to the media; `template` → hidden; comments dropped;
  unknown tags stripped, text kept.
- **Raw HTML tables are tables** (M3): rows, a first row of only `<th>` as the header,
  per-column `align` (the first given), `colspan` as empty cells after the spanning one; a cell's
  paragraphs, `<br>`s, block tags, `<details>` and `<summary>` are lines of it; a table inside a
  cell reads as text in it; text outside any cell shows before the table; anything a cell
  cannot hold (a code block, a list) ends the table there.
- A `<details>` with no `<summary>` gets "Details", as a browser shows it. One trailing `<br>`
  in a block shows nothing and the rest are empty lines, as a browser lays them out. Inline
  `<video>`/`<audio>` become links, like block ones. Links and images keep only the protocols
  GitHub's sanitiser allows (§5).

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

The app counts, in all builds: layouts and bytes of text given to Pango before the first frame,
width resets before the first frame, layouts created during scroll. Every layout goes through
one counted constructor (`disallowed-methods` refuses any other path), so no layout work is
uncounted. The gate's Xvfb smoke run (§8) asserts bytes before the first frame ≤ 32 KiB, a bound
independent of the view's own layout window (the M0 review found the first version, "laid out ≤
the view's own window", could not fail). From M2: a --bench-scroll smoke run asserts layouts
inside scroll frames = 0. A leaf's render node is built in the same step as its layout, so that
counter covers node building too. From M3 the smoke also budgets layouts before the first
frame (≤ 1000: bytes miss a table's many small cells), scrolls table-1mb and a 100-column
table, and checks links and clicks through the probe: `--link-at=X,Y` reports the link at a
point after the first frame (`link_at=leaf:N|uri:…|relative:…|missing:…|none`), and
`--wait-images` reports `images_loaded`, `images_failed` and `images_loading` once nothing is
loading (M4; smoke checks images.md loads 5 and fails 2, and compares heights after images
load), `--press=KEY,...` presses keys through the view after the first frame and reports the next
(`pressed=`, `document_height=`, `top_leaf=`; M6: smoke pages down, goes to the end and back
to the top, and zooms), `--rewrite-with=PATH` writes PATH over the file after the first frame and
reports once it has reloaded (`rewritten=`, `reloads=`), every first-frame report says
`theme=light|dark` from the theme's text colour (smoke: dark with `--theme=dark`), and
`notices=` counts notices (smoke: clicking a link to a missing anchor shows one).
`--wait-highlights` highlights all code and reports `code_highlighted`,
`code_no_grammar`, `code_stopped`, `code_pending` and `highlights_sent_in_frames` once done (M5;
smoke checks code.md's 6, 1, 1 and 0 sends, and compares heights with code coloured, requiring
the check's `code_highlighted` above 0; scroll reports add `recoloured` since scrolling began,
which must be above 0 on code.md, and `highlights_sent_in_frames`, which must be 0), and `--click-at=X,Y` clicks there through the widget's own handling (never opening anything outside
the document) and reports the next frame (`clicked=`, `document_height=`, `top_leaf=`). Scroll scenarios other than steady scroll-to-end (flinging up
into estimated leaves, resizing mid-document) are covered by `render::positions`' unit tests,
not by bench runs.

## 7. Paranoid Rust rules

### Lints

`[workspace.lints]`, every member `[lints] workspace = true`.

- rust: `unknown_lints = { level = "deny", priority = -100 }` (clippy appends `-D warnings` after
  Cargo's lint flags, so `-D warnings` alone does **not** make unknown lints fatal; verified),
  `unsafe_code = "deny"` (one exception, §4.4, enforced by `scripts/check-boundaries`), `unused_must_use = "deny"`,
  `missing_debug_implementations`, `unreachable_pub`, `rust_2018_idioms = { level, priority = -1 }`.
- clippy: `pedantic = { level = "deny", priority = -1 }`, then the restriction lints:
  `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`,
  `panic_in_result_fn`, `unwrap_in_result`, `indexing_slicing`, `string_slice`, `as_conversions`,
  `wildcard_enum_match_arm`, `let_underscore_must_use`, `map_err_ignore`, `allow_attributes`,
  `allow_attributes_without_reason`, `dbg_macro`, `print_stdout`, `print_stderr`,
  `disallowed_methods` (with `clippy.toml`: stdout/stderr only through `app/src/output.rs`, Pango
  layouts only through one counted constructor).
- `-D warnings` on clippy makes all warnings fatal.
- **Lint canary** (Engineer): `crates/lint-canary` holds one violating line per configured lint;
  the gate runs clippy on it and fails if any expected lint does not fire. This catches a
  misspelled, renamed or unprioritised lint, and a member crate missing `workspace = true`.
- `indexing_slicing`: iterate instead of index (Eliminate at those sites); `#[expect(..., reason)]`
  for the rest.
- **Numeric conversions** live only in a `units` module per crate, with one module-level
  `#![expect(clippy::as_conversions, ..., reason = ...)]`. Std has no non-`as` float/integer
  conversion, and GTK/Pango are all f64/f32/i32 (Eliminate everywhere else). Core uses index
  newtypes (`ByteOffset`, `LineNo`, `LeafId`, ...). *As built (M2):* the app has no `LogicalPx`
  newtype: positions and lengths are whole logical pixels in `i64` and em lengths are `f64`
  fields named `*_em`, so the two cannot be mixed without a conversion through `Metrics::px`
  or `units` (the types already differ). A `Px(i64)` newtype was considered (Engineer) and
  not taken: it would add wrapping at every GTK boundary for no bug class the types do not
  already separate. Revisit if a px/em or px/Pango-unit mix-up ever ships.
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

Licence allow-list (MIT, Apache-2.0, BSD-2/3, Zlib, Unicode-3.0, MPL-2.0, and ISC since M4 for
ring, rustls-webpki and untrusted (reviewed: permissive, MIT-equivalent); a new one fails the
gate until reviewed; the graph is limited to `x86_64-unknown-linux-gnu`, the only target built; Unicode-DFS-2016 is allowed for `finl_unicode` alone, as a per-crate exception), advisories with the current syntax (the removed `vulnerability = "deny"` key
errors) and `unmaintained = "workspace"` so an unmaintained transitive GTK crate does not block
every push, `anyhow` banned, comrak wrapped to core, duplicate versions warn. two-face is
MIT/Apache-2.0, but it bundles bat's grammars, each under its own licence (bat lists them in
its acknowledgements); cargo-deny cannot see those, and they were not reviewed one by one in
M5. The viewer is not distributed; review them before it is.

## 8. Build environment and gate

### Docker

- `FROM ubuntu:26.04@sha256:<pinned>`. apt: `build-essential pkg-config libgtk-4-dev curl
  ca-certificates xvfb`, `librsvg2-common` (gdk-pixbuf's SVG loader, for image tests and
  smoke) and `desktop-file-utils` (M6: the desktop file is validated). Smoke and bench-startup
  run `--offline`: the gate contacts no network.
- `RUSTUP_HOME=/rustup`, `CARGO_HOME=/cargo`; `rustup-init` downloaded with a pinned sha256;
  toolchain from the copied `rust-toolchain.toml`. `/cargo/{registry,git,advisory-dbs}` created in
  the image with mode 1777 and `/rustup` world-readable, so the build can run as the host UID.
- cargo-deny 0.20.2 (musl tarball), cargo-mutants 27.1.0 and cargo-nextest as **prebuilt
  binaries with pinned sha256s**, not `cargo install` (Substitute: minutes faster per image
  rebuild).
- Run as `--user $(id -u):$(id -g)`; repo at `/work`; named volumes on `/cargo/registry`, `/cargo/git` and `/cargo/advisory-dbs` (registry, git,
  advisory database); `CARGO_TARGET_DIR=/work/target/docker` so host tools (rust-analyzer) do not
  contend for the same lock.
- Image tag = hash of `Dockerfile` + `rust-toolchain.toml`; `scripts/in-docker` builds it if
  missing, and then removes this project's images it replaces (each was 2.4 GB).
- `scripts/in-docker` refuses to start with under 5 GiB free on the repo's disk (Engineer, M6:
  scratch builds, superseded images and mutation runs' stale test binaries filled the disk,
  which every program on the machine shares).
- `docker run --init`: otherwise the first command is PID 1, and `xvfb-run` as PID 1 waits
  forever for its X server (it hung three times in M3).

### The gate: `scripts/check`

1. Refuse to run if `git status --porcelain` is non-empty (the gate checks a commit, not a
   working tree).
2. `cargo fmt --all --check`
3. `cargo clippy --workspace --exclude gbk-markdown-lint-canary --all-targets --locked -- -D warnings`
4. Lint canary (§7), the boundary grep (§3), `.claude/hooks/test-guard-push` and
   `test-guard-wait`, plus the `unsafe` count check if §4.4 needs it.
5. `cargo nextest run --locked` over the default members (the canary is excluded via
   `default-members`). nextest fails when it finds
   no tests; a committed minimum test count per binary catches a test suite that shrinks
   silently.
6. `cargo deny check`
7. `cargo build --release --locked`, then `scripts/test-install` (M6: install into a throwaway
   home and uninstall again).
8. Xvfb smoke: `--exit-after-first-frame` on every fixture; asserts exit 0 and the work counters
   (§6); saves a screenshot per fixture to `target/docker/screens/` for before/after comparison
   at each milestone.
9. Assert the tree is still clean (proptest writes regression files into the repo on failure).
10. Write a stamp `target/gate/<tree-hash>` where tree-hash = `git rev-parse HEAD^{tree}`.

### Push

- `.githooks/pre-push` reads the pushed refs from stdin and refuses any pushed tip whose tree has no
  stamp (Engineer). `core.hooksPath` is set by `scripts/check` on every run, so a fresh clone
  gets it after its first gate.
- Claude runs the gate in the background (it can exceed the 10-minute tool timeout on first
  image build), then pushes.
- Bypasses (`--no-verify`, any change to `core.hooksPath`, including `-c` and `GIT_CONFIG_*`) are
  refused for Claude by a Claude Code `PreToolUse` hook, `.claude/hooks/guard-push`, registered in
  `.claude/settings.json` (Engineer; approved 2026-10-11). It fails closed (`onFailure: block`).
  `.claude/hooks/test-guard-push` is its table test, mutation-checked, and runs in gate step 4.
  It matches command text, so a shell command that merely mentions those strings (a heredoc
  editing docs) is refused too; use the Edit tool for such text. It stops a convenient bypass,
  not a determined one (a wrapper script, or deleting the hook file).
- Claude's waits on background work are bounded (Engineer, 2026-10-12, after a wait loop whose
  condition could never come true ran unseen for three hours): `.claude/hooks/guard-wait`
  refuses a Bash command with a `while`/`until` loop that sleeps unless the whole command
  starts with `timeout N`; `scripts/wait-for SECONDS COMMAND` polls with a deadline. It fails
  closed; `.claude/hooks/test-guard-wait` is its table test (hand-mutated: every planted bug
  caught) and runs in the gate after the push guard's. Like guard-push it matches text, so a
  command that only mentions such a loop is refused too.

### Not in the gate

`cargo mutants` (slow; see §9), `cargo fuzz` (nightly), `criterion` benches, wall-clock benches.

## 9. Testing

### Fidelity oracles (gated)

"No panic plus a well-formed model" only re-tests comrak's robustness. Lowering errors (dropped
nodes, lost tightness, wrong link targets) need oracles:

1. **Blocks and 2. styled characters** (M1; `crates/core/tests/spec.rs`, `tests/common/`).
   Both the expected HTML of every example in `spec.txt` (v0.29, 672 examples) and
   `extensions.txt` (30) and our model are reduced to the same two views: the sequence of blocks
   with their containers (quote, list and item, alert, footnote) and kind (paragraph, tight
   paragraph, heading level, code, rule, table with row count, alert title); and every visible
   character with its style (emphasis, strong, strikethrough, code, footnote reference) and
   whether it is in a link, whitespace collapsed to word gaps. Known, intended deviations are in
   `crates/core/tests/data/spec/deviations.txt`, each with a reason; the list is exact both ways.
   This replaces draft 3's "test-only HTML serialiser": rebuilding nested HTML from flat leaves
   would test the serialiser as much as the model, while the two views compare exactly what the
   renderer draws. Mutation-checked: planted bugs in tightness, emphasis, links, soft and hard
   breaks, code spans, quotes, footnotes, heading levels, table headers, raw `<pre>` and span
   merging are all caught.
3. **GitHub corpus** (fetched 2026-10-11 by `scripts/fetch-corpus`, in
   `crates/core/tests/data/corpus/`; `crates/core/tests/corpus.rs` from M3). 21 READMEs under MIT, Apache-2.0, BSD-3, Unlicense or CC0,
   each pinned to a commit with its licence and GitHub's own repo-view HTML; plus two synthetic
   inputs of ours (`_synthetic-features`, `_synthetic-front-matter`) rendered by GitHub's
   repository file view. Compared on text, styles, link targets and images (the styled-character
   view of oracles 1 and 2, with images as characters so `<br>` trimming is exact) and, in
   `tests/gfm.rs`, heading ids. Known differences are substitutions in
   `tests/data/corpus-deviations.txt` (GitHub's text, ours, why), exact both ways. GitHub's
   page chrome is normalised in the oracle: the `target="_blank"` link around each image,
   screen-reader-only text, footnote href hashes, math `$` delimiters, video wrappers, and image
   source rewrites (camo, `/blob/` to `/raw/`, uploaded assets). Block structure is not
   compared for the corpus (raw HTML segments blocks differently; oracles 1 and 2 cover blocks
   on the spec), and the corpus has one small raw HTML table: a synthetic input of HTML tables
   (`th`, `colspan`, nesting) rendered by GitHub is the next addition. `POST /markdown` is **not** used: it
   skips alerts, math and emoji and renders front matter as a heading. Signed image-URL tokens are
   stripped at fetch.
- A count floor on each oracle: finding fewer examples than expected fails.
- Per-option checks (`tests/gfm.rs`): one input per comrak option showing it on or off as on
  GitHub; heading anchors compared with the ids GitHub gave the synthetic corpus file.
- `insta` snapshots: not adopted; the oracles above and the per-option checks cover what they
  would.

### Robustness

- `proptest`: arbitrary bytes and Markdown-shaped strings → `core::load` never panics; invariants:
  offsets on char boundaries, spans in bounds and non-overlapping, every id resolves.
- `proptest` is in place from M1 (`tests/properties.rs`, 512 cases each, plus every corpus input).
- `cargo-fuzz` (later; dated nightly pinned in a separate image stage): the same targets, plus comrak's
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
  **Decided the startup path (plain `gtk::init`) and the first-screen `prepare`; the renderer
  is decided after M2 (§11).**
- **M1 core model.** Decode, `gfm_options`, lowering to leaves, inlines, newtypes, HTML lowering
  skeleton; oracles 1 and 2; proptest. *Done 2026-10-11:* 688 of 702 spec/extension examples
  match, 14 listed deviations (front matter, autolink extension, raw HTML tables as text,
  inline HTML across blocks, tag filter, one comrak/cmark-gfm autolink difference). Raw HTML
  in M1 already covers `<pre>` (code block), `<blockquote>` (quote), `<details>`, alignment,
  images, links and inline styles. The renderer still shows M0's spike paragraphs until M2.
- **M2 renderer v1.** Fenwick tree, first-viewport layout, worker heights, anchoring, caches;
  M2 features; scroll scenarios; counters within budget. *Built 2026-10-11* (§4.5 "As built").
- **M3 GFM completeness.** Tables, tasks, links, footnotes, alerts, anchors, HTML lowering in
  full, front matter, math, emoji; oracle 3. *Done 2026-10-12:* oracle 3 green on all 23
  corpus inputs with 8 listed deviations; spec/extension oracles 684 of 702 (four raw-HTML-table
  deviations went, 6 for GitHub's link protocols came); §4.5 "As built in M3", §5's known deviations.
- **M4 images.** Placeholders first; local and remote fetched lazily once visible (§4.6).
  *Done 2026-10-12:* §4.5 "As built in M4", §5's image deviations.
- **M5 syntax highlighting.** Lazy syntect. *Done 2026-10-12:* §4.5 "As built in M5".
- **M6 viewer features.** Reload on
  change (`gio::FileMonitor`); keyboard navigation; zoom; dark mode; `.desktop` file and MIME
  association; install to `~/.local`. *Done 2026-10-12:* §4.5 "As built in M6".
- **After v1:** text selection and copy, find (Ctrl+F), accessibility (`AccessibleTextImpl`).

## 11. Decisions from the user

Answered 2026-10-11:

- Remote images: render a placeholder, fetch asynchronously once the placeholder is visible.
- Text selection, copy and find: not in v1.
- Binary name: `gbk-markdown`.

- GitHub corpus: approved; fetched.
- Claude Code push-guard hook: approved; installed (§8).

Answered 2026-10-12:

- Renderer: Cairo by default (70-90 ms faster first frame; scrolling about even after the M2
  review fixes), `--renderer=gl|cairo` to choose, overriding `GSK_RENDERER`; the one `unsafe`
  exception accepted as worth its cost (§4.4). GL's possible advantages, not yet measured: large
  (maximised, HiDPI) windows, where Cairo's per-frame CPU cost grows with area, and GPU
  textures for M4 images and per-leaf caching.

Open: none.

## 12. Known risks

- Selection, find and accessibility in the custom widget are real work after v1; the (leaf,
  `ByteOffset`) addressing keeps the model ready for them.
- Remote image fetches contact third-party hosts (badge services, trackers) when a document is
  viewed; this is the chosen behaviour, and nothing is fetched before first paint. A README can
  also point at addresses on the local network (`http://192.168.1.1/...`): a GET is made, its
  answer only shown as an image if it is one. `--offline` turns fetching off; a default-off or
  per-document setting is a candidate for M6.
- A decoder that never returns (glycin, out of process) keeps one of the four loading threads;
  its image fails after 30 s, but enough such images would leave none.
- Worker heights may not match main-thread layout exactly; the equality test says so, and the
  idle-slice fallback exists.
- The renderer floor may dominate TTFP; §4.4's choices are the lever, decided on numbers.
- Notices are drawn by the custom widget, so a screen reader does not hear them: part of the
  accessibility work after v1.
- Remote images are fetched when a document is viewed (§11's choice); M6 keeps those already
  shown across reloads, so a save fetches nothing again. A default-off or per-document setting
  is still open (asked of the user at M6's end).
- Holding Page Down: each press lays out where it lands (8 ms at most) before the frame; a
  layout that does not fit in that falls to the next frame. The scroll benchmark drives the
  adjustment, not keys, so this is not measured in frames (no key repeat under Xvfb).
- A relative `.md` link starts a new viewer process: fast (it is the startup path), but each
  is a window of its own; in-place navigation with history is a possible later change.
- The highlighter runs third-party parsing (syntect, fancy-regex, 213 grammars) on a thread,
  and the release profile aborts on panic, so a panic there closes the viewer; the panic hook's
  message is the only record (Protect). A higher rung would need `panic = "unwind"` and
  `catch_unwind` around the thread, giving up abort's smaller, simpler binary for a failure not
  yet seen: the M5 review compiled every pattern of every grammar (syntect `expect`s that
  they compile on first use) and ran all 213 grammars over the fixtures and corpus without one.
- A two-face upgrade can rename scopes and change colours; the class tests say so, and the
  answer is to check the mapping against GitHub again, not to loosen them.
- Jumping deep into one long code block waits for everything before it to be read (3 s for
  the end of code-1mb): a grammar cannot start mid-block.

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
