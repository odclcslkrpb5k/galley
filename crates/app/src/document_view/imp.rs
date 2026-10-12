use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk, pango};

use crate::output::report_error;
use crate::probe::Probe;
use crate::render::geometry::{LeafGeometry, Rect, Shade};
use crate::render::heights::{Job, Worker, WorkerGone};
use crate::render::indicator::{self, Scroll};
use crate::render::paint::{BarColor, LeafPaint};
use crate::render::positions::Positions;
use crate::render::text::{Palette, TextSetup, Typesetter};
use crate::units;

/// Space around the document, in em.
const MARGIN_EM: f64 = 2.0;
/// Leaves are laid out ahead of the viewport this many screens up and down, between frames.
const PREFETCH_SCREENS: i64 = 2;
/// Cached leaves further than this many screens from the viewport are dropped.
const KEEP_SCREENS: i64 = 3;
/// Most time one idle prefetch slice may take, so it never delays a frame by much.
const IDLE_BUDGET: Duration = Duration::from_millis(4);
/// Prefetch time taken right after each painted frame. With GL the main loop may get almost no
/// idle time while scrolling (frames wait on the GPU), so prefetch gets this slice regardless.
const AFTER_FRAME_BUDGET: Duration = Duration::from_millis(3);
/// Scrolling counts as stopped this long after the last movement.
const MOTION_SETTLE: Duration = Duration::from_millis(150);
/// How often the height worker's results are collected while it runs.
const POLL_INTERVAL: Duration = Duration::from_millis(16);
/// The `gtk::Settings` properties that change how text is laid out.
const TEXT_SETTINGS: [&str; 7] = [
    "gtk-font-name",
    "gtk-xft-dpi",
    "gtk-font-rendering",
    "gtk-xft-antialias",
    "gtk-xft-hinting",
    "gtk-xft-hintstyle",
    "gtk-hint-font-metrics",
];

#[derive(Debug, glib::Properties)]
#[properties(wrapper_type = super::DocumentView)]
pub(crate) struct DocumentView {
    #[property(override_interface = gtk::Scrollable, get, set = Self::set_hadjustment)]
    hadjustment: RefCell<Option<gtk::Adjustment>>,
    #[property(override_interface = gtk::Scrollable, get, set = Self::set_vadjustment)]
    vadjustment: RefCell<Option<gtk::Adjustment>>,
    #[property(override_interface = gtk::Scrollable, get, set, builder(gtk::ScrollablePolicy::Minimum))]
    hscroll_policy: Cell<gtk::ScrollablePolicy>,
    #[property(override_interface = gtk::Scrollable, get, set, builder(gtk::ScrollablePolicy::Minimum))]
    vscroll_policy: Cell<gtk::ScrollablePolicy>,
    vadjustment_handler: RefCell<Option<glib::SignalHandlerId>>,
    document: RefCell<Option<Doc>>,
    /// When the user last scrolled: height changes above the viewport wait until it settles.
    last_scroll: Cell<Option<Instant>>,
    /// Set while the view moves the scroll position itself, which is not the user scrolling.
    adjusting: Cell<bool>,
    poll: RefCell<Option<glib::SourceId>>,
    prefetch: RefCell<Option<glib::SourceId>>,
    settle: RefCell<Option<glib::SourceId>>,
    reconfigure: RefCell<Option<glib::SourceId>>,
    /// The allocated size, to configure the adjustment and re-wrap outside of an allocation.
    page: Cell<f64>,
    widget_width: Cell<i64>,
    /// The after-paint handler that prefetches after each frame, while mapped.
    after_paint: RefCell<Option<(gtk::gdk::FrameClock, glib::SignalHandlerId)>>,
    /// The scroll indicator: whether the pointer is over it, and the scroll value a drag on it
    /// started from.
    indicator_hover: Cell<bool>,
    indicator_drag: Cell<Option<f64>>,
}

