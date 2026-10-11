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

Open questions:
- GTK4 defaults to Adwaita, not the Xfce theme. Plan: GTK4 plus a light stylesheet. Not yet confirmed with the user.
- `clippy::indexing_slicing` may be too noisy in layout code; user hasn't ruled on it.
- Syntax highlighting: `syntect`, loaded lazily so it stays off the startup path. Not yet started.

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
  `panic_in_result_fn`, `unwrap_in_result`, `indexing_slicing` (pending user call),
  `string_slice`, `as_conversions`, `wildcard_enum_match_arm`, `let_underscore_must_use`,
  `map_err_ignore`, `allow_attributes`, `allow_attributes_without_reason`; `pedantic` as warn.
- Exceptions only via `#[expect(lint, reason = "...")]`.
- `clippy.toml`: allow unwrap/expect/panic/indexing in tests only.
- Release profile `overflow-checks = true`.

Conventions:
- Parse at the boundary: file bytes -> UTF-8 decision (refuse or lossy-with-notice, never panic);
  comrak AST converted once into our own block types; renderer never sees comrak types.
- `thiserror` enums in `core`; no `anyhow` in `core`.
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
  Stack and rules agreed; scaffold not yet written. Next: workspace, Dockerfile, toolchain pin,
  lint config, check-then-build script; first commit + push.
