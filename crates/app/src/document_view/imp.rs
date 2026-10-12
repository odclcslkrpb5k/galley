use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib, graphene, gsk, pango};

use crate::images::{Gallery, ImageCounts, ImageState, TEXTURE_BUDGET};
use crate::links::{self, Opening};
use crate::output::report_error;
use crate::probe::Probe;
use crate::render::geometry::{LeafGeometry, MarkerPlace, Rect, Shade};
use crate::render::heights::{Job, Worker, WorkerGone};
use crate::render::indicator::{self, Scroll};
use crate::render::paint::TextAttr;
use crate::render::paint::{self, BarColor, LeafPaint, Target};
use crate::render::positions::{Positions, Spot};
use crate::render::text::{self, ColumnWidths, Content, Measured, Palette, TextSetup, Typesetter};
use crate::units;
use gbk_markdown_core::units::{DetailsId, ImageId, LeafId, TableId};

/// A summary's marker while its `<details>` is open, and closed.
const OPEN_MARKER: &str = "▾";
const CLOSED_MARKER: &str = "▸";
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
/// How often finished image loads are collected while any are in flight: a row of badges
/// arriving within it is laid out again once, not once per badge.
const IMAGE_POLL: Duration = Duration::from_millis(100);
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
    /// Collects finished image loads while any are in flight.
    image_poll: RefCell<Option<glib::SourceId>>,
    /// Images were wanted in the last frame: send them after it.
    images_wanted: Cell<bool>,
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
    /// The document's file, which its relative links start from.
    path: RefCell<Option<PathBuf>>,
    /// The paint to show at the top once the view has a size (`--anchor`).
    start: Cell<Option<usize>>,
    /// Where the view was before each jump to a link inside the document, for going back.
    history: RefCell<Vec<Spot>>,
    /// The link or summary under the pointer.
    hovered: RefCell<Option<Pointed>>,
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
            image_poll: RefCell::default(),
            images_wanted: Cell::new(false),
            prefetch: RefCell::default(),
            settle: RefCell::default(),
            reconfigure: RefCell::default(),
            page: Cell::new(0.0),
            widget_width: Cell::new(0),
            after_paint: RefCell::default(),
            indicator_hover: Cell::new(false),
            indicator_drag: Cell::new(None),
            path: RefCell::default(),
            start: Cell::new(None),
            history: RefCell::default(),
            hovered: RefCell::default(),
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
    /// Tables' column widths at this width and text setup.
    columns: ColumnWidths,
    /// Anchor names and the leaves they name (`paint::anchor_leaves`).
    anchors: HashMap<String, LeafId>,
    /// The leaf last hit-tested for links, laid out (the pointer moves over one leaf at a time).
    hit: Option<(usize, Measured, LeafGeometry)>,
    /// Whether each `<details>` is open; paints inside a closed one have no height.
    open: Vec<bool>,
    /// Each paint's laid-out height, open or not, once known at this width.
    natural: Vec<Option<i64>>,
    /// The document's images and their loading.
    gallery: Gallery,
    /// Each table's pieces (paints), to lay out again together when an image in its column
    /// sample arrives (its columns may widen).
    tables: HashMap<TableId, Vec<usize>>,
    /// The image table's version when the height worker's job began (`--check-heights`
    /// compares only heights measured with every image's size in).
    job_images: u64,
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
        let click = gtk::GestureClick::new();
        // Any button: the primary opens links, the "back" mouse button goes back.
        click.set_button(0);
        click.connect_released(glib::clone!(
            #[weak(rename_to = view)]
            widget,
            move |gesture, presses, x, y| view.imp().clicked(
                gesture.current_button(),
                presses,
                x,
                y
            )
        ));
        widget.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = view)]
            widget,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_controller, key, _code, modifiers| view.imp().key_pressed(key, modifiers)
        ));
        widget.add_controller(keys);
        widget.set_focusable(true);
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = view)]
            widget,
            move |_controller, x, y| {
                let on_indicator = view.imp().on_indicator(x);
                view.imp().set_indicator_hover(on_indicator);
                view.imp().hover((!on_indicator).then_some((x, y)));
            }
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            widget,
            move |_controller| {
                view.imp().set_indicator_hover(false);
                view.imp().hover(None);
            }
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
        // For its keys (Alt+Left goes back).
        self.obj().grab_focus();
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
        // The first allocation: open at `--anchor`'s paint.
        let value = self.start.take().map_or(value, |paint| {
            self.document
                .borrow()
                .as_ref()
                .map(|doc| units::px_to_f64(doc.top_of(paint)))
        });
        self.configure_vadjustment(value);
    }

    fn snapshot(&self, snapshot: &gtk::Snapshot) {
        let Some((top, page)) = self.viewport() else {
            return;
        };
        let widget = self.obj();
        let context = widget.pango_context();
        let still = !self.in_motion();
        let mut wanted = false;
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
            // Its placeholders are drawn: now its images may load, unless the view is moving
            // (a fling would queue every image it passed); settling draws again.
            if still && let Some(paint) = doc.paints.get(leaf) {
                wanted |= doc.gallery.want(paint);
            }
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
        if wanted {
            self.images_wanted.set(true);
        }
        self.schedule_prefetch();
    }
}