// By hand: gtk::ScrollablePolicy has no Default.
impl Default for DocumentView {
    fn default() -> Self {
        Self {
            hadjustment: RefCell::default(),
            vadjustment: RefCell::default(),
            hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
            vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
            vadjustment_handler: RefCell::default(),
            document: RefCell::default(),
            last_scroll: Cell::new(None),
            adjusting: Cell::new(false),
            poll: RefCell::default(),
            prefetch: RefCell::default(),
            settle: RefCell::default(),
            reconfigure: RefCell::default(),
            page: Cell::new(0.0),
            widget_width: Cell::new(0),
            after_paint: RefCell::default(),
            indicator_hover: Cell::new(false),
            indicator_drag: Cell::new(None),
        }
    }
}

#[derive(Debug)]
struct Doc {
    paints: Arc<Vec<LeafPaint>>,
    positions: Positions,
    /// The width text is wrapped to, without margins; 0 until allocated.
    width: i64,
    generation: u64,
    setup: TextSetup,
    palette: Palette,
    /// Drawn leaves (`None`: nothing to draw), near the viewport.
    cache: HashMap<usize, Option<gsk::RenderNode>>,
    /// `None` once the worker has stopped.
    worker: Option<Worker>,
    /// For `--check-heights` only: every height the worker sent.
    worker_heights: Vec<Option<i64>>,
    probe: Rc<Probe>,
}

#[glib::object_subclass]
impl ObjectSubclass for DocumentView {
    const NAME: &'static str = "GbkDocumentView";
    type Type = super::DocumentView;
    type ParentType = gtk::Widget;
    type Interfaces = (gtk::Scrollable,);
}

#[glib::derived_properties]
impl ObjectImpl for DocumentView {
    fn constructed(&self) {
        self.parent_constructed();
        let widget = self.obj();
        let drag = gtk::GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = view)]
            widget,
            move |gesture, x, y| view.imp().indicator_drag_begin(gesture, x, y)
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = view)]
            widget,
            move |_gesture, _dx, dy| view.imp().indicator_drag_update(dy)
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = view)]
            widget,
            move |_gesture, _dx, _dy| {
                view.imp().indicator_drag.set(None);
                view.queue_draw();
            }
        ));
        widget.add_controller(drag);
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = view)]
            widget,
            move |_controller, x, _y| view.imp().set_indicator_hover(view.imp().on_indicator(x))
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            widget,
            move |_controller| view.imp().set_indicator_hover(false)
        ));
        widget.add_controller(motion);
        let settings = widget.settings();
        for name in TEXT_SETTINGS {
            settings.connect_notify_local(
                Some(name),
                glib::clone!(
                    #[weak(rename_to = view)]
                    widget,
                    // After GTK's own handlers have updated the widget's Pango context.
                    move |_settings, _property| {
                        glib::idle_add_local_once(glib::clone!(
                            #[weak]
                            view,
                            move || view.imp().refresh_setup()
                        ));
                    }
                ),
            );
        }
    }
}

impl WidgetImpl for DocumentView {
    fn map(&self) {
        self.parent_map();
        if let Some(clock) = self.obj().frame_clock() {
            let handler = clock.connect_after_paint(glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                move |_clock| view.imp().after_frame()
            ));
            self.after_paint.replace(Some((clock, handler)));
        }
    }

    fn unmap(&self) {
        if let Some((clock, handler)) = self.after_paint.take() {
            clock.disconnect(handler);
        }
        self.parent_unmap();
    }

    fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
        (0, 0, -1, -1)
    }

    fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
        self.page.set(f64::from(height));
        self.widget_width.set(i64::from(width));
        let value = self.set_width(i64::from(width));
        self.configure_vadjustment(value);
    }

    fn snapshot(&self, snapshot: &gtk::Snapshot) {
        let Some((top, page)) = self.viewport() else {
            return;
        };
        let widget = self.obj();
        let context = widget.pango_context();
        let mut document = self.document.borrow_mut();
        let Some(doc) = document.as_mut() else {
            return;
        };
        doc.sync_palette(widget.color());
        let margin = doc.margin();
        let anchor = doc.positions.anchor(top - margin);
        let total = doc.positions.total();
        // Leaves that scrolling up brought into view take their held heights; then lay out
        // what is visible (normally already cached) and draw it where those heights put it.
        doc.positions.release_from(anchor);
        for leaf in doc.positions.visible(top - margin, page) {
            doc.ensure(&context, leaf, anchor, false);
        }
        for leaf in doc.positions.visible(top - margin, page) {
            let Some(Some(node)) = doc.cache.get(&leaf) else {
                continue;
            };
            let y = margin + doc.positions.start(leaf) - top;
            snapshot.save();
            snapshot.translate(&point(margin, y));
            snapshot.append_node(node);
            snapshot.restore();
        }
        let resized = doc.positions.total() != total;
        let indicator_color = doc
            .palette
            .faint(if self.indicator_active() { 0.5 } else { 0.3 });
        doc.probe.document_drawn(doc.paints.len());
        drop(document);
        self.draw_indicator(snapshot, indicator_color);
        if resized {
            self.schedule_reconfigure();
        }
        self.schedule_prefetch();
    }
}

