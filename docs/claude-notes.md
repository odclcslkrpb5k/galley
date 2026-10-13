# Claude notes

Durable context for Claude sessions on this repo. Read this first. Keep it current: update a
section when a decision changes, record why, and date entries (YYYY-MM-DD).

## Ground rules (from the user)

- Write only inside this repo.
- Claude owns git. No team, no PRs: commit and push straight to `main` when appropriate.
- No CI. Builds are local, inside Docker. The test suite runs before every build.
- The user is new to Rust and relies on Claude to research and apply best practices. Explain
  Rust-specific choices briefly when making them.
- User's desktop: Xubuntu 26.04 (Xfce, GTK3-based desktop; GTK4 available).
- Classify every proposed fix on the Hierarchy of Defect Controls (Eliminate > Substitute >
  Engineer > Administer > Protect), name the rung, and justify any Administer/Protect choice by
  saying which higher rung was considered. Applies to Claude's own repeated mistakes too
  (plugin skill `hierarchy-of-defect-controls`; load it at session start).

## Product

Lightweight GUI Markdown viewer for Linux.

Optimisation goals, in priority order as stated:
1. Low time to first rendered paint.
2. Fast scrolling.
3. Good GFM (GitHub Flavored Markdown) support.

## Decisions

| Date | Decision | Why |
|---|---|---|
| 2026-10-11 | Rust + GTK4 (gtk4-rs) + comrak | comrak ports cmark-gfm and passes the GFM spec; native rendering beats a webview on startup |
| 2026-10-11 | Native rendering: comrak AST -> own block model -> Pango layouts in a custom scrollable widget, laying out only blocks near the viewport | Startup and scroll cost independent of document size |
| 2026-10-11 | Rejected WebKitGTK (and Tauri, which uses it) | Web process spawn costs hundreds of ms and ~100 MB before first paint |
| 2026-10-11 | Build environment is Docker, base image `ubuntu:26.04` | Binary runs on the host and links the host's GTK4; matching glibc/GTK versions avoids mismatches |
| 2026-10-11 | Adopt a Rust version of the "paranoid TypeScript" rules (see below) | User's standard from `~/Projects/hygeia-standard/docs/paranoid-typescript.md` (read-only; outside this repo) |
| 2026-10-11 | `app` must not depend on comrak; cargo-deny `wrappers` restricts comrak to `core` | Eliminate: renderer can't see parser types; deny rule keeps it so |
| 2026-10-11 | `anyhow` banned via cargo-deny | Engineer: errors stay typed enums in our own words |
| 2026-10-11 | Keep `clippy::indexing_slicing`; iterate instead of index, `#[expect(..., reason)]` for the rest | Engineer lint; most hits removed by Eliminate rewrites |
| 2026-10-11 | Flat leaf document model (lists, quotes, tables, code chunked into leaves) | Red-team blocker: per-block virtualisation can't meet TTFP on big lists/tables/code |
| 2026-10-11 | No text selection, copy or find in v1; keep (leaf, ByteOffset) addressing | User decision; addressing keeps them addable later |
| 2026-10-11 | Remote images: placeholder first, fetch async once placeholder is visible | User decision |
| 2026-10-11 | Binary name `gbk-markdown` | User decision |
| 2026-10-11 | Budgets split: GTK floor (measured) + our added cost; work counters gate, wall clock reports | Red-team: GTK startup may exceed a fixed TTFP budget |
| 2026-10-12 | Remote images via ureq 3 + rustls (ring), system certificates (platform-verifier); ISC licence allowed after review | M4; no bundled roots, no OpenSSL; startup unchanged within 1 ms |
| 2026-10-12 | Cairo renderer by default, `--renderer=cairo\|gl` overrides GSK_RENDERER; `unsafe_code` deny with one exception (set_var in main) | User decision: 70-90 ms faster first frame, scrolling even; check-boundaries keeps it the only unsafe |
| 2026-10-12 | Syntax highlighting with syntect 5.3 (fancy-regex, parsing only) + two-face 0.5.2 (bat's 213 grammars) | M5; syntect's own 75 lack TOML/TS/Dockerfile/fish/Nix; pure Rust, no Oniguruma; binary 4.3 -> 7.2 MB, startup unchanged |
| 2026-10-12 | Tokens are colour only (GitHub's classes), highlighter thread started after the first frame, read ahead 16 KiB | M5; heights cannot change, so the worker measures uncoloured and check-heights proves it; nothing before first paint |
| 2026-10-12 | Dark mode follows GTK's interface colour scheme (portal), else a `*-dark` GTK theme name; `--theme` overrides | M6; Xfce's portal states no preference; the palette already followed the theme |
| 2026-10-12 | Zoom scales the text setup (em), browser steps 50-400%; images keep pixel size | M6; reuses the DPI-change path, so the worker and anchoring follow |
| 2026-10-12 | Install records the Markdown defaults it replaces and gives them back on uninstall (where ours is still the default) | M6; xdg-mime default replaces the whole list; run only with the user's go-ahead (writes outside the repo) |
| 2026-10-12 | Reload: FileMonitor on the canonical path after the first frame; 200 ms after a save ends, 2 s after a bare write, 1 s at most; through set_document; shown images and code colours kept | M6 reviews: symlinks, slow writers, continuous rewrites, badges fetched on every save |
| 2026-10-12 | Notices drawn by the view (typed Message), not a GTK overlay widget | M6; one widget, no new allocation path; the layout is made outside frames; not heard by screen readers (after v1) |
| 2026-10-12 | `--theme=light|dark` via GTK_THEME before init | M6 review: setting the colour scheme after init reparsed the theme, 15-33 ms before the first frame |

Push guard (installed 2026-10-11): `.claude/hooks/guard-push` refuses Bash commands whose text
contains `--no-verify` with `git`, or mentions `core.hooksPath` (except setting it to
`.githooks`). Text match, so a heredoc mentioning those strings is refused: edit such text with
the Edit tool. Never route around it; fix the gate.

Open questions (awaiting user, see `docs/plan.md` §11):

- Benchmarks: the default is now Cairo, so a GL comparison needs `-c gl:--renderer=gl` in
  bench-startup (an empty configuration is Cairo).

`docs/plan.md` is the authoritative plan; this file records decisions and state.

## Where the rules and layout live

Not repeated here, so there is one source each (an earlier copy here drifted from the code):
- Repo layout, architecture, gate steps, testing: `docs/plan.md` (§3, §4, §8, §9).
- Lint rules: `[workspace.lints]` in `Cargo.toml` and `clippy.toml`; rationale in plan §7.
  `scripts/lint-canary` proves each configured lint fires.
- Measurements: `docs/perf-baseline.md`.
- Test style: tests return `()` and use `.expect("what")` (`panic_in_result_fn` fires on
  `Result`-returning tests that assert).
- Output: only `crates/app/src/output.rs` writes stdout/stderr; Pango layouts only through the
  counted constructor `new_layout` in `render/text.rs` (both enforced by `disallowed-methods`).
- Not applicable here (service-specific parts of the paranoid standard): shadow-mode production
  reads, retry/refusal policy, unknown-outcome writes, credentials in URLs.

## Status log

- 2026-10-11: Repo empty (no commits). Remote `origin` = github.com:odclcslkrpb5k/gbk-markdown.
  Stack and rules agreed; scaffold not yet written.
- 2026-10-11: `docs/plan.md` draft 3 after three-way red-team + user answers. Next: M0.
  Verified facts from red-team worth keeping: host is X11 so GTK uses the GL renderer (Vulkan is
  Wayland-only); xfwm4 gives no frame presentation times; `-D warnings` does NOT make
  `unknown_lints` fatal (set it deny at priority -100); `panic_in_result_fn` fires in tests;
  comrak 0.56 removed `tagfilter` and `header_ids` (use `Anchorizer`). Next: workspace, Dockerfile, toolchain pin,
  lint config, check-then-build script; first commit + push.
- 2026-10-11: GitHub corpus fetched (`scripts/fetch-corpus`, 21 READMEs + 2 synthetic). Learned:
  `POST /markdown` is not github.com's pipeline (no alerts/math/emoji; front matter becomes a
  heading), so synthetic inputs are rendered via the contents API (repo file view) after push;
  `/license?ref=` returns NOASSERTION for files detected fine without a ref; uploaded-image URLs
  carry 5-minute JWTs (stripped at fetch). GitHub: `> [!NOTE] Title` is a plain quote,
  `[!note]` lower-case is an alert, front matter renders as a table.
- 2026-10-11: M0 gate green and pushed (d2147b8). Workspace (core, app, lint-canary), Dockerfile (pinned by digest/sha256),
  `scripts/in-docker`, gate `scripts/check` -> `scripts/gate-steps` (fmt, clippy, lint canary,
  boundaries, push-guard test, nextest + test floor, cargo-deny, release build, Xvfb smoke),
  `.githooks/pre-push` (tree-hash stamps in target/gate/). Measurements in docs/perf-baseline.md.
  Learned: GTK floor here is ~190-250 ms (Cairo) / ~250-350 ms (GL), dominated by xfwm4 map
  latency and GL init; plain gtk::init beats gtk::Application by 20-45 ms (now default);
  fat LTO no startup effect. `clippy::unwrap_in_result` does not fire on `Ok(opt.unwrap())`
  (1.99), only on the documented shape. `ScrollablePolicy` has no Default (impl by hand).
  Workspace `default-members` excludes the lint canary so plain cargo commands skip it.
  perf is unavailable (perf_event_paranoid=4); use strace -T for wait analysis.
  M0 shortcuts to undo in M1/M2: spike paragraphs (app/src/spike.rs), Extent is O(n) with raw
  f64 px (no LogicalPx newtype yet), decode notice ignored in load().
- 2026-10-11: M0 review follow-up (three reviewers: correctness, performance, coherence).
  Fixed: push-guard bypasses (abbreviated long options such as `--no-verif`, which git accepts;
  an allowed line carrying a bypass on the next line); draw/layout window ignored MARGIN (one
  `extent_span` helper now); smoke work gate graded itself (now a 32 KiB byte budget); probe
  scroll metrics (refresh-based drops, busy time, reached_end, no defaulted zeros); stdout/stderr
  and Pango layout creation each confined to one place by `disallowed-methods`; loader joined
  after present; first screen laid out right after present (`prepare`, measured -10/-24 ms);
  bench-startup rewritten (interleaved, CI, load check); test floor ignores #[ignore]d tests;
  boundary grep fails on grep errors; screenshots kept per tree. Docs: notes no longer duplicate
  rules (pointers instead), plan §1/§3/§6/§8/§10 match the code.
  Machine state matters: during this session the host had swap full (Firefox, VS Code), load
  ~4; bench-startup refuses to run then. Absolute timings from such runs are not comparable.
  Open: is before-paint -> after-paint busy time real work or waiting? Check in M2.
  The push guard refused one of Claude's own heredocs that quoted the flag (expected false
  positive): doc text that names guarded strings goes through the Edit tool.
  Mutation testing: the first cargo-mutants run was INVALID (CARGO_TARGET_DIR from in-docker
  made every mutant copy test the same prebuilt binary; "226 missed of 300" meant nothing).
  `scripts/mutants` now unsets it and refuses a run whose control mutant is not caught. The
  corrected run was stopped by Claude Code because the host ran out of memory; not yet rerun.