impl ScrollableImpl for DocumentView {}

impl DocumentView {
    pub(super) fn set_document(&self, document: super::Opened, probe: Rc<Probe>) {
        let super::Opened {
            paints,
            anchors,
            details,
            images,
            path,
            start,
            offline,
        } = document;
        // While --check-heights compares layouts, images load only if it waits for them.
        let images_enabled = !probe.checking_heights() || probe.waits_for_images();
        let tables = table_pieces(&paints);
        let gallery = Gallery::new(images, &paints, path.clone(), offline, images_enabled);
        let start = start.and_then(|leaf| paint::first_paint_of(&paints, leaf.index()));
        let mut open = details;
        // Opening inside a closed `<details>` opens it.
        for id in start
            .and_then(|start| paints.get(start))
            .map(|paint| paint.details.clone())
            .unwrap_or_default()
        {
            if let Some(slot) = open.get_mut(id.index()) {
                *slot = true;
            }
        }
        self.path.replace(path);
        self.start.set(start);
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
            columns: ColumnWidths::default(),
            anchors,
            hit: None,
            open,
            natural: vec![None; count],
            gallery,
            tables,
            job_images: 0,
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
            let top = self
                .start
                .get()
                .map_or(-margin, |paint| doc.positions.start(paint));
            for leaf in doc.positions.visible(top, units::round_px(height)) {
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
        doc.job_images = doc.gallery.table().version();
        doc.cache.clear();
        doc.hit = None;
        doc.natural = vec![None; doc.paints.len()];
        let metrics = doc.setup.metrics();
        doc.positions = Positions::new(
            doc.paints
                .iter()
                .map(|paint| {
                    if doc.shown(paint) {
                        metrics.estimate(paint, width)
                    } else {
                        0
                    }
                })
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
                images: Arc::clone(doc.gallery.table()),
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
            // Natural column widths are measured in the old setup's fonts.
            doc.columns.clear();
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

    fn start_image_polling(&self) {
        if self.image_poll.borrow().is_some() {
            return;
        }
        let source = glib::timeout_add_local(
            IMAGE_POLL,
            glib::clone!(
                #[weak(rename_to = view)]
                self.obj(),
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || view.imp().collect_images()
            ),
        );
        self.image_poll.replace(Some(source));
    }

    /// Takes finished image loads. The paints showing them (and every piece of a table whose
    /// columns they may widen) are laid out again now, between frames, if near the view, so
    /// their new heights shift the view as anchoring says; others when they next come near.
    fn collect_images(&self) -> glib::ControlFlow {
        let viewport = self.viewport();
        let scrolling = self.in_motion();
        let context = self.obj().pango_context();
        let (changed, busy, shift) = {
            let mut document = self.document.borrow_mut();
            let Some(doc) = document.as_mut() else {
                self.image_poll.replace(None);
                return glib::ControlFlow::Break;
            };
            let arrived = doc.gallery.collect(Instant::now());
            let mut shift = 0;
            if !arrived.is_empty() {
                doc.hit = None;
                let mut paints = doc.affected_by(&arrived);
                paints.sort_unstable();
                paints.dedup();
                for paint in &paints {
                    doc.cache.remove(paint);
                }
                if let Some((top, page)) = viewport {
                    let margin = doc.margin();
                    let anchor = doc.positions.anchor(top - margin);
                    let near = doc.positions.visible(
                        top - margin - PREFETCH_SCREENS * page,
                        (1 + 2 * PREFETCH_SCREENS) * page,
                    );
                    for paint in paints.into_iter().filter(|paint| near.contains(paint)) {
                        shift += doc.ensure(&context, paint, anchor, scrolling);
                    }
                }
            }
            (!arrived.is_empty(), doc.gallery.busy(), shift)
        };
        if changed {
            self.after_heights_changed(shift);
        }
        if busy {
            return glib::ControlFlow::Continue;
        }
        self.image_poll.replace(None);
        self.check_heights_when_settled();
        glib::ControlFlow::Break
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
                    let height = doc.record_height(leaf, height);
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
            // First: the check waits while the worker is still being polled.
            self.poll.replace(None);
            self.check_heights_when_settled();
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
        // Images the frame wanted are sent now, after it: never inside a frame (the first
        // frame's thread start included).
        if self.images_wanted.replace(false) {
            if let Some(doc) = self.document.borrow_mut().as_mut() {
                doc.gallery.dispatch(Instant::now());
            }
            self.start_image_polling();
        }
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
                let paints = Arc::clone(&doc.paints);
                let kept: std::collections::HashSet<ImageId> = paints
                    .get(keep)
                    .unwrap_or_default()
                    .iter()
                    .flat_map(|paint| paint.images.iter().copied())
                    .collect();
                doc.gallery
                    .trim(TEXTURE_BUDGET, &|image| kept.contains(&image));
            }
            (more, shift, doc.positions.total() != total)
        };
        if shift != 0 || resized {
            self.after_heights_changed(shift);
        }
        more
    }

    /// For `--check-heights`: compares once the worker is done and, with `--wait-images`, no
    /// image is loading; if an image arrived after the worker began, it measures again first.
    fn check_heights_when_settled(&self) {
        let remeasure = {
            let document = self.document.borrow();
            let Some(doc) = document.as_ref() else {
                return;
            };
            if !doc.probe.checking_heights() || doc.gallery.busy() || self.poll.borrow().is_some() {
                return;
            }
            doc.job_images != doc.gallery.table().version()
        };
        if remeasure {
            // Forces `set_width` to start the worker over.
            if let Some(doc) = self.document.borrow_mut().as_mut() {
                doc.width = 0;
            }
            let value = self.set_width(self.widget_width.get());
            self.configure_vadjustment(value);
            return;
        }
        self.check_heights();
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
        let mut columns = ColumnWidths::default();
        let mismatches = doc
            .paints
            .iter()
            .zip(&doc.worker_heights)
            .filter(|&(paint, worker)| {
                let measured = typesetter.measure(paint, doc.width, &mut columns, &mut |_| {});
                *worker != Some(metrics.leaf_height(paint, measured.height))
            })
            .count();
        doc.probe.heights_checked(mismatches, doc.paints.len());
    }

    // Links and <details>.

    /// The link at a point of the view, if any.
    pub(super) fn link_at(&self, x: f64, y: f64) -> Option<Target> {
        match self.hit_at(x, y)? {
            Hit::Link(target) => Some(target),
            Hit::Image(_) => None,
        }
    }

    /// The link or image at a point of the view, if any.
    fn hit_at(&self, x: f64, y: f64) -> Option<Hit> {
        let (top, _page) = self.viewport()?;
        let context = self.obj().pango_context();
        let mut document = self.document.borrow_mut();
        let doc = document.as_mut()?;
        let margin = doc.margin();
        let y = top - margin + units::round_px(y);
        let paint = doc.positions.visible(y, 1).next()?;
        let local = (units::round_px(x) - margin, y - doc.positions.start(paint));
        doc.hit_in(&context, paint, local)
    }

    /// For --click-at: what a click at a point does, done as `Scope::Document` allows.
    pub(super) fn probe_click(&self, x: f64, y: f64) -> String {
        let pointed = self.pointed_at(x, y);
        let label = match &pointed {
            Some(Pointed::Link(target)) => target.label(),
            Some(Pointed::Image(image)) => format!("image:{}", image.index()),
            Some(Pointed::Summary(details)) => format!("summary:{}", details.index()),
            None => "none".to_owned(),
        };
        if let Some(pointed) = pointed {
            self.activate(pointed, Scope::Document);
        }
        label
    }

    pub(super) fn probe_images(&self) -> ImageCounts {
        self.document
            .borrow()
            .as_ref()
            .map_or_else(ImageCounts::default, |doc| doc.gallery.counts())
    }

    /// The document's height, and the leaf whose paint is at the top of the view.
    pub(super) fn probe_state(&self) -> (i64, usize) {
        let top = self.viewport().map_or(0, |(top, _page)| top);
        self.document.borrow().as_ref().map_or((0, 0), |doc| {
            let paint = doc.positions.anchor(top - doc.margin()).unwrap_or(0);
            let leaf = doc.paints.get(paint).map_or(0, |paint| paint.source);
            (doc.positions.total(), leaf)
        })
    }

    /// What a click at a point would do: follow a link, or open or close a `<details>`.
    fn pointed_at(&self, x: f64, y: f64) -> Option<Pointed> {
        match self.hit_at(x, y) {
            Some(Hit::Link(target)) => return Some(Pointed::Link(target)),
            Some(Hit::Image(image)) => return Some(Pointed::Image(image)),
            None => {}
        }
        let (top, _page) = self.viewport()?;
        let document = self.document.borrow();
        let doc = document.as_ref()?;
        let y = top - doc.margin() + units::round_px(y);
        let paint = doc.positions.visible(y, 1).next()?;
        // On the paint: not in the margin above the document, nor past its end.
        let inside = (doc.positions.start(paint)..doc.positions.start(paint + 1)).contains(&y);
        doc.paints
            .get(paint)?
            .summary_of
            .filter(|_| inside)
            .map(Pointed::Summary)
    }

    /// The pointer moved to `at` (or left): a hand over a link or summary, and where a link
    /// goes. Not while scrolling: the content moves under a still pointer, and laying out the
    /// leaf it is over would cost frames.
    fn hover(&self, at: Option<(f64, f64)>) {
        if at.is_some() && self.in_motion() {
            return;
        }
        let pointed = at.and_then(|(x, y)| self.pointed_at(x, y));
        if *self.hovered.borrow() == pointed {
            return;
        }
        let widget = self.obj();
        let clickable = matches!(pointed, Some(Pointed::Link(_) | Pointed::Summary(_)));
        widget.set_cursor_from_name(clickable.then_some("pointer"));
        let path = self.path.borrow();
        let tooltip = match &pointed {
            Some(Pointed::Link(target)) => describe(target, path.as_deref()),
            Some(Pointed::Image(image)) => self
                .document
                .borrow()
                .as_ref()
                .and_then(|doc| image_note(&doc.gallery, *image)),
            Some(Pointed::Summary(_)) | None => None,
        };
        widget.set_tooltip_text(tooltip.as_deref());
        self.hovered.replace(pointed);
    }

    fn clicked(&self, button: u32, presses: i32, x: f64, y: f64) {
        match button {
            gtk::gdk::BUTTON_PRIMARY if presses == 1 && !self.on_indicator(x) => {
                if let Some(pointed) = self.pointed_at(x, y) {
                    self.activate(pointed, Scope::Everywhere);
                }
            }
            // The "back" side button.
            8 => self.go_back(),
            _ => {}
        }
    }

    fn key_pressed(
        &self,
        key: gtk::gdk::Key,
        modifiers: gtk::gdk::ModifierType,
    ) -> glib::Propagation {
        let alt = modifiers.contains(gtk::gdk::ModifierType::ALT_MASK);
        if alt && key == gtk::gdk::Key::Left {
            self.go_back();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    }

    /// Does what a click on `pointed` means; in `Scope::Document`, nothing that leaves the
    /// document (a URL, another file) is opened.
    fn activate(&self, pointed: Pointed, scope: Scope) {
        let result = match pointed {
            // An image that is not a link does nothing when clicked.
            Pointed::Image(_) => Ok(()),
            Pointed::Summary(details) => {
                self.toggle(details);
                Ok(())
            }
            Pointed::Link(Target::Leaf(leaf)) => {
                self.reveal_leaf(leaf);
                Ok(())
            }
            Pointed::Link(Target::Missing(name)) => Err(OpenError::NoAnchor(name)),
            Pointed::Link(Target::Uri(_) | Target::Relative { .. }) if scope == Scope::Document => {
                Ok(())
            }
            Pointed::Link(Target::Uri(uri)) => {
                self.launch_uri(uri);
                Ok(())
            }
            Pointed::Link(Target::Relative { path, fragment }) => {
                self.open_relative(&path, fragment.as_deref())
            }
        };
        if let Err(error) = result {
            report_error(error);
        }
    }

    /// Opens or closes a `<details>`: its content's heights become their laid-out heights (or
    /// estimates) or nothing, keeping the content at the top of the view still.
    fn toggle(&self, details: DetailsId) {
        let top = self.viewport().map(|(top, _page)| top);
        let shift = {
            let mut document = self.document.borrow_mut();
            let Some(doc) = document.as_mut() else {
                return;
            };
            let Some(slot) = doc.open.get_mut(details.index()) else {
                return;
            };
            *slot = !*slot;
            let anchor = top.and_then(|top| doc.positions.anchor(top - doc.margin()));
            doc.hit = None;
            let paints = Arc::clone(&doc.paints);
            let mut shift = 0;
            for (index, paint) in paints.iter().enumerate() {
                let inside = paint.details.contains(&details);
                if inside || paint.summary_of == Some(details) {
                    // Redrawn: content laid out again, the summary's marker turned.
                    doc.cache.remove(&index);
                }
                if inside {
                    let height = doc.shown_height(index, paint);
                    shift += doc.positions.estimate(index, height, anchor);
                }
            }
            shift
        };
        self.after_heights_changed(shift);
    }

    /// Shows a leaf's first paint at the top, opening any `<details>` closed around it first.
    fn reveal_leaf(&self, leaf: LeafId) {
        let (paint, closed) = {
            let document = self.document.borrow();
            let Some(doc) = document.as_ref() else {
                return;
            };
            let Some(paint) = paint::first_paint_of(&doc.paints, leaf.index()) else {
                return;
            };
            (paint, doc.closed_around(paint))
        };
        for details in closed {
            self.toggle(details);
        }
        self.jump_to(paint);
    }

    /// Shows paint `paint` at the top, remembering where the view was.
    fn jump_to(&self, paint: usize) {
        let (top, value) = {
            let document = self.document.borrow();
            let Some(doc) = document.as_ref() else {
                return;
            };
            let Some((top, _page)) = self.viewport() else {
                return;
            };
            (
                doc.positions.spot(top - doc.margin()),
                units::px_to_f64(doc.top_of(paint)),
            )
        };
        if let Some(spot) = top {
            self.history.borrow_mut().push(spot);
        }
        self.configure_vadjustment(Some(value));
        self.obj().queue_draw();
    }

    /// Returns to where the view was before the last jump.
    fn go_back(&self) {
        let Some(spot) = self.history.borrow_mut().pop() else {
            return;
        };
        let value = self
            .document
            .borrow()
            .as_ref()
            .map(|doc| units::px_to_f64(doc.positions.y_of(spot) + doc.margin()));
        self.configure_vadjustment(value);
        self.obj().queue_draw();
    }

    fn launch_uri(&self, uri: String) {
        let window = self.obj().root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(&uri).launch(
            window.as_ref(),
            gio::Cancellable::NONE,
            move |result| {
                if let Err(error) = result {
                    report_error(OpenError::Launch(uri, error.to_string()));
                }
            },
        );
    }

    fn open_relative(&self, link: &str, fragment: Option<&str>) -> Result<(), OpenError> {
        let Some(document) = self.path.borrow().clone() else {
            return Ok(());
        };
        let is_repository = |folder: &Path| folder.join(".git").exists();
        match links::resolve(&document, link, fragment, &is_repository) {
            Opening::Viewer { path, fragment } if same_file(&path, &document) => {
                let Some(name) = fragment else {
                    return Ok(());
                };
                let leaf = self
                    .document
                    .borrow()
                    .as_ref()
                    .and_then(|doc| super::anchor_leaf(&doc.anchors, &name));
                let leaf = leaf.ok_or(OpenError::NoAnchor(name))?;
                self.reveal_leaf(leaf);
                Ok(())
            }
            Opening::Viewer { path, fragment } => open_in_viewer(&path, fragment.as_deref()),
            Opening::System(path) => {
                let metadata = std::fs::metadata(&path)
                    .map_err(|error| OpenError::Read(path.clone(), error.kind()))?;
                if !links::safe_to_open(&path, &metadata) {
                    return Err(OpenError::Program(path));
                }
                let window = self.obj().root().and_downcast::<gtk::Window>();
                let file = gio::File::for_path(&path);
                let content_type = if metadata.is_dir() {
                    "inode/directory".into()
                } else {
                    gio::content_type_guess(Some(&path), None).0
                };
                let launcher = gtk::FileLauncher::new(Some(&file));
                launcher.set_always_ask(links::launch(&content_type) == links::Launch::Ask);
                launcher.launch(window.as_ref(), gio::Cancellable::NONE, move |result| {
                    if let Err(error) = result {
                        report_error(OpenError::Launch(
                            path.display().to_string(),
                            error.to_string(),
                        ));
                    }
                });
                Ok(())
            }
        }
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

/// What a click at a point does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pointed {
    Link(Target),
    /// An image that is not a link: its tooltip says how its loading went.
    Image(ImageId),
    /// The summary of this `<details>`: opens or closes it.
    Summary(DetailsId),
}

/// What is at a point of a leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Hit {
    Link(Target),
    Image(ImageId),
}

/// An image's tooltip: why it is not shown, if it is not.
fn image_note(gallery: &Gallery, image: ImageId) -> Option<String> {
    let alt = gallery
        .table()
        .info(image)
        .map(|info| info.alt.as_str())
        .filter(|alt| !alt.is_empty())
        .unwrap_or("image");
    match gallery.state(image)? {
        ImageState::Ready(_) => None,
        ImageState::Waiting | ImageState::Wanted | ImageState::Loading { .. } => {
            Some(format!("{alt}: loading"))
        }
        ImageState::Failed(error) => Some(format!("{alt}: {error}")),
        ImageState::Offline => Some(format!("{alt}: not fetched (--offline)")),
        ImageState::NoSource => Some(format!("{alt}: nothing to load it from")),
    }
}

/// How far a click may reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Everywhere,
    /// Only this document: the probe's --click-at never opens a browser or another file.
    Document,
}

/// Why a link could not be followed.
#[derive(Debug, thiserror::Error)]
enum OpenError {
    #[error("no heading or anchor named {0:?} in this document")]
    NoAnchor(String),
    #[error("cannot open {0}: {1:?}")]
    Read(PathBuf, std::io::ErrorKind),
    #[error("not opening {0}: it is a program or launcher")]
    Program(PathBuf),
    #[error("cannot find this viewer to open {0}: {1:?}")]
    NoViewer(PathBuf, std::io::ErrorKind),
    #[error("cannot open {0}: {1}")]
    Launch(String, String),
}

/// Each table's pieces.
fn table_pieces(paints: &[LeafPaint]) -> HashMap<TableId, Vec<usize>> {
    let mut tables: HashMap<TableId, Vec<usize>> = HashMap::new();
    for (index, paint) in paints.iter().enumerate() {
        if let Some(table) = paint.table.as_ref() {
            tables.entry(table.columns.table).or_default().push(index);
        }
    }
    tables
}

/// What a link's tooltip says: where it goes, when that is not this document. A relative path
/// is shown resolved as a click resolves it.
fn describe(target: &Target, document: Option<&Path>) -> Option<String> {
    match target {
        Target::Leaf(_) => None,
        Target::Missing(name) => Some(format!("#{name} (not in this document)")),
        Target::Uri(uri) => Some(uri.clone()),
        Target::Relative { path, fragment } => {
            let is_repository = |folder: &Path| folder.join(".git").exists();
            let shown = match document
                .map(|document| links::resolve(document, path, fragment.as_deref(), &is_repository))
            {
                Some(Opening::Viewer { path, .. } | Opening::System(path)) => path,
                None => PathBuf::from(path),
            };
            Some(match fragment {
                Some(fragment) => format!("{}#{fragment}", shown.display()),
                None => shown.display().to_string(),
            })
        }
    }
}

/// Whether two paths name the same file.
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Opens a Markdown file in a new viewer window, at `fragment` if given.
fn open_in_viewer(path: &Path, fragment: Option<&str>) -> Result<(), OpenError> {
    let viewer = std::env::current_exe()
        .map_err(|error| OpenError::NoViewer(path.to_path_buf(), error.kind()))?;
    let mut argv: Vec<std::ffi::OsString> = vec![viewer.into_os_string()];
    if let Some(fragment) = fragment {
        argv.push(format!("--anchor={fragment}").into());
    }
    argv.push("--".into());
    argv.push(path.as_os_str().to_owned());
    let argv: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsString::as_os_str).collect();
    gio::Subprocess::newv(&argv, gio::SubprocessFlags::NONE)
        .map(drop)
        .map_err(|error| OpenError::Launch(path.display().to_string(), error.to_string()))
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
            images: Arc::clone(self.gallery.table()),
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

