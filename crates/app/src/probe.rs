//! Startup and work measurement (docs/plan.md §6).
//!
//! The probe counts layout work, and on the first frame after the document is drawn it can save
//! a screenshot, print a one-line report and exit. scripts/bench-startup and the gate's smoke run
//! read that report.
//!
//! With --bench-scroll it instead scrolls to the end of the document, one step per frame, and
//! reports frame intervals and each frame's main-thread busy time (before-paint to the end of
//! the view's after-frame prefetch, which covers tick, layout, our snapshot, GSK rendering and
//! prefetch). xfwm4 sends no presentation
//! times, so a dropped frame shows only as a gap between successive frame times; frames the
//! compositor or GPU miss are not visible here.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::args::ProbeArgs;
use crate::output::{print_line, report_error};
use crate::units;

/// Scroll speed for --bench-scroll, in logical pixels per second: a brisk fling (faster for a
/// document too long to cross at this speed within half the frame limit).
const BENCH_SCROLL_SPEED: f64 = 3000.0;
/// --bench-scroll gives up after this many frames; reaching it is a failed run.
const BENCH_SCROLL_MAX_FRAMES: usize = 6000;
/// Exit status when a measurement run cannot produce its report.
const EXIT_NO_REPORT: i32 = 3;
/// Exit status when --bench-scroll stops before the end of the document.
const EXIT_SCROLL_INCOMPLETE: i32 = 4;

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
    /// Bytes of text given to Pango before the first frame: the work, not the leaf count.
    bytes_laid_out_before_first_frame: Cell<usize>,
    /// Times the view dropped its layouts for a new width before the first frame.
    width_resets_before_first_frame: Cell<usize>,
    leaves: Cell<usize>,
    /// When GTK finished starting and when the window was presented, after `main` began.
    gtk_ready: Cell<Option<Duration>>,
    presented: Cell<Option<Duration>>,
    /// How long the main thread waited for the loader thread.
    loader_wait: Cell<Option<Duration>>,
    /// Whether a frame is being painted (before-paint to after-paint).
    in_frame: Cell<bool>,
    /// Layouts made inside frames since --bench-scroll began scrolling: steady scrolling must
    /// find every leaf already laid out (docs/plan.md §6, gated).
    layouts_in_scroll_frames: Cell<usize>,
    /// When the height worker finished, after `main` began.
    heights_measured: Cell<Option<Duration>>,
    /// The vertical adjustment --bench-scroll drives.
    scroll_target: RefCell<Option<gtk::Adjustment>>,
    scroll: RefCell<ScrollRecord>,
    /// For --link-at and --click-at: the document view, once there is one.
    view: RefCell<Option<ViewProbe>>,
    /// For --click-at and --wait-images: the report so far, finished and printed after a later
    /// frame.
    deferred: RefCell<Option<String>>,
}

/// What the probe asks of the document view.
pub(crate) struct ViewProbe {
    /// Describes the link at a point.
    pub(crate) link_at: Box<dyn Fn(f64, f64) -> String>,
    /// Clicks at a point; describes what the click did.
    pub(crate) click_at: Box<dyn Fn(f64, f64) -> String>,
    /// The document's height and the paint at the top of the view.
    pub(crate) state: Box<dyn Fn() -> (i64, usize)>,
    /// Images shown, failed, and still loading.
    pub(crate) images: Box<dyn Fn() -> (usize, usize, usize)>,
}

impl std::fmt::Debug for ViewProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ViewProbe")
    }
}

