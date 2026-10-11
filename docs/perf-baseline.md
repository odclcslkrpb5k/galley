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