    /// What is at `(x, y)` in paint `leaf` (leaf coordinates): a link, else an image; laying
    /// the paint out if it is not the one last asked about. Paints with neither are not laid out.
    fn hit_in(&mut self, context: &pango::Context, leaf: usize, (x, y): (i64, i64)) -> Option<Hit> {
        let paints = Arc::clone(&self.paints);
        let paint = paints
            .get(leaf)
            .filter(|paint| paint.has_links() || !paint.images.is_empty())?;
        let typesetter = self.typesetter(context.clone());
        let probe = Rc::clone(&self.probe);
        let mut counted = |bytes: usize| probe.layout_created(bytes);
        if self.hit.as_ref().is_none_or(|(at, _, _)| *at != leaf) {
            let measured = typesetter.measure(paint, self.width, &mut self.columns, &mut counted);
            let geometry =
                self.setup
                    .metrics()
                    .geometry(paint, measured.height, measured.grid(), self.width);
            self.hit = Some((leaf, measured, geometry));
        }
        let (_leaf, measured, geometry) = self.hit.as_ref()?;
        typesetter
            .link_at(paint, measured, geometry, (x, y), &mut counted)
            .map(Hit::Link)
            .or_else(|| Typesetter::image_at(paint, measured, geometry, (x, y)).map(Hit::Image))
    }