#[derive(Debug, Default)]
struct ScrollRecord {
    /// The monitor's refresh interval, if GDK knows it.
    refresh_us: Option<i64>,
    last_frame_time: Option<i64>,
    paint_started: Option<Instant>,
    layouts_at_start: usize,
    intervals_us: Vec<i64>,
    busy_us: Vec<u128>,
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
            bytes_laid_out_before_first_frame: Cell::new(0),
            width_resets_before_first_frame: Cell::new(0),
            leaves: Cell::new(0),
            gtk_ready: Cell::new(None),
            presented: Cell::new(None),
            loader_wait: Cell::new(None),
            in_frame: Cell::new(false),
            layouts_in_scroll_frames: Cell::new(0),
            heights_measured: Cell::new(None),
            scroll_target: RefCell::new(None),
            scroll: RefCell::new(ScrollRecord::default()),
            view: RefCell::new(None),
            deferred: RefCell::new(None),
        })
    }

    /// Whether to lay out the first screen before allocation (`--no-prepare` turns it off, to
    /// measure the difference).
    pub(crate) fn prepare_enabled(&self) -> bool {
        !self.args.no_prepare
    }

    pub(crate) fn gtk_ready(&self) {
        self.gtk_ready.set(Some(self.main_started.elapsed()));
    }

    pub(crate) fn presented(&self) {
        self.presented.set(Some(self.main_started.elapsed()));
    }

    pub(crate) fn loader_joined(&self, waited: Duration) {
        self.loader_wait.set(Some(waited));
    }

    pub(crate) fn set_view(&self, view: ViewProbe) {
        self.view.replace(Some(view));
    }

    pub(crate) fn set_scroll_target(&self, adjustment: gtk::Adjustment) {
        self.scroll_target.replace(Some(adjustment));
    }

    pub(crate) fn width_reset(&self) {
        if !self.first_frame_done.get() {
            increment(&self.width_resets_before_first_frame, 1);
        }
    }

    pub(crate) fn heights_measured(&self) {
        self.heights_measured.set(Some(self.main_started.elapsed()));
    }

    pub(crate) fn checking_heights(&self) -> bool {
        self.args.check_heights
    }

    /// For --check-heights: reports how many leaf heights the worker got wrong, and exits.
    pub(crate) fn heights_checked(&self, mismatches: usize, leaves: usize) {
        let report = format!(
            "height_mismatches={mismatches} leaves={leaves} heights_measured_us={}",
            optional_micros(self.heights_measured.get())
        );
        std::process::exit(if print_line(report) {
            0
        } else {
            EXIT_NO_REPORT
        });
    }

    /// Counts one Pango layout of `bytes` bytes of text.
    pub(crate) fn layout_created(&self, bytes: usize) {
        increment(&self.layouts_created, 1);
        if self.in_frame.get() && self.scroll.borrow().last_frame_time.is_some() {
            increment(&self.layouts_in_scroll_frames, 1);
        }
        if !self.first_frame_done.get() {
            increment(&self.laid_out_before_first_frame, 1);
            increment(&self.bytes_laid_out_before_first_frame, bytes);
        }
    }

    /// Called by the document view when its work after a frame (prefetch) is done: a scroll
    /// frame's busy time runs from before-paint to here, so it includes that work.
    pub(crate) fn frame_work_done(&self) {
        if self.first_frame_done.get() && self.args.bench_scroll {
            let mut record = self.scroll.borrow_mut();
            if let Some(started) = record.paint_started.take() {
                record.busy_us.push(started.elapsed().as_micros());
            }
        }
    }

    /// Called by the document view after each draw.
    pub(crate) fn document_drawn(&self, leaves: usize) {
        if !self.document_drawn.replace(true) {
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
            let after = Rc::clone(&probe);
            let window = window.clone();
            clock.connect_after_paint(move |_clock| after.after_paint(&window));
            let before = Rc::clone(&probe);
            clock.connect_before_paint(move |_clock| before.before_paint());
        });
    }

    fn before_paint(&self) {
        self.in_frame.set(true);
        if self.first_frame_done.get() && self.args.bench_scroll {
            self.scroll.borrow_mut().paint_started = Some(Instant::now());
        }
    }

    fn after_paint(self: &Rc<Self>, window: &gtk::Window) {
        self.in_frame.set(false);
        if self.first_frame_done.get() {
            // A frame after a --click-at click, or once --wait-images has nothing loading:
            // report what it shows.
            let view = self.view.borrow();
            let images = view
                .as_ref()
                .map(|view| (view.images)())
                .unwrap_or_default();
            if self.args.wait_images && images.2 > 0 {
                return;
            }
            let Some(mut report) = self.deferred.take() else {
                return;
            };
            if self.args.click_at.is_some() {
                let (height, top) = view.as_ref().map(|view| (view.state)()).unwrap_or_default();
                report = format!("{report} document_height={height} top_leaf={top}");
            }
            if self.args.wait_images {
                let (loaded, failed, loading) = images;
                report = format!(
                    "{report} images_loaded={loaded} images_failed={failed} images_loading={loading}"
                );
            }
            drop(view);
            self.finish(window, report);
            return;
        }
        if self.expect_document && !self.document_drawn.get() {
            return;
        }
        self.first_frame_done.set(true);
        if self.args.bench_scroll {
            self.start_scroll(window);
            return;
        }
        if !self.args.exit_after_first_frame || self.args.check_heights {
            return;
        }
        // Measure first: the screenshot and report are not part of startup.
        let report = self.startup_report(window);
        let mut report = report;
        if let Some((x, y)) = self.args.click_at {
            let clicked = self.view.borrow().as_ref().map_or_else(
                || "no-view".to_owned(),
                |view| (view.click_at)(f64::from(x), f64::from(y)),
            );
            report = format!("{report} clicked={clicked}");
        }
        if self.args.click_at.is_some() || self.args.wait_images {
            self.deferred.replace(Some(report));
            // A click that changes nothing, or no images, would otherwise leave no next frame.
            window.queue_draw();
            return;
        }
        self.finish(window, report);
    }

    /// Saves the screenshot, prints the report and exits.
    fn finish(&self, window: &gtk::Window, report: String) {
        let screenshot_ok = self
            .args
            .screenshot
            .as_deref()
            .is_none_or(|path| save_screenshot(window, path));
        let report_ok = print_line(report);
        // Exit at once: teardown (unrealising GL, unloading drivers) is not part of startup.
        std::process::exit(if screenshot_ok && report_ok {
            0
        } else {
            EXIT_NO_REPORT
        });
    }

    fn startup_report(&self, window: &gtk::Window) -> String {
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
        let link = self.args.link_at.map_or_else(String::new, |(x, y)| {
            let label = self.view.borrow().as_ref().map_or_else(
                || "no-view".to_owned(),
                |view| (view.link_at)(f64::from(x), f64::from(y)),
            );
            format!(" link_at={label}")
        });
        format!(
            "first_frame_us={since_t0} since_main_us={} gtk_ready_us={} presented_us={} \
             loader_wait_us={} layouts_created={} laid_out_before_first_frame={} \
             bytes_laid_out_before_first_frame={} width_resets_before_first_frame={} leaves={} \
             renderer={}{link}",
            micros(self.main_started.elapsed()),
            optional_micros(self.gtk_ready.get()),
            optional_micros(self.presented.get()),
            optional_micros(self.loader_wait.get()),
            self.layouts_created.get(),
            self.laid_out_before_first_frame.get(),
            self.bytes_laid_out_before_first_frame.get(),
            self.width_resets_before_first_frame.get(),
            self.leaves.get(),
            renderer_name(window),
        )
    }

    fn start_scroll(self: &Rc<Self>, window: &gtk::Window) {
        if self.scroll_target.borrow().is_none() {
            report_error("--bench-scroll: there is no document to scroll");
            std::process::exit(EXIT_NO_REPORT);
        }
        {
            let mut record = self.scroll.borrow_mut();
            record.layouts_at_start = self.layouts_created.get();
            record.refresh_us = refresh_interval_us(window);
        }
        let probe = Rc::clone(self);
        window.add_tick_callback(move |_window, clock| probe.scroll_tick(clock));
    }

    fn scroll_tick(&self, clock: &gdk::FrameClock) -> glib::ControlFlow {
        let Some(adjustment) = self.scroll_target.borrow().clone() else {
            return glib::ControlFlow::Break;
        };
        let now = clock.frame_time();
        let (last, frames) = {
            let mut record = self.scroll.borrow_mut();
            let last = record.last_frame_time.replace(now);
            if let Some(last) = last {
                record.intervals_us.push(now.saturating_sub(last));
            }
            (last, record.intervals_us.len())
        };
        // Set the value with no borrow held: value-changed runs the view's layout code.
        if let Some(last) = last {
            // Fast enough to cross any document within the frame limit, and no slower than a
            // brisk fling.
            let span = adjustment.upper() - adjustment.page_size();
            let frames = units::count_to_f64(BENCH_SCROLL_MAX_FRAMES / 2);
            let speed = BENCH_SCROLL_SPEED.max(span / frames * 60.0);
            let step = speed * units::micros_to_seconds(now.saturating_sub(last));
            adjustment.set_value(adjustment.value() + step);
        }
        let reached_end = adjustment.value() >= adjustment.upper() - adjustment.page_size();
        if reached_end || frames >= BENCH_SCROLL_MAX_FRAMES {
            let report_ok = self.scroll_report(reached_end).is_some_and(print_line);
            let code = match (report_ok, reached_end) {
                (false, _) => EXIT_NO_REPORT,
                (true, false) => EXIT_SCROLL_INCOMPLETE,
                (true, true) => 0,
            };
            std::process::exit(code);
        }
        glib::ControlFlow::Continue
    }

    /// The scroll report, or None (with a message) if there were no frames to report on.
    fn scroll_report(&self, reached_end: bool) -> Option<String> {
        let record = self.scroll.borrow();
        let mut intervals = record.intervals_us.clone();
        intervals.sort_unstable();
        let mut busy = record.busy_us.clone();
        busy.sort_unstable();
        let (Some(&shortest), Some(&longest), Some(&busiest)) =
            (intervals.first(), intervals.last(), busy.last())
        else {
            report_error("--bench-scroll: no frames were recorded");
            return None;
        };
        // Plan §6: a gap over 1.5 refresh intervals means at least one frame was missed. Without
        // a known refresh rate the shortest interval stands in for it.
        let refresh = record.refresh_us.unwrap_or(shortest);
        let dropped = intervals
            .iter()
            .filter(|&&interval| interval.saturating_mul(2) > refresh.saturating_mul(3))
            .count();
        Some(format!(
            "scroll_frames={} reached_end={reached_end} refresh_us={refresh} refresh_known={} \
             median_interval_us={} p99_interval_us={} max_interval_us={longest} dropped={dropped} \
             p50_busy_us={} p99_busy_us={} max_busy_us={busiest} layouts_during_scroll={} \
             layouts_in_scroll_frames={}",
            intervals.len(),
            record.refresh_us.is_some(),
            percentile(&intervals, 50)?,
            percentile(&intervals, 99)?,
            percentile(&busy, 50)?,
            percentile(&busy, 99)?,
            self.layouts_created
                .get()
                .saturating_sub(record.layouts_at_start),
            self.layouts_in_scroll_frames.get(),
        ))
    }
}