impl ScrollableImpl for DocumentView {}

impl DocumentView {
    pub(super) fn set_document(&self, paints: Vec<LeafPaint>, probe: Rc<Probe>) {
        let widget = self.obj();
        let setup = TextSetup::of(&*widget);
        let count = paints.len();
        let worker = Worker::spawn();
        *self.document.borrow_mut() = Some(Doc {
            paints: Arc::new(paints),
            positions: Positions::new(vec![0; count]),
            width: 0,
            generation: 0,
            setup,
            palette: Palette::for_foreground(widget.color()),
            cache: HashMap::new(),
            worker: Some(worker),
            worker_heights: Vec::new(),
            probe,
        });
        widget.queue_allocate();
    }

    /// Lays out the first screen for an expected widget size, before GTK allocates the real one.
    ///
    /// Called right after the window is presented: the layout work then overlaps the window
    /// manager mapping the window instead of following it. If the real width differs, the
    /// width-reset path lays out again (the probe counts that).
    pub(super) fn prepare(&self, width: f64, height: f64) {
        self.page.set(height);
        self.set_width(units::round_px(width));
        let widget = self.obj();
        let context = widget.pango_context();
        if let Some(doc) = self.document.borrow_mut().as_mut() {
            doc.sync_palette(widget.color());
            let margin = doc.margin();
            for leaf in doc.positions.visible(-margin, units::round_px(height)) {
                doc.ensure(&context, leaf, None, false);
            }
        }
    }

    fn viewport(&self) -> Option<(i64, i64)> {
        let adjustment = self.vadjustment.borrow().clone()?;
        Some((
            units::round_px(adjustment.value()),
            units::round_px(adjustment.page_size()),
        ))
    }

    /// Re-estimates every height and restarts the worker when the text width changes. Returns
    /// the scroll value that keeps the same content at the top of the viewport.
    fn set_width(&self, widget_width: i64) -> Option<f64> {
        let value = self
            .vadjustment
            .borrow()
            .as_ref()
            .map(gtk::Adjustment::value);
        let mut document = self.document.borrow_mut();
        let doc = document.as_mut()?;
        let margin = doc.margin();
        let width = (widget_width - 2 * margin).max(1);
        if width == doc.width {
            return None;
        }
        let spot = value.and_then(|value| doc.positions.spot(units::round_px(value) - margin));
        doc.probe.width_reset();
        doc.width = width;
        doc.generation += 1;
        doc.cache.clear();
        let metrics = doc.setup.metrics();
        doc.positions = Positions::new(
            doc.paints
                .iter()
                .map(|paint| metrics.estimate(paint, width))
                .collect(),
        );
        if doc.probe.checking_heights() {
            doc.worker_heights = vec![None; doc.paints.len()];
        }
        let submitted = doc.worker.as_ref().map(|worker| {
            worker.submit(Job {
                generation: doc.generation,
                view_text_width: width,
                paints: Arc::clone(&doc.paints),
                setup: doc.setup,
            })
        });
        if let Some(Err(gone)) = submitted {
            worker_gone(doc, gone);
        }
        let value = spot.map(|spot| units::px_to_f64(doc.positions.y_of(spot) + margin));
        let polling = doc.worker.is_some();
        drop(document);
        if polling {
            self.start_polling();
        }
        value
    }