    /// The scroll value that puts a paint's top at the top of the view. It is then the anchor,
    /// so it stays there while heights above and below it become exact.
    fn top_of(&self, paint: usize) -> i64 {
        self.positions.start(paint) + self.margin()
    }

    /// The paints to lay out again when `images` arrive: those showing them, and every piece
    /// of a table whose column sample shows one.
    fn affected_by(&self, images: &[ImageId]) -> Vec<usize> {
        let mut paints = Vec::new();
        for &image in images {
            for &paint in self.gallery.paints_of(image) {
                paints.push(paint);
                let sample = self
                    .paints
                    .get(paint)
                    .and_then(|paint| paint.table.as_ref())
                    .map(|table| &table.columns);
                if let Some(sample) = sample
                    && sample.images.contains(&image)
                {
                    paints.extend(self.tables.get(&sample.table).into_iter().flatten());
                }
            }
        }
        paints
    }

    /// Whether a paint is shown: every `<details>` it is inside is open.
    fn shown(&self, paint: &LeafPaint) -> bool {
        self.closed_around_paint(paint).next().is_none()
    }

    /// The closed `<details>` a paint is inside.
    fn closed_around_paint<'a>(
        &'a self,
        paint: &'a LeafPaint,
    ) -> impl Iterator<Item = DetailsId> + 'a {
        paint
            .details
            .iter()
            .copied()
            .filter(|id| self.open.get(id.index()) == Some(&false))
    }

    fn closed_around(&self, paint: usize) -> Vec<DetailsId> {
        self.paints
            .get(paint)
            .map(|paint| self.closed_around_paint(paint).collect())
            .unwrap_or_default()
    }

    /// The height a paint takes now: none in a closed `<details>`, else its laid-out height,
    /// or an estimate until it has one.
    fn shown_height(&self, index: usize, paint: &LeafPaint) -> i64 {
        if !self.shown(paint) {
            return 0;
        }
        self.natural
            .get(index)
            .copied()
            .flatten()
            .unwrap_or_else(|| self.setup.metrics().estimate(paint, self.width))
    }

    /// Keeps a paint's laid-out height; returns the height it takes in the document (none if
    /// it is in a closed `<details>`).
    fn record_height(&mut self, leaf: usize, height: i64) -> i64 {
        if let Some(slot) = self.natural.get_mut(leaf) {
            *slot = Some(height);
        }
        let shown = self.paints.get(leaf).is_none_or(|paint| self.shown(paint));
        if shown { height } else { 0 }
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
        let paints = Arc::clone(&self.paints);
        let Some(paint) = paints.get(leaf) else {
            return 0;
        };
        if !self.shown(paint) {
            // In a closed `<details>`: nothing to draw, and no height.
            self.cache.insert(leaf, None);
            return 0;
        }
        // A summary's marker points down while its details are open, right while closed.
        let disclosure = if paint
            .summary_of
            .is_some_and(|id| self.open.get(id.index()) == Some(&false))
        {
            CLOSED_MARKER
        } else {
            OPEN_MARKER
        };
        let probe = Rc::clone(&self.probe);
        let mut counted = |bytes: usize| probe.layout_created(bytes);
        let measured = typesetter.measure(paint, self.width, &mut self.columns, &mut counted);
        let geometry =
            self.setup
                .metrics()
                .geometry(paint, measured.height, measured.grid(), self.width);
        let node = draw_leaf(
            &Drawing {
                typesetter: &typesetter,
                gallery: &self.gallery,
                paint,
                measured: &measured,
                geometry: &geometry,
                disclosure,
            },
            &mut counted,
        );
        self.cache.insert(leaf, node);
        let height = self.record_height(leaf, geometry.height);
        self.positions.set(leaf, height, anchor, scrolling)
    }
}