fn increment(counter: &Cell<usize>, by: usize) {
    counter.set(counter.get().saturating_add(by));
}

/// The refresh interval of the window's monitor, in microseconds, if GDK knows the rate.
fn refresh_interval_us(window: &gtk::Window) -> Option<i64> {
    let surface = window.surface()?;
    let monitor = surface.display().monitor_at_surface(&surface)?;
    let millihertz = i64::from(monitor.refresh_rate());
    (millihertz > 0).then(|| 1_000_000_000 / millihertz)
}

/// The value at `percent` in sorted `values`, nearest-rank.
fn percentile<T: Copy>(sorted: &[T], percent: usize) -> Option<T> {
    let rank = (sorted.len().saturating_mul(percent)).div_ceil(100);
    sorted.get(rank.saturating_sub(1)).copied()
}

fn micros(duration: Duration) -> String {
    duration.as_micros().to_string()
}

fn optional_micros(duration: Option<Duration>) -> String {
    duration.map_or_else(|| "none".to_owned(), micros)
}

/// Renders the window to a PNG. Returns false, with a message, if any step fails.
fn save_screenshot(window: &gtk::Window, path: &Path) -> bool {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        &snapshot,
        f64::from(window.width()),
        f64::from(window.height()),
    );
    let Some(node) = snapshot.to_node() else {
        report_error("screenshot: the window drew nothing");
        return false;
    };
    let Some(native) = window.native() else {
        report_error("screenshot: the window has no native surface");
        return false;
    };
    let Some(renderer) = native.renderer() else {
        report_error("screenshot: the window has no renderer");
        return false;
    };
    let texture = renderer.render_texture(node, None);
    if texture.save_to_png(path).is_err() {
        report_error("screenshot: cannot write the PNG");
        return false;
    }
    true
}

/// The renderer GTK actually uses for the window (it falls back to Cairo if GL fails), as its
/// type name: `GskCairoRenderer`, `GskGLRenderer`.
fn renderer_name(window: &gtk::Window) -> String {
    window.renderer().map_or_else(
        || "none".to_owned(),
        |renderer| renderer.type_().name().to_owned(),
    )
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