    /// Takes a new text setup (font, DPI, hinting) and lays everything out again.
    fn refresh_setup(&self) {
        let setup = TextSetup::of(&*self.obj());
        {
            let mut document = self.document.borrow_mut();
            let Some(doc) = document.as_mut() else {
                return;
            };
            if doc.setup == setup {
                return;
            }
            doc.setup = setup;
            // Forces `set_width` to start over even if the width is the same.
            doc.width = 0;
        }
        let width = self.widget_width.get();
        if width > 0 {
            let value = self.set_width(width);
            self.configure_vadjustment(value);
            self.obj().queue_draw();
        }
    }

    fn start_polling(&self) {
        if self.poll.borrow().is_some() {
            return;
        }
        let source = glib::timeout_add_local(
            POLL_INTERVAL,
            glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || view.imp().collect_heights()
            ),
        );
        self.poll.replace(Some(source));
    }

    /// Applies the worker's heights; ends polling when its job is done.
    fn collect_heights(&self) -> glib::ControlFlow {
        let top = self.viewport().map(|(top, _page)| top);
        let scrolling = self.in_motion();
        let (done, shift) = {
            let mut document = self.document.borrow_mut();
            let Some(doc) = document.as_mut() else {
                self.poll.replace(None);
                return glib::ControlFlow::Break;
            };
            let ready = match doc.worker.as_ref().map(Worker::ready) {
                Some(Ok(ready)) => ready,
                Some(Err(gone)) => {
                    worker_gone(doc, gone);
                    self.poll.replace(None);
                    return glib::ControlFlow::Break;
                }
                None => {
                    self.poll.replace(None);
                    return glib::ControlFlow::Break;
                }
            };
            let anchor = top.and_then(|top| doc.positions.anchor(top - doc.margin()));
            let mut done = false;
            let mut shift = 0;
            for batch in ready {
                if batch.generation != doc.generation {
                    continue;
                }
                done |= batch.done;
                for (leaf, height) in (batch.first..).zip(batch.heights) {
                    if let Some(slot) = doc.worker_heights.get_mut(leaf) {
                        *slot = Some(height);
                    }
                    shift += doc.positions.set_measured(leaf, height, anchor, scrolling);
                }
            }
            if done {
                doc.probe.heights_measured();
            }
            (done, shift)
        };
        self.after_heights_changed(shift);
        if done {
            self.check_heights();
            self.poll.replace(None);
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    }

    fn in_motion(&self) -> bool {
        self.last_scroll
            .get()
            .is_some_and(|at| at.elapsed() < MOTION_SETTLE)
            || self.indicator_drag.get().is_some()
    }

    /// After heights changed: move the scroll position by `shift`, so the content on screen
    /// stays still, and resize the scroll range.
    fn after_heights_changed(&self, shift: i64) {
        let value = self
            .vadjustment
            .borrow()
            .as_ref()
            .map(|adjustment| adjustment.value() + units::px_to_f64(shift));
        self.configure_vadjustment(value);
        self.obj().queue_draw();
    }

    /// Sets the scroll range from the document's height, and the scroll value to `value` (or
    /// keeps it), within the range. The shift is applied before clamping, so a shrinking
    /// document at its end does not move the content twice.
    fn configure_vadjustment(&self, value: Option<f64>) {
        let Some(adjustment) = self.vadjustment.borrow().clone() else {
            return;
        };
        let page = self.page.get();
        let total = self.document.borrow().as_ref().map_or(0.0, |doc| {
            units::px_to_f64(doc.positions.total() + 2 * doc.margin())
        });
        let upper = total.max(page);
        let value = value.unwrap_or_else(|| adjustment.value());
        self.adjusting.set(true);
        adjustment.configure(
            value.min(upper - page).max(0.0),
            0.0,
            upper,
            page * 0.1,
            page * 0.9,
            page,
        );
        self.adjusting.set(false);
    }

    /// Resizes the scroll range after a draw changed heights (not during the draw itself).
    fn schedule_reconfigure(&self) {
        if self.reconfigure.borrow().is_some() {
            return;
        }
        let source = glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self.obj(),
            move || {
                view.imp().reconfigure.replace(None);
                view.imp().configure_vadjustment(None);
            }
        ));
        self.reconfigure.replace(Some(source));
    }

    fn set_hadjustment(&self, adjustment: Option<gtk::Adjustment>) {
        // Text wraps to the width, so there is nothing to scroll horizontally.
        self.hadjustment.replace(adjustment);
    }

    fn set_vadjustment(&self, adjustment: Option<gtk::Adjustment>) {
        if let (Some(old), Some(handler)) = (
            self.vadjustment.borrow().as_ref(),
            self.vadjustment_handler.take(),
        ) {
            old.disconnect(handler);
        }
        if let Some(adjustment) = adjustment.as_ref() {
            let handler = adjustment.connect_value_changed(glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                move |_adjustment| view.imp().scrolled()
            ));
            self.vadjustment_handler.replace(Some(handler));
        }
        self.vadjustment.replace(adjustment);
        self.obj().queue_allocate();
    }

    fn scrolled(&self) {
        if !self.adjusting.get() {
            self.last_scroll.set(Some(Instant::now()));
            self.schedule_settle();
        }
        self.obj().queue_draw();
    }

    /// Applies held height changes once scrolling has stopped.
    fn schedule_settle(&self) {
        if let Some(source) = self.settle.take() {
            source.remove();
        }
        let source = glib::timeout_add_local_once(
            MOTION_SETTLE,
            glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                move || view.imp().settled()
            ),
        );
        self.settle.replace(Some(source));
    }

    fn settled(&self) {
        self.settle.replace(None);
        if self.indicator_drag.get().is_some() {
            // Still dragging: wait for the next movement or the end of the drag.
            self.schedule_settle();
            return;
        }
        let top = self.viewport().map(|(top, _page)| top);
        let shift = {
            let mut document = self.document.borrow_mut();
            let Some(doc) = document.as_mut() else {
                return;
            };
            let anchor = top.and_then(|top| doc.positions.anchor(top - doc.margin()));
            doc.positions.release(anchor)
        };
        self.after_heights_changed(shift);
    }

    /// After each frame: prefetch, then tell the probe the frame's work is done.
    fn after_frame(&self) {
        self.prefetch(AFTER_FRAME_BUDGET);
        if let Some(doc) = self.document.borrow().as_ref() {
            doc.probe.frame_work_done();
        }
    }

    /// Lays out leaves near the viewport between frames, so scrolling finds them ready, and
    /// drops cached leaves far from it.
    fn schedule_prefetch(&self) {
        if self.prefetch.borrow().is_some() {
            return;
        }
        let source = glib::idle_add_local(glib::clone!(
            #[weak(rename_to = view)]
            self.obj(),
            #[upgrade_or]
            glib::ControlFlow::Break,
            move || view.imp().prefetch_slice()
        ));
        self.prefetch.replace(Some(source));
    }

    fn prefetch_slice(&self) -> glib::ControlFlow {
        if self.prefetch(IDLE_BUDGET) {
            glib::ControlFlow::Continue
        } else {
            self.prefetch.replace(None);
            glib::ControlFlow::Break
        }
    }

    /// Lays out uncached leaves near the viewport for up to `budget`; returns whether some
    /// remain.
    fn prefetch(&self, budget: Duration) -> bool {
        let Some((top, page)) = self.viewport() else {
            return false;
        };
        let started = Instant::now();
        let context = self.obj().pango_context();
        let scrolling = self.in_motion();
        let (more, shift, resized) = {
            let mut document = self.document.borrow_mut();
            let Some(doc) = document.as_mut() else {
                return false;
            };
            let margin = doc.margin();
            let total = doc.positions.total();
            let anchor = doc.positions.anchor(top - margin);
            let range = doc.positions.visible(
                top - margin - PREFETCH_SCREENS * page,
                (1 + 2 * PREFETCH_SCREENS) * page,
            );
            let mut more = false;
            let mut shift = 0;
            for leaf in Positions::nearest_first(range, anchor) {
                if doc.cache.contains_key(&leaf) {
                    continue;
                }
                if started.elapsed() > budget {
                    more = true;
                    break;
                }
                shift += doc.ensure(&context, leaf, anchor, scrolling);
            }
            if !more {
                let keep = doc.positions.visible(
                    top - margin - KEEP_SCREENS * page,
                    (1 + 2 * KEEP_SCREENS) * page,
                );
                doc.cache.retain(|leaf, _| keep.contains(leaf));
            }
            (more, shift, doc.positions.total() != total)
        };
        if shift != 0 || resized {
            self.after_heights_changed(shift);
        }
        more
    }

    /// For `--check-heights`: lays out every leaf here and compares with the worker's heights.
    fn check_heights(&self) {
        let document = self.document.borrow();
        let Some(doc) = document.as_ref() else {
            return;
        };
        if !doc.probe.checking_heights() {
            return;
        }
        let typesetter = doc.typesetter(self.obj().pango_context());
        let metrics = doc.setup.metrics();
        let mismatches = doc
            .paints
            .iter()
            .zip(&doc.worker_heights)
            .filter(|&(paint, worker)| {
                let (_layout, text_height) = typesetter.measure(paint, doc.width, &mut |_| {});
                *worker != Some(metrics.leaf_height(paint, text_height))
            })
            .count();
        doc.probe.heights_checked(mismatches, doc.paints.len());
    }

    // The scroll indicator.

    fn scroll(&self) -> Option<Scroll> {
        let adjustment = self.vadjustment.borrow().clone()?;
        Some(Scroll {
            value: adjustment.value(),
            page: adjustment.page_size(),
            upper: adjustment.upper(),
        })
    }

    fn on_indicator(&self, x: f64) -> bool {
        x >= f64::from(self.obj().width()) - indicator::HIT_WIDTH
    }

    fn indicator_active(&self) -> bool {
        self.indicator_hover.get() || self.indicator_drag.get().is_some()
    }

    fn set_indicator_hover(&self, hover: bool) {
        if self.indicator_hover.replace(hover) != hover {
            self.obj().queue_draw();
        }
    }

    fn draw_indicator(&self, snapshot: &gtk::Snapshot, color: gtk::gdk::RGBA) {
        let Some(thumb) = self.scroll().and_then(Scroll::thumb) else {
            return;
        };
        let width = if self.indicator_active() {
            indicator::ACTIVE_WIDTH
        } else {
            indicator::WIDTH
        };
        let x = f64::from(self.obj().width()) - indicator::INSET - width;
        snapshot.append_color(
            &color,
            &graphene::Rect::new(
                units::to_f32(x),
                units::to_f32(thumb.top),
                units::to_f32(width),
                units::to_f32(thumb.length),
            ),
        );
    }

    /// A press at the right edge grabs the indicator; in its track off the thumb, it first
    /// jumps there. Any other press is left to other handlers.
    fn indicator_drag_begin(&self, gesture: &gtk::GestureDrag, x: f64, y: f64) {
        let (Some(scroll), Some(adjustment)) = (self.scroll(), self.vadjustment.borrow().clone())
        else {
            return;
        };
        if !self.on_indicator(x) || scroll.thumb().is_none() {
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let start = if scroll.on_thumb(y) {
            scroll.value
        } else {
            scroll.centred_on(y)
        };
        self.indicator_drag.set(Some(start));
        adjustment.set_value(start);
    }

    fn indicator_drag_update(&self, dy: f64) {
        let (Some(start), Some(scroll), Some(adjustment)) = (
            self.indicator_drag.get(),
            self.scroll(),
            self.vadjustment.borrow().clone(),
        ) else {
            return;
        };
        adjustment.set_value(scroll.dragged(start, dy));
    }
}

