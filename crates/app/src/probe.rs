//! Startup and work measurement (docs/plan.md §6).
//!
//! The probe counts layout work, and on the first frame after the document is drawn it can save
//! a screenshot, print a one-line report and exit. scripts/bench-startup and the gate's smoke run
//! read that report. With --bench-scroll it instead scrolls to the end of the document, one step
//! per frame, and reports frame intervals: xfwm4 sends no presentation times, so a dropped frame
//! shows only as a gap between successive frame times (docs/plan.md §6).

use std::cell::{Cell, RefCell};
use std::io::Write as _;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gtk::prelude::*;

use crate::args::ProbeArgs;
use crate::units;

/// Scroll speed for --bench-scroll, in logical pixels per second: a brisk fling.
const BENCH_SCROLL_SPEED: f64 = 3000.0;
/// --bench-scroll stops after this many frames even if the end is not reached.
const BENCH_SCROLL_MAX_FRAMES: usize = 3000;

#[derive(Debug)]
pub(crate) struct Probe {
    args: ProbeArgs,
    main_started: Instant,
    /// Whether the first frame must wait for the document view to draw.
    expect_document: bool,
    document_drawn: Cell<bool>,
    first_frame_done: Cell<bool>,
    layouts_created: Cell<usize>,
    laid_out_before_first_frame: Cell<usize>,
    /// Times the view dropped its layouts for a new width before the first frame.
    width_resets_before_first_frame: Cell<usize>,
    /// Leaves within the first viewport plus one screen, as the view saw them on its first draw.
    first_screen_leaves: Cell<usize>,
    leaves: Cell<usize>,
    /// When GTK finished starting, and when the window was presented, after `main` began.
    gtk_ready: Cell<Option<Duration>>,
    presented: Cell<Option<Duration>>,
    /// The vertical adjustment --bench-scroll drives.
    scroll_target: RefCell<Option<gtk::Adjustment>>,
    /// Time spent in the document view's snapshot since the last scroll frame.
    snapshot_time: Cell<Duration>,
    scroll: RefCell<ScrollRecord>,
}

#[derive(Debug, Default)]
struct ScrollRecord {
    last_frame_time: Option<i64>,
    layouts_at_start: usize,
    intervals_us: Vec<i64>,
    snapshot_us: Vec<u128>,
}

impl Probe {
    pub(crate) fn new(args: ProbeArgs, main_started: Instant, expect_document: bool) -> Rc<Self> {
        Rc::new(Self {
            args,
            main_started,
            expect_document,
            document_drawn: Cell::new(false),
            first_frame_done: Cell::new(false),
            layouts_created: Cell::new(0),
            laid_out_before_first_frame: Cell::new(0),
            width_resets_before_first_frame: Cell::new(0),
            first_screen_leaves: Cell::new(0),
            leaves: Cell::new(0),
            gtk_ready: Cell::new(None),
            presented: Cell::new(None),
            scroll_target: RefCell::new(None),
            snapshot_time: Cell::new(Duration::ZERO),
            scroll: RefCell::new(ScrollRecord::default()),
        })
    }

    pub(crate) fn gtk_ready(&self) {
        self.gtk_ready.set(Some(self.main_started.elapsed()));
    }

    pub(crate) fn presented(&self) {
        self.presented.set(Some(self.main_started.elapsed()));
    }

    pub(crate) fn set_scroll_target(&self, adjustment: gtk::Adjustment) {
        self.scroll_target.replace(Some(adjustment));
    }

    pub(crate) fn snapshot_took(&self, duration: Duration) {
        self.snapshot_time
            .set(self.snapshot_time.get().saturating_add(duration));
    }

    pub(crate) fn width_reset(&self) {
        if !self.first_frame_done.get() {
            let resets = self.width_resets_before_first_frame.get();
            self.width_resets_before_first_frame
                .set(resets.saturating_add(1));
        }
    }

    pub(crate) fn layout_created(&self) {
        self.layouts_created
            .set(self.layouts_created.get().saturating_add(1));
        if !self.first_frame_done.get() {
            let before = self.laid_out_before_first_frame.get();
            self.laid_out_before_first_frame
                .set(before.saturating_add(1));
        }
    }

    /// Called by the document view after each draw.
    pub(crate) fn document_drawn(&self, first_screen_leaves: usize, leaves: usize) {
        if !self.document_drawn.replace(true) {
            self.first_screen_leaves.set(first_screen_leaves);
            self.leaves.set(leaves);
        }
    }

    /// Watches the window's frame clock for the first frame painted after the document is drawn.
    pub(crate) fn attach(self: &Rc<Self>, window: &gtk::Window) {
        let probe = Rc::clone(self);
        window.connect_map(move |window| {
            let Some(clock) = window.frame_clock() else {
                return;
            };
            let probe = Rc::clone(&probe);
            let window = window.clone();
            clock.connect_after_paint(move |_clock| probe.after_paint(&window));
        });
    }

    fn after_paint(self: &Rc<Self>, window: &gtk::Window) {
        if self.first_frame_done.get() || (self.expect_document && !self.document_drawn.get()) {
            return;
        }
        self.first_frame_done.set(true);
        if self.args.bench_scroll {
            self.start_scroll(window);
            return;
        }
        if !self.args.exit_after_first_frame {
            return;
        }
        // Measure first: the screenshot and report are not part of startup.
        let report = self.report();
        let screenshot_ok = self
            .args
            .screenshot
            .as_ref()
            .is_none_or(|path| save_screenshot(window, path));
        let report_ok = writeln!(std::io::stdout().lock(), "{report}").is_ok();
        // Exit at once: teardown (unrealising GL, unloading drivers) is not part of startup.
        std::process::exit(if screenshot_ok && report_ok { 0 } else { 3 });
    }

