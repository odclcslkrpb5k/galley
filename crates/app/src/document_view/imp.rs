use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk, pango};

use crate::probe::Probe;
use crate::render::fenwick::Fenwick;
use crate::render::heights::{Job, Worker};
use crate::render::paint::{BarColor, Decoration, Fill, LeafPaint};
use crate::render::text::{self, Palette, TextSetup};
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
    /// When the view last scrolled: height changes above the viewport wait until it settles.
    last_scroll: Cell<Option<Instant>>,
    poll: RefCell<Option<glib::SourceId>>,
    prefetch: RefCell<Option<glib::SourceId>>,
    settle: RefCell<Option<glib::SourceId>>,
    /// The viewport's height, to configure the adjustment outside of a snapshot.
    page: Cell<f64>,
    /// The after-paint handler that prefetches after each frame, while mapped.
    after_paint: RefCell<Option<(gtk::gdk::FrameClock, glib::SignalHandlerId)>>,
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
            poll: RefCell::default(),
            prefetch: RefCell::default(),
            settle: RefCell::default(),
            page: Cell::new(0.0),
            after_paint: RefCell::default(),
        }
    }
}

#[derive(Debug)]
struct Doc {
    paints: Arc<Vec<LeafPaint>>,
    heights: Fenwick,
    /// Whether a leaf's height is measured (by the worker or here), not estimated.
    exact: Vec<bool>,
    /// The width text is wrapped to, without margins; 0 until allocated.
    width: i64,
    generation: u64,
    setup: TextSetup,
    palette: Palette,
    cache: HashMap<usize, Cached>,
    worker: Worker,
    /// Height changes for leaves above the viewport, held while scrolling is in motion:
    /// `GtkScrolledWindow`'s kinetic scrolling overwrites outside changes to the scroll position,
    /// so a correction made mid-fling would jump.
    queued: Vec<(usize, i64)>,
    probe: Rc<Probe>,
}

#[derive(Debug)]
struct Cached {
    node: Option<gsk::RenderNode>,
}

#[glib::object_subclass]
impl ObjectSubclass for DocumentView {
    const NAME: &'static str = "GbkDocumentView";
    type Type = super::DocumentView;
    type ParentType = gtk::Widget;
    type Interfaces = (gtk::Scrollable,);
}

#[glib::derived_properties]
impl ObjectImpl for DocumentView {}

impl WidgetImpl for DocumentView {
    fn map(&self) {
        self.parent_map();
        if let Some(clock) = self.obj().frame_clock() {
            let handler = clock.connect_after_paint(glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                move |_clock| {
                    view.imp().prefetch(AFTER_FRAME_BUDGET);
                }
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
        self.set_width(i64::from(width));
        self.configure_vadjustment();
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
        let margin = doc.setup.metrics().px(MARGIN_EM);
        // Lay out what is visible first (normally already cached), then draw at the positions
        // those layouts settle.
        let range = doc.visible(top - margin, page);
        for leaf in range.clone() {
            doc.ensure(&context, leaf);
        }
        for leaf in doc.visible(top - margin, page) {
            let Some(Cached { node: Some(node) }) = doc.cache.get(&leaf) else {
                continue;
            };
            let y = margin + doc.heights.start(leaf) - top;
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                units::to_f32(units::px_to_f64(margin)),
                units::to_f32(units::px_to_f64(y)),
            ));
            snapshot.append_node(node);
            snapshot.restore();
        }
        doc.probe.document_drawn(doc.paints.len());
        drop(document);
        self.schedule_prefetch();
    }
}

impl ScrollableImpl for DocumentView {}