/// Reports, once, that heights will stay estimates.
fn worker_gone(doc: &mut Doc, gone: WorkerGone) {
    if doc.worker.take().is_some() {
        report_error(gone);
    }
}

impl Doc {
    fn margin(&self) -> i64 {
        self.setup.metrics().px(MARGIN_EM)
    }

    fn typesetter(&self, context: pango::Context) -> Typesetter {
        Typesetter {
            context,
            setup: self.setup,
            palette: self.palette,
        }
    }

    /// Follows the widget's foreground colour (it is only known once the widget is styled, and
    /// changes with the theme); cached leaves were drawn in the old colours.
    fn sync_palette(&mut self, foreground: gtk::gdk::RGBA) {
        if self.palette.foreground != foreground {
            self.palette = Palette::for_foreground(foreground);
            self.cache.clear();
        }
    }

    /// Lays out and draws a leaf if it is not cached, and takes its exact height. Returns the
    /// scroll shift that keeps the anchor still (see `Positions::set`).
    fn ensure(
        &mut self,
        context: &pango::Context,
        leaf: usize,
        anchor: Option<usize>,
        scrolling: bool,
    ) -> i64 {
        if self.cache.contains_key(&leaf) {
            return 0;
        }
        let typesetter = self.typesetter(context.clone());
        let Some(paint) = self.paints.get(leaf) else {
            return 0;
        };
        let probe = Rc::clone(&self.probe);
        let mut counted = |bytes: usize| probe.layout_created(bytes);
        let (layout, text_height) = typesetter.measure(paint, self.width, &mut counted);
        let geometry = self
            .setup
            .metrics()
            .geometry(paint, text_height, self.width);
        let node = draw_leaf(&typesetter, &layout, &geometry, &mut counted);
        self.cache.insert(leaf, node);
        self.positions.set(leaf, geometry.height, anchor, scrolling)
    }
}