/// Draws one leaf at the origin, as its geometry places it.
/// What drawing one leaf needs.
struct Drawing<'a> {
    typesetter: &'a Typesetter,
    gallery: &'a Gallery,
    paint: &'a LeafPaint,
    measured: &'a Measured,
    geometry: &'a LeafGeometry,
    /// The summary marker to show, if the leaf has one.
    disclosure: &'a str,
}

fn draw_leaf(drawing: &Drawing<'_>, counted: &mut dyn FnMut(usize)) -> Option<gsk::RenderNode> {
    let Drawing {
        typesetter,
        gallery,
        paint,
        measured,
        geometry,
        disclosure,
    } = *drawing;
    let palette = typesetter.palette;
    let snapshot = gtk::Snapshot::new();
    for (rect, shade) in &geometry.fills {
        let color = match shade {
            Shade::CodeBackground => palette.faint(0.06),
            Shade::Bar(BarColor::Quote) => palette.faint(0.25),
            Shade::Bar(BarColor::Alert(kind)) => palette.alert(*kind),
            Shade::Rule | Shade::TableLine => palette.faint(0.2),
            Shade::HeadingRule => palette.faint(0.15),
            Shade::TableStripe => palette.faint(0.04),
        };
        snapshot.append_color(&color, &graphene_rect(*rect));
    }
    let places = geometry.markers.iter().map(
        |MarkerPlace {
             text, right, top, ..
         }| (text.as_str(), *right, *top),
    );
    let disclosed = geometry
        .disclosure
        .map(|(right, top)| (disclosure, right, top));
    for (text, right, top) in places.chain(disclosed) {
        let marker = typesetter.marker(text, counted);
        let marker_width = i64::from(marker.pixel_size().0);
        append_layout(
            &snapshot,
            &marker,
            (right - marker_width, top),
            palette.foreground,
        );
    }
    match &measured.content {
        Content::Text(layout) => {
            let origin = (geometry.text_x, geometry.text_y);
            append_layout(&snapshot, layout, origin, palette.foreground);
            draw_images(
                &snapshot,
                typesetter,
                gallery,
                layout,
                &paint.attrs,
                origin,
                counted,
            );
        }
        Content::Table { cells, .. } => {
            let rows = paint
                .table
                .as_ref()
                .map(|table| table.rows.as_slice())
                .unwrap_or_default();
            for ((layouts, origins), row) in cells.iter().zip(&geometry.cells).zip(rows) {
                for ((layout, &origin), cell) in layouts.iter().zip(origins).zip(&row.cells) {
                    if let Some(layout) = layout {
                        append_layout(&snapshot, layout, origin, palette.foreground);
                        draw_images(
                            &snapshot,
                            typesetter,
                            gallery,
                            layout,
                            &cell.attrs,
                            origin,
                            counted,
                        );
                    }
                }
            }
        }
    }
    snapshot.to_node()
}