- 2026-10-11: M0 mutation testing done (valid run: 300 mutants, 54 caught, 22 unviable, 224
  missed). 212 survivors are GTK glue (main, probe, output, the widget) that only the Xvfb smoke
  run exercises, ~120 of them equivalent `measure()` size-hint variants. Policy (Administer,
  deliberate): glue is covered by the smoke run, not unit tests; running the smoke run per
  mutant (Engineer) means a release build per mutant, not proportionate. The 12 pure-code
  survivors (units helpers, the explicit `--spike=paragraphs` arm) now have tests; re-run of
  those files: 33 caught, 0 missed. `scripts/mutants` now also refuses any run whose logs show
  tests run from the shared target dir (the first run's failure, checked directly), and checks
  the percentile control on unfiltered runs.
- 2026-10-11: M1 core model built. Layout: `units` (index newtypes), `model` (flat leaves with
  container stacks), `builder` (comrak-free: leaves, chunking, inline runs, raw HTML meaning),
  `html` (html5gum tokens to our own), `link` (classify, percent-decode), `parse/` (the only
  comrak code: `gfm_options`, tree walk), `load()` in lib. Tests: `tests/spec.rs` +
  `tests/common` (block + styled-character oracles over spec.txt/extensions.txt, exact
  deviations list), `tests/gfm.rs` (per-option, heading ids vs GitHub), `tests/properties.rs`.
  Learned: comrak features off by default-features=false remove shortcodes/HEEx/attributes
  types entirely; TaskItem replaces Item in the tree; spec example openers carry labels
  ("example table") and `<IGNORE>` means crash-only; the oracle needed its own fixes several
  times (tight items, raw-HTML text, cells, empty paragraphs) and each was checked against
  rendering semantics, not just made to pass; mutation-checking it found that ignoring
  whitespace hid dropped soft breaks, so it now keeps word gaps.
  Open for M2: table-1mb loads in 75 ms (profile comrak vs lowering); renderer still uses
  spike paragraphs.
- 2026-10-11: M1 review follow-up (three reviews: correctness/fidelity 17 findings, performance
  8, coherence 11). Rebuilt raw-HTML handling around one document-wide tag stack (scopes,
  cached derived state, O(1) closers, depth cap 256, deferred closes when Markdown containers
  are open inside an HTML container); Markdown styles/links by #[must_use] tokens; context tree
  instead of per-leaf container stacks; private model fields + accessors; 16 KiB leaf cap;
  GitHub tag filter (shown as text, verified against github.com); heading slugs from GitHub's
  text content (checked against all 600 corpus ids); `>[!tip]` alerts; titled alerts from the
  source line; footnote first-definition-wins; CRLF normalised; protocol-relative links;
  `<details open>`; video/audio as links; summaries collect until `</summary>`. Oracles now
  compare link targets, list numbers, checkboxes, code info, alert kinds, images, hard breaks.
  App spike now renders core::load's leaves as plain text.
  Process lesson: `cargo fmt` (run inside in-docker) rewrites files, which makes the next
  whole-file Write fail as "modified since read" - three times this milestone. Fix used: read
  one line of a file before rewriting it whole.
- 2026-10-11: M1 mutation testing done (core). Full run: 501 mutants, 110 missed; tests added for
  every non-equivalent survivor (split/slice arithmetic, HTML stack bookkeeping, pre/summary/
  media paths, all style/heading/align/alert arms, model accessors, footnote numbering). Two
  real bugs found on the way: an inline `<summary>` lost its text to the paragraph; a code line
  longer than the leaf cap had no test (its chunking loop would spin if `start + 1` broke).
  Remaining survivors, all equivalent:
  - `next_token` constant: tokens only matter for out-of-order ends; the walker always nests.
  - `high << 4 | low` vs `^`: the bits never overlap.
  - `floor_char_boundary` `>`/`>=`: index 0 is always a char boundary.
  - `trim_end` `>`/`>=`, `close_html` `>`/`>=`, `ensure_started` `>`/`>=`: the equal case is a
    no-op (span already ends there; stale deferred token popped later; container already gone
    is not reachable since HTML closes are deferred).
  - `close_containers_from` `&&`/`||` with `details_from`: flushing a summary that is not open
    only flushes pending text, which `end_container` does anyway.
  - `append` `&&`/`||`: `ends_with_space` is already true for empty text.
  - `split_inlines` `cut > start + max/2` vs `>=`: a space exactly at the midpoint may or may
    not be a cut; both are valid.
  - `Footnote::number` -> 1 survived only before the first-reference ordering test was added.
- 2026-10-11: M2 renderer built (`crates/app/src/render/`: fenwick, paint, text, heights; widget
  in `document_view/imp.rs`). Spike and Extent removed. Smoke asserts 0 worker/main height
  mismatches and 0 layouts in scroll frames. Learned: pangocairo cannot read back a context's
  font options (derive from gtk::Settings); the widget's colour is wrong until styled (sync the
  palette in snapshot); idle-only prefetch loses to fast scrolling (after-frame slice + 2-screen
  window fixed it); scroll benches must be interleaved, as a non-interleaved set at load 4
  reversed the GL/Cairo result. Never pass `--workspace` to clippy/test by hand: it pulls in
  the lint canary (which must fail); the gate excludes it.
