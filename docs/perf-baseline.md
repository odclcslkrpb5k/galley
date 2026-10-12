# Performance baseline

Numbers from `scripts/bench-startup` and `--bench-scroll` on the target host (docs/plan.md §6).
Each milestone appends a section; a median more than 20% worse than the previous one is flagged.

Host: Xubuntu 26.04, X11, xfwm4 4.20 compositing, Intel UHD 620 (Mesa 26), 1080p 60 Hz.
Warm disk cache. "First frame" is frame *submitted* (xfwm4 reports no presentation times),
measured from process spawn (`--t0`) to the first `after-paint` after the document is drawn.

## M0 (2026-10-11): spike, plain-text paragraphs

### Where the floor goes (empty window, Cairo with GL disabled, plain init)

| Phase | Time |
|---|---|
| spawn → `main` (dynamic loading of the GTK stack) | ~30 ms |
| `gtk::init` | 12-30 ms (GL path: 70-80 ms) |
| window built and presented | 60-150 ms, varies run to run |
| presented → first frame | 30-70 ms |

The main thread spends most of "presented" and "first frame" blocked on the X connection, after
ReparentNotify / ConfigureNotify / PropertyNotify: xfwm4 framing and mapping the window. That
latency is the window manager's; nothing paints before the window is mapped. Overriding the GTK
theme (`GTK_THEME=Adwaita`, `Default`) and disabling accessibility changed nothing measurable.
A CPU profile was not possible: `kernel.perf_event_paranoid` is 4.

### First frame, median of 10 runs (ms)

| Fixture | GL, `gtk::Application` | GL, plain init | Cairo no-GL, `gtk::Application` | Cairo no-GL, plain init |
|---|---|---|---|---|
| empty window (floor) | 349 | 332 | 253 | 208 |
| ripgrep README, 22 KB | 386 | 360 | 283 | 260 |

Later runs, plain init (the default since M0):

| Fixture | GL | Cairo no-GL | Leaves laid out before first frame |
|---|---|---|---|
| empty window | 249 | 188 | 0 |
| ripgrep README, 22 KB | 302 | 215 | 24 |
| readme-200kb (144 KB) | 317 | 235 | 39 |
| list-1mb (awesome-rust ×3) | 309 | 239 | 5 |
| table-1mb, code-1mb | 21 000, 35 000 | not run | 2 (one paragraph each) |

Run-to-run spread is wide (the floor alone ranged 175-235 ms over these sessions), so compare
medians of 10+ runs taken together, not across sessions.

Our added cost over the floor is 25-50 ms on normal documents. The 1 MB table and code fixtures
show the red-team blocker: to the spike each is one paragraph, and Pango lays out 1 MB in 20-35 s.
The flat leaf model (M1/M2) is the fix.

### Scroll, `--bench-scroll` at 3000 px/s, two runs each

| Fixture | Renderer | Frames | Dropped | p99 our snapshot | Layouts during scroll |
|---|---|---|---|---|---|
| readme-200kb | GL | ~1195 | 18, 20 | 1.8-1.9 ms | 879 |
| readme-200kb | Cairo no-GL | ~1157 | 50, 63 | 0.9 ms | 879 |
| list-1mb | GL | 3000 | 108, 153 | 3.2 ms | ~440 |
| list-1mb | Cairo no-GL | 3000 | 114, 117 | 2.3-2.5 ms | ~436 |

Both renderers drop frames while the spike creates Pango layouts on the main thread during the
scroll; M2 moves that off the scroll path. GSK render time is not in "our snapshot".

### Build

| | Clean release build | Binary | First frame (empty / ripgrep, Cairo, median 15) |
|---|---|---|---|
| fat LTO, 1 CGU | 1 m 23 s | 421 KB | 193 / 216 ms |
| no LTO, 16 CGU | 1 m 18 s | 483 KB | 198 / 218 ms |

No measurable startup difference; fat LTO kept for size.

### Decisions

- **Plain `gtk::init`** is the default (`--application` keeps the other path for measurement):
  20-45 ms faster to first frame.
- **Renderer: open.** Cairo with GL disabled is 60-100 ms faster to first frame; scroll is
  inconclusive until M2 removes layout work from the scroll path. Recorded as a decision for the
  user in docs/plan.md §11.
