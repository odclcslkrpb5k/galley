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

Push guard (installed 2026-10-11): `.claude/hooks/guard-push` refuses Bash commands whose text
contains `--no-verify` with `git`, or mentions `core.hooksPath` (except setting it to
`.githooks`). Text match, so a heredoc mentioning those strings is refused: edit such text with
the Edit tool. Never route around it; fix the gate.

Open questions (awaiting user, see `docs/plan.md` §11):
- Renderer: Cairo with GL disabled (60-100 ms faster first frame) vs GL; needs `unsafe` set_var
  or a .desktop env. Proposed: decide after M2 scroll numbers.
- Theme: GTK4 + light stylesheet (not yet confirmed).

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
  counted constructor in `document_view/imp.rs` (both enforced by `disallowed-methods`).
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