- 2026-10-11: M2 review follow-up (correctness 8 findings, performance 9, coherence 10). Fixed:
  a single huge leaf froze first paint (2 MB code line: no frame in 120 s; 40 KB table cells:
  68 s) -> `paint::split` caps every paint (2 KiB at line breaks or in unbreakable runs, 16 KiB
  for prose), smoke has huge-* fixtures and a 5 s first-frame bound; anchoring moved into pure
  `render::positions` (single writer of heights; prefetch no longer bypasses the hold while
  scrolling; shift applied before clamping; resize keeps the reading position; own corrections
  not taken for user scrolling); one `Typesetter::measure` and one `Metrics::geometry` shared
  by worker, check and draw; GtkScrollbar replaced by a drawn indicator (`render::indicator`):
  the scrollbar cost ~16 sync X round trips per scroll frame; fonts warm while waiting on the
  loader; `--check-heights` now compares worker heights for every leaf; worker death reported;
  settings (font/DPI/hinting) changes re-lay out; probe busy time includes the after-frame
  prefetch; `--spike` renamed `--empty-window`; clippy config warnings (a disallowed path that
  does not resolve only warns) now fail the gate.
  Process lesson (recurring): exact-text Python replacements fail after `cargo fmt` reformats a
  test. Engineer fix used: a scratchpad helper that replaces a `#[test]` fn by structure
  (`scratchpad/tools/replace_test.py`, session-local; recreate if needed: find `#[test]\n fn
  NAME(` and cut to the next `#[test]`), or index-based replacement between fn headers.
  Deferred, recorded in plan §4.5: paint-to-source offset map (M3 links), estimates on the
  loader thread, per-leaf textures for scroll, list marker baseline on `- # heading`.