- **Fat LTO** kept.

## M0 review follow-up (2026-10-11)

Three independent reviews (correctness, performance, coherence) checked M0. Corrections to the
section above:

- **The phase table overstates what it shows.** "Presented" and "presented → first frame" are
  bimodal and move in opposite directions (~55/110 ms and ~70/20 ms): one ~50 ms wait lands on
  either side of `present()`. Only the total and `since_main` are reliable. That xfwm4's map
  latency dominates the floor is a belief: no control run (compositor off, or no window
  manager) has been done.
- **The startup tables used n=10, not interleaved, across sessions.** `scripts/bench-startup`
  is now a Python harness: configurations interleave run by run, the spawn time is taken just
  before spawning (the old `date` call added 5-7 ms), medians come with a bootstrap 95% CI on
  differences, and it refuses to run under load unless forced.
- **The empty window is the wrong floor.** It pays no text start-up (font loading, first Pango
  context); a one-line file costs ~15 ms more. `target/fixtures/one-line.md` is the floor now.
- **The scroll table measured partial scrolls and the wrong things.** The list-1mb rows stopped
  at the 3000-frame cap (now 6000, and a run that stops short exits 4 with
  `reached_end=false`). Dropped frames were judged against the median interval, not the
  refresh interval; "our snapshot" missed layout (done in `value-changed`) and GSK rendering.
  The probe now judges against the monitor's refresh interval and reports each frame's
  main-thread busy time, before-paint to after-paint.

### First-screen layout during the window map (`prepare`)

The view now lays out the first screen right after `present()`, at the default size, so the
work overlaps the window manager mapping the window instead of following it. Interleaved,
n=20, Cairo with GL disabled; taken under heavy memory pressure (swap full, load 3.8), so the
differences hold but the absolute numbers are not comparable with the section above:

| Fixture | `prepare` | `--no-prepare` | Difference (95% CI) |
|---|---|---|---|
| ripgrep README | 203.0 ms | 213.1 ms | −10.1 ms (−20.3 .. −0.9) |
| readme-200kb | 202.6 ms | 226.6 ms | −24.1 ms (−35.8 .. −10.9) |

`width_resets_before_first_frame` stayed 1 (the window manager gave the expected width), and
`loader_wait_us` is 0-1: the loader always finishes before the window is presented.

### Scroll, corrected probe (same memory-pressure conditions; qualitative only)

| Fixture | Renderer | Dropped (two runs) | p50 / p99 busy |
|---|---|---|---|
| control-words (3000 one-word paragraphs) | GL | 58, 73 | 12-13 / 23-25 ms |
| control-words | Cairo | 105, 119 | 11 / 40-45 ms |
| readme-200kb | GL | 100, 65 | 10-13 / 27-29 ms |
| readme-200kb | Cairo | 99, 91 | 11 / 38-39 ms |
| list-1mb | GL | 297, 334 | 12-13 / 29-32 ms |
| list-1mb | Cairo | 169, 21 | 11 / 20-37 ms |

Not usable for a decision: run-to-run spread is as large as the differences (list-1mb Cairo: 169
then 21), and "control-words" is not a zero-work control, since each one-word paragraph is laid
out as it scrolls in (2931 layouts). The ~11 ms median busy time even for the control suggests
before-paint → after-paint includes waiting (for example on the X server), not only our work;
to be checked before the metric is trusted. Renderer decision stays with M2 (plan §11).

## M1 (2026-10-11): `core::load` time

`cargo run --release -p gbk-markdown-core --example load_time`, in the build container, 15 runs:

| Input | Size | Leaves | Median |
|---|---|---|---|
| ripgrep README | 21 KiB | 148 | 0.6 ms |
| readme-200kb | 200 KiB | 1834 | 5.6 ms |
| awesome-rust | 390 KiB | 2355 | 6.4 ms |
| list-1mb | 1 MiB | 6132 | 26.6 ms |
| code-1mb | 1 MiB | 106 | 2.9 ms |
| table-1mb | 1 MiB | 237 | 75.2 ms |

