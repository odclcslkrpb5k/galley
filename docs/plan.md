# gbk-markdown: implementation plan

Status: draft 3, 2026-10-11; M0-M3 complete (see §10, docs/perf-baseline.md). Draft 2 was revised after a three-way red-team (performance; GFM and
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
- Later: `syntect` 5.x with `default-fancy` (pure-Rust regex).

## 3. Repository layout

```
Cargo.toml              workspace; [workspace.lints]; every member has [lints] workspace = true
rust-toolchain.toml     channel = "1.99.0", components = clippy, rustfmt
clippy.toml             test-only allowances
deny.toml               cargo-deny (§7)
Dockerfile              build image (§8)
scripts/check           the gate (§8)
scripts/bench-startup   TTFP report, host only; interleaved configs, CI on differences (§6)
scripts/in-docker       runs a command in the build container
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
  back to the leaf's `ByteOffset`s (placeholders shift them) was thought needed for M3's link
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
- **Not yet:** long code lines wrap instead of scrolling sideways,
  images are `[alt]` placeholders (M4). Leaf estimates for the first width are computed on the
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
| Images | §4.6 | M4 |
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
`--click-at=X,Y` clicks there through the widget's own handling (never opening anything outside
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

Licence allow-list (MIT, Apache-2.0, BSD-2/3, Zlib, Unicode-3.0, MPL-2.0; a new one fails the gate
until reviewed; Unicode-DFS-2016 is allowed for `finl_unicode` alone, as a per-crate exception), advisories with the current syntax (the removed `vulnerability = "deny"` key
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
- Run as `--user $(id -u):$(id -g)`; repo at `/work`; named volumes on `/cargo/registry`, `/cargo/git` and `/cargo/advisory-dbs` (registry, git,
  advisory database); `CARGO_TARGET_DIR=/work/target/docker` so host tools (rust-analyzer) do not
  contend for the same lock.
- Image tag = hash of `Dockerfile` + `rust-toolchain.toml`; `scripts/check` builds it if missing.
- `docker run --init`: otherwise the first command is PID 1, and `xvfb-run` as PID 1 waits
  forever for its X server (it hung three times in M3).

### The gate: `scripts/check`

1. Refuse to run if `git status --porcelain` is non-empty (the gate checks a commit, not a
   working tree).
2. `cargo fmt --all --check`
3. `cargo clippy --workspace --exclude gbk-markdown-lint-canary --all-targets --locked -- -D warnings`
4. Lint canary (§7), the boundary grep (§3), `.claude/hooks/test-guard-push`, plus the `unsafe`
   count check if §4.4 needs it.
5. `cargo nextest run --locked` over the default members (the canary is excluded via
   `default-members`). nextest fails when it finds
   no tests; a committed minimum test count per binary catches a test suite that shrinks
   silently.
6. `cargo deny check`
7. `cargo build --release --locked`
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
  viewed; this is the chosen behaviour, and nothing is fetched before first paint.
- Worker heights may not match main-thread layout exactly; the equality test says so, and the
  idle-slice fallback exists.
- The renderer floor may dominate TTFP; §4.4's choices are the lever, decided on numbers.
- Theming: GTK4 defaults to Adwaita on Xfce; a small stylesheet aligns it.
- A relative `.md` link starts a new viewer process: fast (it is the startup path), but each
  is a window of its own; in-place navigation with history is a possible later change.
- Link failures are invisible without a terminal until M6 adds an in-window notice.

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