- 2026-10-11: M2 mutation testing (app pure modules: fenwick, paint, geometry, positions,
  indicator, units): 421 mutants, 34 missed -> 0 after tests for exact estimates, indented
  widths, table text and gaps, details/alert/footnote indents, attrs at a cut, thumb end; and
  three Eliminate/Substitute fixes for dead code the survivors exposed (`cuts`' space filter
  was unreachable, `centred_on`'s value override unread, own `floor_char_boundary` replaced by
  std's `str::floor_char_boundary`). Timeouts (17) are infinite loops, i.e. caught. GTK glue
  (widget, text, heights worker) stays covered by the smoke run, per the M0 policy.

- 2026-10-12: M3 (GFM completeness) built and reviewed. Core: oracle 3 (`tests/corpus.rs`,
  23 READMEs vs GitHub's HTML; text, styles, links, images; 8 listed deviations), raw HTML
  tables lowered to tables (colspan, details in cells), front matter table, shortcodes, link
  protocols as GitHub's sanitiser, raw `<a id>` anchors (one rule: `model::anchor_name`),
  default "Details" summary, `<br>` trimming. App: tables as grids (`render/table.rs`), links
  resolved at paint time (`Target`), hit testing, hover, Alt+Left back, `--anchor`,
  `links.rs` (relative paths, safe opening: allowlisted types direct, others via the chooser,
  executables never), `<details>` collapse, display math centred, probe `--link-at` and
  `--click-at` (smoke checks a link jump and a summary opening). Plan §4.5 "As built in M3"
  and §5 list what is deferred (rowspan, `<th>` per cell, inline table tags, headings in
  summaries, in-window error notice, `LeafPaint` text/table as an enum).
  Learned: GitHub's sanitiser drops links whose "protocol" (text before the first `:` with no
  `/` or `#` before it) is not allowed, which is why `[x](javascript:...)` and `[a](b)` badge
  typos are text there; GitHub wraps every image in a `target="_blank"` link and adds
  screen-reader text ("Loading" on diagrams), which the oracle must ignore.
  Process lessons (each fixed on a higher rung than "be careful"):
  - `xvfb-run` as a container's PID 1 waits forever: `scripts/in-docker` runs `--init`
    (Engineer; hung three times first).
  - Exact-text replacements kept breaking after `cargo fmt` (third milestone running): the
    session uses a whitespace-blind edit helper, `scratchpad/tools/wsedit.py` (match with all
    whitespace removed; `(start, end)` marker pairs for spans rustfmt rewrote). Recreate it if
    the scratchpad is gone; it is not in the repo because it edits the repo.
  - `pkill -f PATTERN` inside a command whose own text contains PATTERN kills that command
    (Eliminate: stop stuck tasks with the task tool, never pkill -f in a compound command).
  - Smoke helpers that set `fail`/`n` must not run as `$(...)` (a subshell drops them):
    `probe_run VAR ...` assigns with `printf -v`. And a bash `((a > b))` on a non-number is an
    error a condition reads as false, so smoke now requires each counter to match ^[0-9]+$
    (a layout budget passed vacuously once before this).
  Closed 2026-10-12 (pushed fa01c97): three reviews (correctness 14 findings, performance 11,
  coherence 15) addressed or recorded as deviations in plan §5; mutation testing of the new pure
  code: 725 mutants (53 missed) -> tests or Eliminate rewrites for every non-equivalent one;
  the rest are equivalent and named in commit 7f69d89. Perf: first frames as M2 within noise,
  table-1mb scroll drops 1-3 (M2 11-18); docs/perf-baseline.md "M3".
  Next: M4 (images), awaiting the user's go-ahead.
- 2026-10-12: M4 (images) built, reviewed and pushed. Images stay at their object character
  with a Pango shape attribute; sizes from a shared ImageTable (worker and view agree:
  `--check-heights --wait-images` 0 mismatches); images.rs loads once drawn while still, after
  the frame, 4 threads, ureq/rustls, sniffed formats only (render/sniff.rs), 40 MP refusal
  before decoding, 2048 px decode cap, 30 s deadline, dedup by source, 256 MB texture budget.
  Reviews: correctness 11 findings (worst: /proc/self/pagemap read until OOM; gdk-pixbuf 2.44
  sends PNG/JPEG/GIF to glycin, so the in-process list was wrong), performance 11 (redraw every
  16 ms while loading; arrivals laid out in frames, which the smoke caught), coherence 15.
  Learned: gdk-pixbuf on Ubuntu 26.04 decodes most formats out of process with glycin, which
  hangs forever if a size callback asks for 0 × 0, so sniff sizes ourselves and never ask
  gdk-pixbuf to stop; glycin warns "running without sandbox" in Docker (no bwrap there).
  Process: save multi-file edit scripts to the scratchpad before running them (a failed
  anchor then costs a re-run, not a re-type; it happened twice this milestone).
  Mutants: 39 survivors first, 5 after new tests, all equivalent (commit 0313a44). Perf: no
  layouts in scroll frames; busy times match M3's binary interleaved on the same day (a lone
  run looked 1-2 ms worse: always A/B against the previous binary, never against old tables);
  table-1mb drops ~1.5 more frames per 3000 (watch item for M5).
  Next: M5 (syntax highlighting), awaiting the user's go-ahead.