`load` runs on the loader thread while GTK starts and the window maps (60-150 ms here), so these
stay off the critical path except possibly table-1mb. Where table-1mb's time goes (comrak's table
parsing or our lowering) is for M2 to find. The flat model removes M0's 20-35 s layout of the
1 MB table and code fixtures: both are now chunked into leaves of ≤ 50 rows / 200 lines.


### M1 review follow-up (2026-10-11): `load` time after the fixes

Same method; machine load 3.6 (noisy, compare within the table only):

| Input | Leaves | Median before | Median after |
|---|---|---|---|
| ripgrep README | 148 | 0.6 ms | 0.5 ms |
| readme-200kb | 1829 | 5.6 ms | 5.2 ms |
| awesome-rust | 2355 | 6.4 ms | 7.4 ms |
| list-1mb | 6132 | 26.6 ms | 19.9 ms |
| code-1mb | 106 | 2.9 ms | 1.8 ms |
| table-1mb | 237 | 75.2 ms | 82.5 ms (min 66.0) |

The walker borrows comrak's nodes instead of cloning them (the review measured 25-35% fewer
allocations). table-1mb is ~80% comrak's own table parsing (review profile: 57-60 ms of
72-88 ms); our share is ~13-15 ms. It runs on the loader thread during the window map; M2's
`loader_wait_us` shows whether it ever reaches the critical path.

Pathological inputs that took 0.5-19 s before (unclosed tags across blocks, stray closing
tags) are now linear: the HTML stack caches its state per entry, rejects unmatched closers in
O(1) and is capped at 256 deep. `crates/core/tests/scaling.rs` gates this (4× input must take
< 8× time; a planted quadratic bug measured 15-16×).

## M2 (2026-10-11): renderer v1

Real renderer: `LeafPaint`s built on the loader thread, first screen laid out in `prepare`,
heights from the worker, prefetch after each frame. Host load 4.5-4.8 (`--force`), so absolute
numbers are noisy; the GL/Cairo comparisons are interleaved and comparable.

### First frame, `bench-startup -n 20`, interleaved (ms)

| Fixture | GL median | Cairo median | Cairo vs GL (95% CI) | Laid out before first frame |
|---|---|---|---|---|
| one-line | 284.6 | 210.1 | -74.5 (-98.4 .. -60.6) | 1 leaf, 9 B |
| ripgrep README | 340.0 | 248.5 | -91.6 (-110.1 .. -70.3) | 29 leaves, 2.0 KiB |
| list-1mb | 322.7 | 233.3 | -89.5 (-110.5 .. -65.4) | 59 leaves, 559 B |
| table-1mb | 325.7 | 257.8 | -67.9 (-89.4 .. -35.2) | 2 leaves, 2.9 KiB |