/// Draws one leaf at the origin, as its geometry places it.
fn draw_leaf(
    typesetter: &Typesetter,
    layout: &pango::Layout,
    geometry: &LeafGeometry,
    counted: &mut dyn FnMut(usize),
) -> Option<gsk::RenderNode> {
    let palette = typesetter.palette;
    let snapshot = gtk::Snapshot::new();
    for (rect, shade) in &geometry.fills {
        let color = match shade {
            Shade::CodeBackground => palette.faint(0.06),
            Shade::Bar(BarColor::Quote) => palette.faint(0.25),
            Shade::Bar(BarColor::Alert(kind)) => palette.alert(*kind),
            Shade::Rule => palette.faint(0.2),
            Shade::HeadingRule => palette.faint(0.15),
        };
        snapshot.append_color(&color, &graphene_rect(*rect));
    }
    for (text, right, top) in &geometry.markers {
        let marker = typesetter.marker(text, counted);
        let marker_width = i64::from(marker.pixel_size().0);
        snapshot.save();
        snapshot.translate(&point(right - marker_width, *top));
        snapshot.append_layout(&marker, &palette.foreground);
        snapshot.restore();
    }
    snapshot.save();
    snapshot.translate(&point(geometry.text_x, geometry.text_y));
    snapshot.append_layout(layout, &palette.foreground);
    snapshot.restore();
    snapshot.to_node()
}

fn point(x: i64, y: i64) -> graphene::Point {
    graphene::Point::new(
        units::to_f32(units::px_to_f64(x)),
        units::to_f32(units::px_to_f64(y)),
    )
}

fn graphene_rect(rect: Rect) -> graphene::Rect {
    graphene::Rect::new(
        units::to_f32(units::px_to_f64(rect.x)),
        units::to_f32(units::px_to_f64(rect.y)),
        units::to_f32(units::px_to_f64(rect.width.max(0))),
        units::to_f32(units::px_to_f64(rect.height.max(0))),
    )
}