- 2026-10-12: M5 (syntax highlighting) built, reviewed and pushed. syntect + two-face on a
  thread started after the first frame; only named code (aliases as a const table; console
  sessions colour commands only); GitHub's colours, colour only, so heights cannot change
  (worker measures uncoloured; check-heights with highlighting: 0 mismatches incl. code-1mb).
  A paint asks for its code plus 16 KiB (again at half of it); least-left-first steps of 100 lines or 15 ms; a block
  stops at a 4 KiB line, a failing grammar, or 500 ms + 10 ms/KiB; stale cache entries are
  redrawn by prefetch, never in a frame. Reviews: correctness (no high findings; all 21 739
  grammar patterns compile, so syntect's expect cannot abort us), performance (double layout
  while scrolling: 1148 vs 573 layouts, fixed by read-ahead -> 643; seconds-long steps;
  attribute count), coherence (12: state enum, stale in the cache entry, vacuous gates).
  Learned:
  - syntect's newline grammars need every line to end in "\n"; core strips a block's last.
  - Measure a binary-size change against the previous binary, not a spike's baseline: the
    spike said +0.6 MB (two-face vs syntect's grammars), the real change was 4.3 -> 7.2 MB.
  - A real-clock time limit made tests flaky in a debug build under mutation load, and the
    flake hid 13 surviving mutants as "caught" (Engineer: limits are parameters, tests use a
    fake clock or an unhurried budget). A mutation run with 0 missed deserves a second look
    when the code has time-dependent paths.
  - Don't `grep -l | xargs sed` across the tree: it touched only code by luck (corpus files
    matched the word); name the files.
  Known gap, not M5's: 9 surviving mutants in text.rs measure_table (M3/M4: the stale-widths
  check `measured < arrived`, cell wrap width, `left` advance), found by M5's filtered run.
  Next: M6 (viewer features), awaiting the user's go-ahead.