`loader_wait_us` ≤ 6 everywhere: `load` + paint (even table-1mb's ~80 ms) finishes while GTK
maps the window. Our added cost over the M0 floor is within noise; time to first frame is GTK.

### Scroll, `--bench-scroll`, three interleaved runs each (dropped frames)

| Fixture | Frames | GL | Cairo |
|---|---|---|---|
| readme-200kb | ~1730 | 75, 48, 40 | 90, 79, 94 |
| list-1mb | ~2900 | 75, 64, 41 | 128, 173, 127 |
| code-1mb | ~2800 | 143, 170, 177 | 167, 108, 163 |
| table-1mb | ~2780 | 157, 177, 217 | 165, 174, 201 |

`layouts_in_scroll_frames=0` in every run: scroll frames only append cached render nodes.
GL drops fewer frames on prose and lists; code and table are even. An earlier non-interleaved
set (same day, same load) had Cairo ahead on scrolling, so that set was a load artefact.
The remaining drops (2-7%) are not layout; where they go (snapshot of many nodes, X server
waits) is open.

### Renderer trade-off (superseded by the review follow-up below)

Cairo with GL disabled: 70-90 ms faster first frame (goal 1). GL: fewer dropped frames on
prose and long lists (goal 2), even on code and tables. The performance review showed both
scroll results were dominated by GtkScrollbar's X round trips and slow large leaves, not by
the renderer.

## M2 review follow-up (2026-10-11)

Changes: own scroll indicator instead of GtkScrollbar, paints capped (2 KiB at line breaks or
unbreakable runs, 16 KiB prose), fonts warmed while waiting on the loader. Busy time now runs to
the end of the after-frame prefetch (it used to stop before it). Load 2-3.

### Scroll, `--bench-scroll`, three interleaved runs each

| Fixture | Frames | GL dropped | Cairo dropped | GL / Cairo p50 busy | before (GL / Cairo dropped) |
|---|---|---|---|---|---|
| readme-200kb | ~1807 | 1, 2, 1 | 1, 0, 8 | 5.7-7.2 / 5.7-8.3 ms | 40-75 / 79-94 |
| list-1mb | ~3000 | 0, 0, 0 | 1, 2, 1 | 6.6-7.0 / 5.7-5.8 ms | 41-75 / 127-173 |
| code-1mb | ~2995 | 0, 0, 0 | 7, 7, 7 | 6.0-8.5 / 8.7-8.8 ms | 143-177 / 108-167 |
| table-1mb | ~2982 | 15, 20, 18 | 13, 11, 18 | 5.4-6.3 / 6.0-6.1 ms | 157-217 / 165-201 |

`layouts_in_scroll_frames=0` in every run. Remaining drops are mostly table-1mb on both
renderers (its 2 KiB pieces are slow to rasterise: many short styled runs) and code-1mb on
Cairo. Next lever for both: per-leaf textures (plan §4.5 "Not yet").

### First frame of pathological inputs (host, one run each)

| Fixture | Before | After |
|---|---|---|
| huge-line (one 2 MB code line) | no frame in 120 s | 252 ms |
| huge-cells (50 rows, 40 KB cells) | 67.7 s | 247 ms |
| huge-heading (1 MB) | ~1.0 s | 236 ms |

All at the GTK floor; the smoke now runs them with a 5 s first-frame bound.

### Renderer trade-off, revised (for the user's decision, plan §11)

Scrolling no longer separates the renderers: GL drops 0-2 frames per run on prose, lists and
code, Cairo 0-8; both drop 11-20 on table-1mb. First frame still favours Cairo by 70-90 ms
(M2 table above). On goal order (first paint first), Cairo with GL disabled is favoured.

## M3 (2026-10-12): GFM completeness

Tables became grids (one layout per cell), links, `<details>`, HTML tables. Cairo (the
default). Load 1.3-2.0.

### First frame, `bench-startup -n 20` (ms)

| Fixture | Median | p90 | M2 Cairo median | Laid out before first frame |
|---|---|---|---|---|
| one-line | 223.1 | 238.0 | 210.1 | 1 layout, 9 B |
| ripgrep README | 248.4 | 259.3 | 248.5 | 100 layouts, 2.3 KiB |
| list-1mb | 229.0 | 251.0 | 233.3 | 59 layouts, 559 B |
| table-1mb | 254.1 | 266.0 | 257.8 | 258 layouts, 2.3 KiB |
| wide-table (100 columns) | 236.4 (n=10) | 249.9 | — | 355 layouts, 847 B |

Unchanged within noise: M3's extra work (column samples, more layouts per table screen,
link resolution on the loader thread) is hidden under GTK's floor. wide-table first made 1600
layouts (cells off the right edge); cells starting past it are no longer laid out. The smoke
now budgets layouts before the first frame at 1000.

### Scroll, `--bench-scroll`, three interleaved runs each

| Fixture | Frames | Dropped | p50 busy | M2 follow-up Cairo dropped |
|---|---|---|---|---|
| readme-200kb | 1840 | 1, 1, 1 | 6.5-6.8 ms | 1, 0, 8 |
| list-1mb | 3000 | 0, 0, 0 | 5.9-6.1 ms | 1, 2, 1 |
| code-1mb | ~2985 | 11, 12, 19 | 8.4 ms | 7, 7, 7 |
| table-1mb | ~2998 | 2, 1, 3 | 5.7-5.8 ms | 13, 11, 18 |
| wide-table | 596 | 0, 0, 0 | 6.1-6.4 ms | — |

`layouts_in_scroll_frames=0` in every run. table-1mb drops far fewer frames as grids: its
pieces are now at most 64 cells, cheaper to rasterise than M2's 2 KiB styled text runs.
code-1mb is unchanged code; its 11-19 against M2's 7 is within this load's noise (watch it;
per-leaf textures remain the lever, plan §4.5).