/// Draws a layout's images, or their placeholders, in the boxes Pango kept for them.
fn draw_images(
    snapshot: &gtk::Snapshot,
    typesetter: &Typesetter,
    gallery: &Gallery,
    layout: &pango::Layout,
    attrs: &[TextAttr],
    (x, y): (i64, i64),
    counted: &mut dyn FnMut(usize),
) {
    for (image, place) in text::image_boxes(layout, attrs) {
        let place = Rect {
            x: place.x + x,
            y: place.y + y,
            ..place
        };
        match gallery.state(image) {
            Some(ImageState::Ready(texture)) => {
                snapshot.append_texture(texture, &graphene_rect(place));
            }
            state => {
                let alt = gallery
                    .table()
                    .info(image)
                    .map_or("", |info| info.alt.as_str());
                let failed = matches!(state, Some(ImageState::Failed(_)));
                draw_placeholder(snapshot, typesetter, place, alt, failed, counted);
            }
        }
    }
}

/// An image not (yet) shown: a faint box with its alt text, marked if it failed to load.
fn draw_placeholder(
    snapshot: &gtk::Snapshot,
    typesetter: &Typesetter,
    place: Rect,
    alt: &str,
    failed: bool,
    counted: &mut dyn FnMut(usize),
) {
    let palette = typesetter.palette;
    snapshot.append_color(&palette.faint(0.05), &graphene_rect(place));
    let line = palette.faint(0.25);
    for edge in [
        Rect { height: 1, ..place },
        Rect {
            y: place.y + place.height - 1,
            height: 1,
            ..place
        },
        Rect { width: 1, ..place },
        Rect {
            x: place.x + place.width - 1,
            width: 1,
            ..place
        },
    ] {
        snapshot.append_color(&line, &graphene_rect(edge));
    }
    let text = match (failed, alt.is_empty()) {
        (true, true) => "⚠".to_owned(),
        (true, false) => format!("⚠ {alt}"),
        (false, _) => alt.to_owned(),
    };
    if text.is_empty() || place.width < 8 || place.height < 8 {
        return;
    }
    let pad = 4;
    let caption = typesetter.caption(&text, place.width - 2 * pad, counted);
    let caption_height = i64::from(caption.pixel_size().1);
    snapshot.push_clip(&graphene_rect(place));
    append_layout(
        snapshot,
        &caption,
        (place.x + pad, place.y + (place.height - caption_height) / 2),
        palette.faint(0.6),
    );
    snapshot.pop();
}

fn append_layout(
    snapshot: &gtk::Snapshot,
    layout: &pango::Layout,
    (x, y): (i64, i64),
    color: gtk::gdk::RGBA,
) {
    snapshot.save();
    snapshot.translate(&point(x, y));
    snapshot.append_layout(layout, &color);
    snapshot.restore();
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