    fn report(&self) -> String {
        let since_t0 = self.args.t0.map_or_else(
            || "unknown".to_owned(),
            |t0| match SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|now| now.checked_sub(t0))
            {
                Some(elapsed) => micros(elapsed),
                None => "clock-error".to_owned(),
            },
        );
        format!(
            "first_frame_us={since_t0} since_main_us={} gtk_ready_us={} presented_us={} layouts_created={} laid_out_before_first_frame={} width_resets_before_first_frame={} first_screen_leaves={} leaves={}",
            micros(self.main_started.elapsed()),
            self.gtk_ready
                .get()
                .map_or_else(|| "unknown".to_owned(), micros),
            self.presented
                .get()
                .map_or_else(|| "unknown".to_owned(), micros),
            self.layouts_created.get(),
            self.laid_out_before_first_frame.get(),
            self.width_resets_before_first_frame.get(),
            self.first_screen_leaves.get(),
            self.leaves.get(),
        )
    }
}

impl Probe {
    fn start_scroll(self: &Rc<Self>, window: &gtk::Window) {
        if self.scroll_target.borrow().is_none() {
            crate::report_error("--bench-scroll: there is no document to scroll");
            std::process::exit(3);
        }
        self.scroll.borrow_mut().layouts_at_start = self.layouts_created.get();
        let probe = Rc::clone(self);
        window.add_tick_callback(move |_window, clock| probe.scroll_tick(clock));
    }

    fn scroll_tick(&self, clock: &gtk::gdk::FrameClock) -> gtk::glib::ControlFlow {
        let Some(adjustment) = self.scroll_target.borrow().clone() else {
            return gtk::glib::ControlFlow::Break;
        };
        let now = clock.frame_time();
        let snapshot = self.snapshot_time.replace(Duration::ZERO);
        let frames = {
            let mut record = self.scroll.borrow_mut();
            if let Some(last) = record.last_frame_time.replace(now) {
                let interval = now.saturating_sub(last);
                record.intervals_us.push(interval);
                record.snapshot_us.push(snapshot.as_micros());
                let step = BENCH_SCROLL_SPEED * units::micros_to_seconds(interval);
                adjustment.set_value(adjustment.value() + step);
            }
            record.intervals_us.len()
        };
        let at_end = adjustment.value() >= adjustment.upper() - adjustment.page_size();
        if at_end || frames >= BENCH_SCROLL_MAX_FRAMES {
            let report = self.scroll_report();
            let ok = writeln!(std::io::stdout().lock(), "{report}").is_ok();
            std::process::exit(if ok { 0 } else { 3 });
        }
        gtk::glib::ControlFlow::Continue
    }

    fn scroll_report(&self) -> String {
        let record = self.scroll.borrow();
        let mut intervals = record.intervals_us.clone();
        intervals.sort_unstable();
        let mut snapshots = record.snapshot_us.clone();
        snapshots.sort_unstable();
        let median = percentile(&intervals, 50).unwrap_or(0);
        // A frame interval over 1.5 times the typical one means at least one frame was missed.
        let dropped = intervals
            .iter()
            .filter(|&&interval| interval.saturating_mul(2) > median.saturating_mul(3))
            .count();
        format!(
            "scroll_frames={} median_interval_us={median} p99_interval_us={} max_interval_us={} dropped={dropped} p50_snapshot_us={} p99_snapshot_us={} max_snapshot_us={} layouts_during_scroll={}",
            intervals.len(),
            percentile(&intervals, 99).unwrap_or(0),
            intervals.last().copied().unwrap_or(0),
            percentile(&snapshots, 50).unwrap_or(0),
            percentile(&snapshots, 99).unwrap_or(0),
            snapshots.last().copied().unwrap_or(0),
            self.layouts_created
                .get()
                .saturating_sub(record.layouts_at_start),
        )
    }
}

/// The value at `percent` in sorted `values`, nearest-rank.
fn percentile<T: Copy>(sorted: &[T], percent: usize) -> Option<T> {
    let rank = (sorted.len().saturating_mul(percent)).div_ceil(100);
    sorted.get(rank.saturating_sub(1)).copied()
}

fn micros(duration: Duration) -> String {
    duration.as_micros().to_string()
}

/// Renders the window to a PNG. Returns false, with a message, if any step fails.
fn save_screenshot(window: &gtk::Window, path: &PathBuf) -> bool {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        &snapshot,
        f64::from(window.width()),
        f64::from(window.height()),
    );
    let Some(node) = snapshot.to_node() else {
        crate::report_error("screenshot: the window drew nothing");
        return false;
    };
    let Some(native) = window.native() else {
        crate::report_error("screenshot: the window has no native surface");
        return false;
    };
    let Some(renderer) = native.renderer() else {
        crate::report_error("screenshot: the window has no renderer");
        return false;
    };
    let texture = renderer.render_texture(node, None);
    if texture.save_to_png(path).is_err() {
        crate::report_error("screenshot: cannot write the PNG");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_nearest_rank() {
        let values = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        assert_eq!(percentile(&values, 50), Some(50));
        assert_eq!(percentile(&values, 90), Some(90));
        assert_eq!(percentile(&values, 99), Some(100));
        assert_eq!(percentile(&values, 100), Some(100));
        assert_eq!(percentile(&values, 1), Some(10));
        assert_eq!(percentile(&[7], 50), Some(7));
        assert_eq!(percentile::<i64>(&[], 50), None);
    }
}