- 2026-10-12: M6 (viewer features) built, reviewed and pushed. Keys (navigation.rs, in
  --help), zoom 50-400% (Ctrl+plus/minus/0, Ctrl+wheel summed, --zoom), --theme (GTK_THEME
  before init; the *-dark name fallback after), reload on change (imp/reload.rs: canonical
  path, after the first frame, 200 ms / 2 s / 1 s rule, empty read twice, images and code
  colours kept), notices (imp/notice.rs, typed), data/gbk-markdown.desktop and scripts/install
  (restores previous defaults; absolute Exec), linked documents inherit options.
  Probe: --press, --rewrite-with (writes 500 ms after the first frame), theme=, notices=,
  reloads=; gate: install step, desktop-file-validate; test floor 147 -> 158.
  Reviews: correctness (wheel per event, symlink never reloaded, slow writer showed empty,
  install path/PATH/uninstall issues), performance (--theme +15-33 ms, wheel relayout storm,
  continuous rewrite never reloaded, badges refetched every save), coherence (reload's second
  path, monitor before first frame, typed notices, imp.rs size). All fixed or recorded.
  Mutants: navigation/appearance/reload rule/adopt: 6 survivors left, all equivalent
  (disjoint bit flags `|` -> `^`); 2 real ones (Up/Page Up with modifiers) got cases; one guard
  removed as redundant (Eliminate). Screens: 34 identical to M5, 3 new.
  Learned:
  - The disk filled (scratch builds, superseded 2.4 GB images, mutation runs' stale test
    binaries): Engineer, scripts/in-docker refuses under 5 GiB and removes replaced images.
    Delete scratch builds as soon as their numbers are in (but not before: I deleted M5's
    binary once too early and rebuilt it).
  - A temporary borrow in an argument (`self.path.borrow().clone()` inside a call that
    replaces it) lasts the whole call: the first reload panicked. Bind it first.
  - The real xdg-mime ignores associations whose desktop file is missing and falls back to the
    machine's defaults, so a test using it passed or failed by machine (Eliminate: stubs).
  - Grepping for errors exits 1 when clean (Engineer: scripts/quick exits with the real
    status). Exact-text edits of code I had changed failed three times (rustfmt reflow, my own
    earlier edits): use scratchpad tools/wsedit.py and save edit scripts first.
  - The probe acted in the same frame callback the view starts its watch in; ordering between
    after-paint handlers is not ours to rely on: act later (a timeout) instead.
  - Benchmark order under rising load faked a 2x regression: ABBA.
  User decisions (2026-10-12): install as the default for Markdown (done: ReText recorded for
  text/markdown, nothing for text/x-markdown); remote images keep being fetched.
  The real install recorded itself as the previous default (xdg-mime falls back to installed
  desktop files' cache, which the stub did not model): fixed by recording first, the user's
  record repaired by hand, and the stub now falls back the same way, with the recorded values
  checked (proved to fail on the old order).
  Next: v1 is complete with M6; after-v1 items are in plan §10.
- 2026-10-12: A wait loop from M4 ("until no Docker container is running") ran three hours:
  this machine always has other containers. Engineer control (user-approved):
  .claude/hooks/guard-wait refuses unbounded polling loops; use
  `scripts/wait-for SECONDS COMMAND`, or `timeout N bash -c '...'`, and wait on something this
  session owns (a log line, a PID), never on global state.
