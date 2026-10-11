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

Open questions (awaiting user, see `docs/plan.md` §11):
- Claude Code PreToolUse hook denying `git push --no-verify` / hook-path overrides.
- Theme: GTK4 + light stylesheet (not yet confirmed).

`docs/plan.md` is the authoritative plan; this file records decisions and state.

## Planned layout (not yet created)

- Cargo workspace with two crates:
  - `core`: parsing, document model, layout maths. **No GTK dependency**, so the crate boundary
    stops core code calling GTK. All headless tests and fuzzing target this crate.
  - `app`: GTK/Pango/file I/O/images. The adapter layer; converts `glib::Error`/`io::Error` into
    our own error enums, classifying by `io::ErrorKind`, never by message text.
- `rust-toolchain.toml` pins one toolchain; the Dockerfile uses the same file.
- One script: fmt check -> clippy `-D warnings` -> tests -> cargo-deny -> build (`--locked`).
- `Cargo.lock` committed.

## Paranoid Rust rules (applied to this repo)

Enforced (fail the build):
- `[lints]` in Cargo.toml, clippy run with `-D warnings` (this also makes `unknown_lints` fatal, so
  a lint the pinned toolchain doesn't know can't be silently ignored).
- `unsafe_code = "forbid"`.
- clippy: `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `unreachable`,
  `panic_in_result_fn`, `unwrap_in_result`, `indexing_slicing`,
  `string_slice`, `as_conversions`, `wildcard_enum_match_arm`, `let_underscore_must_use`,
  `map_err_ignore`, `allow_attributes`, `allow_attributes_without_reason`; `pedantic` as warn.
- Exceptions only via `#[expect(lint, reason = "...")]`.
- `clippy.toml`: allow unwrap/expect/panic/indexing in tests only.
- Release profile `overflow-checks = true`.

Conventions:
- Parse at the boundary: file bytes -> UTF-8 decision (refuse or lossy-with-notice, never panic);
  comrak AST converted once into our own block types; renderer never sees comrak types.
- `thiserror` enums in `core`; `anyhow` banned workspace-wide (cargo-deny).
- Newtypes for easily confused quantities: byte offset, char offset, logical px, device px,
  Pango units (px * 1024), line number. One constructor each.
- Panic hook logs the panic message (panics in GTK callbacks abort rather than unwind).
- Tests: return `Result` and use `?`, or compare errors exactly; typed fixture builders.

Testing:
- GFM spec examples as a test suite; a gate that finds zero examples fails.
- `cargo-fuzz` / `proptest`: arbitrary input never panics through parse + layout.
- `cargo-mutants` for tests written against a review finding.
- `cargo-deny` for licences, bans, advisories.
- Performance: a time-to-first-paint benchmark is planned.

Not applicable here (service-specific): shadow-mode production reads, retry/refusal policy, unknown-outcome
writes, credentials in URLs.

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