impl DocumentView {
    pub(super) fn set_document(&self, paints: Vec<LeafPaint>, probe: Rc<Probe>) {
        let widget = self.obj();
        let setup = TextSetup::of(&*widget);
        let count = paints.len();
        *self.document.borrow_mut() = Some(Doc {
            paints: Arc::new(paints),
            heights: Fenwick::new(vec![0; count]),
            exact: vec![false; count],
            width: 0,
            generation: 0,
            setup,
            palette: Palette::for_foreground(widget.color()),
            cache: HashMap::new(),
            worker: Worker::spawn(),
            queued: Vec::new(),
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
            let margin = doc.setup.metrics().px(MARGIN_EM);
            for leaf in doc.visible(-margin, units::round_px(height)) {
                doc.ensure(&context, leaf);
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

    /// Re-estimates every height and restarts the worker when the text width changes.
    fn set_width(&self, widget_width: i64) {
        let started = {
            let mut document = self.document.borrow_mut();
            let Some(doc) = document.as_mut() else {
                return;
            };
            let metrics = doc.setup.metrics();
            let width = (widget_width - 2 * metrics.px(MARGIN_EM)).max(1);
            if width == doc.width {
                return;
            }
            doc.probe.width_reset();
            doc.width = width;
            doc.generation += 1;
            doc.cache.clear();
            doc.queued.clear();
            doc.exact = vec![false; doc.paints.len()];
            doc.heights = Fenwick::new(
                doc.paints
                    .iter()
                    .map(|paint| metrics.estimate(paint, width))
                    .collect(),
            );
            doc.worker.submit(Job {
                generation: doc.generation,
                view_text_width: width,
                paints: Arc::clone(&doc.paints),
                setup: doc.setup,
            });
            true
        };
        if started {
            self.start_polling();
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
        let anchor = self.anchor();
        let in_motion = self.in_motion();
        let (done, shift) = {
            let mut document = self.document.borrow_mut();
            let Some(doc) = document.as_mut() else {
                self.poll.replace(None);
                return glib::ControlFlow::Break;
            };
            let mut done = false;
            let mut shift = 0;
            for batch in doc.worker.ready() {
                if batch.generation != doc.generation {
                    continue;
                }
                done |= batch.done;
                for (leaf, height) in (batch.first..).zip(batch.heights) {
                    if doc.exact.get(leaf).copied().unwrap_or(true) {
                        continue;
                    }
                    if let Some(slot) = doc.exact.get_mut(leaf) {
                        *slot = true;
                    }
                    shift += doc.change_height(leaf, height, anchor, in_motion);
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

    /// The leaf at the top of the viewport: height changes above it move the scroll position
    /// by the same amount, so what is on screen stays still.
    fn anchor(&self) -> Option<usize> {
        let (top, _page) = self.viewport()?;
        let document = self.document.borrow();
        let doc = document.as_ref()?;
        let margin = doc.setup.metrics().px(MARGIN_EM);
        doc.heights.leaf_at(top - margin)
    }

    fn in_motion(&self) -> bool {
        self.last_scroll
            .get()
            .is_some_and(|at| at.elapsed() < MOTION_SETTLE)
    }

    /// After heights changed: keep the viewport on the same content and resize the scrollbar.
    fn after_heights_changed(&self, shift: i64) {
        if let Some(adjustment) = self.vadjustment.borrow().clone() {
            self.configure_vadjustment();
            if shift != 0 {
                adjustment.set_value(adjustment.value() + units::px_to_f64(shift));
            }
        }
        self.obj().queue_draw();
    }

    fn configure_vadjustment(&self) {
        let Some(adjustment) = self.vadjustment.borrow().clone() else {
            return;
        };
        let page = self.page.get();
        let total = self.document.borrow().as_ref().map_or(0.0, |doc| {
            units::px_to_f64(doc.heights.total() + 2 * doc.setup.metrics().px(MARGIN_EM))
        });
        let upper = total.max(page);
        adjustment.configure(
            adjustment.value().min(upper - page).max(0.0),
            0.0,
            upper,
            page * 0.1,
            page * 0.9,
            page,
        );
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
        self.last_scroll.set(Some(Instant::now()));
        self.schedule_settle();
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
        let anchor = self.anchor();
        let shift = {
            let mut document = self.document.borrow_mut();
            let Some(doc) = document.as_mut() else {
                return;
            };
            let queued = std::mem::take(&mut doc.queued);
            queued
                .into_iter()
                .map(|(leaf, height)| doc.change_height(leaf, height, anchor, false))
                .sum()
        };
        self.after_heights_changed(shift);
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
        let (more, shift) = {
            let mut document = self.document.borrow_mut();
            let Some(doc) = document.as_mut() else {
                return false;
            };
            let margin = doc.setup.metrics().px(MARGIN_EM);
            let start = top - margin - PREFETCH_SCREENS * page;
            let range = doc.visible(start, (1 + 2 * PREFETCH_SCREENS) * page);
            let mut more = false;
            let mut shift = 0;
            let anchor = doc.heights.leaf_at(top - margin);
            // Nearest the viewport first: below it, then above.
            let below = range.clone().filter(|&leaf| Some(leaf) >= anchor);
            let above = range.clone().rev().filter(|&leaf| Some(leaf) < anchor);
            for leaf in below.chain(above) {
                if doc.cache.contains_key(&leaf) {
                    continue;
                }
                if started.elapsed() > budget {
                    more = true;
                    break;
                }
                let before = doc.heights.height(leaf).unwrap_or(0);
                doc.ensure(&context, leaf);
                let after = doc.heights.height(leaf).unwrap_or(0);
                if Some(leaf) < anchor {
                    shift += after - before;
                }
            }
            if !more {
                let keep = doc.visible(
                    top - margin - KEEP_SCREENS * page,
                    (1 + 2 * KEEP_SCREENS) * page,
                );
                doc.cache.retain(|leaf, _| keep.contains(leaf));
            }
            (more, shift)
        };
        if shift != 0 {
            self.after_heights_changed(shift);
        }
        more
    }

    /// For `--check-heights`: lays out every leaf here and compares with the worker's heights.
    fn check_heights(&self) {
        let context = self.obj().pango_context();
        let document = self.document.borrow();
        let Some(doc) = document.as_ref() else {
            return;
        };
        if !doc.probe.checking_heights() {
            return;
        }
        let metrics = doc.setup.metrics();
        let mismatches = doc
            .paints
            .iter()
            .enumerate()
            .filter(|&(leaf, paint)| {
                let width = metrics.text_width(paint, doc.width);
                let layout =
                    text::layout(&context, doc.setup, doc.palette, paint, width, &mut |_| {});
                let height = metrics.leaf_height(paint, i64::from(layout.pixel_size().1));
                doc.heights.height(leaf) != Some(height)
            })
            .count();
        doc.probe.heights_checked(mismatches, doc.paints.len());
    }
}

impl Doc {
    /// Follows the widget's foreground colour (it is only known once the widget is styled, and
    /// changes with the theme); cached leaves were drawn in the old colours.
    fn sync_palette(&mut self, foreground: gtk::gdk::RGBA) {
        if self.palette.foreground != foreground {
            self.palette = Palette::for_foreground(foreground);
            self.cache.clear();
        }
    }

    /// The leaves overlapping `[top, top + height)` in document coordinates.
    fn visible(&self, top: i64, height: i64) -> std::ops::Range<usize> {
        let Some(first) = self.heights.leaf_at(top.max(0)) else {
            return 0..0;
        };
        let bottom = top + height;
        let mut end = first;
        while end < self.heights.len() && self.heights.start(end) < bottom {
            end += 1;
        }
        first..end
    }

    /// Lays out and draws a leaf if it is not cached; its measured height replaces the
    /// estimate.
    fn ensure(&mut self, context: &pango::Context, leaf: usize) {
        if self.cache.contains_key(&leaf) {
            return;
        }
        let Some(paint) = self.paints.get(leaf) else {
            return;
        };
        let metrics = self.setup.metrics();
        let width = metrics.text_width(paint, self.width);
        let probe = Rc::clone(&self.probe);
        let mut counted = |bytes: usize| probe.layout_created(bytes);
        let layout = text::layout(
            context,
            self.setup,
            self.palette,
            paint,
            width,
            &mut counted,
        );
        let text_height = i64::from(layout.pixel_size().1);
        let node = draw_leaf(
            context,
            self.setup,
            self.palette,
            paint,
            &layout,
            text_height,
            self.width,
            &mut counted,
        );
        self.cache.insert(leaf, Cached { node });
        if let Some(slot) = self.exact.get_mut(leaf) {
            *slot = true;
        }
        self.heights
            .set(leaf, metrics.leaf_height(paint, text_height));
    }

    /// Sets a leaf's height. Returns how far the scroll position must move to keep the content
    /// on screen still: the change, for a leaf above the anchor; while scrolling, such changes
    /// are held instead (0).
    fn change_height(
        &mut self,
        leaf: usize,
        height: i64,
        anchor: Option<usize>,
        in_motion: bool,
    ) -> i64 {
        let above = anchor.is_some_and(|anchor| leaf < anchor);
        if above && in_motion {
            self.queued.push((leaf, height));
            return 0;
        }
        let delta = self.heights.set(leaf, height).unwrap_or(0);
        if above { delta } else { 0 }
    }
}

/// Draws one leaf, at the origin, the full text width wide.
#[expect(
    clippy::too_many_arguments,
    reason = "the leaf's paint, layout and the setup to draw it with; a struct would only rename them"
)]
fn draw_leaf(
    context: &pango::Context,
    setup: TextSetup,
    palette: Palette,
    paint: &LeafPaint,
    layout: &pango::Layout,
    text_height: i64,
    width: i64,
    counted: &mut dyn FnMut(usize),
) -> Option<gsk::RenderNode> {
    let metrics = setup.metrics();
    let snapshot = gtk::Snapshot::new();
    let height = metrics.leaf_height(paint, text_height);
    let gap_above = metrics.px(paint.gap_above_em);
    let pad_above = metrics.px(paint.pad_above_em);
    let indent = metrics.px(paint.indent_em);
    let rect = |x: i64, y: i64, w: i64, h: i64| {
        graphene::Rect::new(
            units::to_f32(units::px_to_f64(x)),
            units::to_f32(units::px_to_f64(y)),
            units::to_f32(units::px_to_f64(w.max(0))),
            units::to_f32(units::px_to_f64(h.max(0))),
        )
    };
    if paint.background == Some(Fill::Code) {
        let box_height = pad_above + text_height + metrics.px(paint.pad_below_em);
        snapshot.append_color(
            &palette.faint(0.06),
            &rect(indent, gap_above, width - indent, box_height),
        );
    }
    let bar_width = metrics.px(0.25).max(3);
    for decoration in &paint.decorations {
        match decoration {
            Decoration::Bar { x_em, color, last } => {
                let color = match color {
                    BarColor::Quote => palette.faint(0.25),
                    BarColor::Alert(kind) => palette.alert(*kind),
                };
                let bar_height = if *last {
                    height - metrics.px(paint.gap_below_em)
                } else {
                    height
                };
                snapshot.append_color(&color, &rect(metrics.px(*x_em), 0, bar_width, bar_height));
            }
            Decoration::Marker { x_em, text: marker } => {
                let marker = text::marker_layout(context, setup, marker, counted);
                let marker_width = i64::from(marker.pixel_size().0);
                snapshot.save();
                snapshot.translate(&graphene::Point::new(
                    units::to_f32(units::px_to_f64(metrics.px(*x_em) - marker_width)),
                    units::to_f32(units::px_to_f64(gap_above + pad_above)),
                ));
                snapshot.append_layout(&marker, &palette.foreground);
                snapshot.restore();
            }
            Decoration::Rule => {
                let y = gap_above + text_height / 2;
                snapshot.append_color(&palette.faint(0.2), &rect(indent, y, width - indent, 2));
            }
            Decoration::HeadingRule => {
                let y = gap_above + text_height + metrics.px(0.25);
                snapshot.append_color(&palette.faint(0.15), &rect(indent, y, width - indent, 1));
            }
        }
    }
    snapshot.save();
    snapshot.translate(&graphene::Point::new(
        units::to_f32(units::px_to_f64(indent + metrics.horizontal_padding(paint))),
        units::to_f32(units::px_to_f64(gap_above + pad_above)),
    ));
    snapshot.append_layout(layout, &palette.foreground);
    snapshot.restore();
    snapshot.to_node()
}
