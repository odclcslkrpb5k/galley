//! The copy menu (docs/plan.md §4.5 "Copying"), drawn by the view like its copy buttons and
//! notices: a theme's popover style is not ours to rely on (one drew it in dotted bevels, and
//! scrolled two items), and a list this short needs nothing a popover adds. Sized to its items,
//! kept inside the view; the pointer or the keys choose, and anything else closes it.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};

use super::DocumentView;
use crate::render::text::Palette;
use crate::units;

/// Space inside the menu, around and between items, in em.
const PAD_X_EM: f64 = 0.9;
const PAD_Y_EM: f64 = 0.3;
const ITEM_PAD_EM: f64 = 0.3;
const SEPARATOR_EM: f64 = 0.6;
const RADIUS_EM: f64 = 0.4;
/// The narrowest a menu is, in em.
const MIN_WIDTH_EM: f64 = 10.0;

/// An open menu: its items laid out (when it opened, never in a frame) and where it is.
#[derive(Debug)]
pub(in crate::document_view) struct Menu {
    layouts: Vec<pango::Layout>,
    /// The menu's box in the view.
    bounds: graphene::Rect,
    /// Each item's top (from the box's) and height.
    rows: Vec<(f32, f32)>,
    /// The line above the last item (the whole document), from the box's top.
    separator: Option<f32>,
    pad_x: f32,
    item_pad: f32,
    radius: f32,
    highlighted: Option<usize>,
}

/// Where a point is, for an open menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Outside,
    /// In the menu, between items or on its edge.
    Inside,
    Item(usize),
}

impl Menu {
    /// A menu of `layouts` (the last after a line, if `separated`) opening at `(x, y)` in a
    /// view `width` by `height`: down and right from there, or up and left where that would
    /// leave the view.
    fn new(
        layouts: Vec<pango::Layout>,
        separated: bool,
        em: f64,
        at: (f64, f64),
        view: (f64, f64),
    ) -> Self {
        let sizes: Vec<(i32, i32)> = layouts.iter().map(pango::Layout::pixel_size).collect();
        Self {
            layouts,
            ..Self::arranged(&sizes, separated, em, at, view)
        }
    }

    /// Where a menu of items whose text is `sizes` (pixels) goes, with no text yet.
    fn arranged(
        sizes: &[(i32, i32)],
        separated: bool,
        em: f64,
        (x, y): (f64, f64),
        (width, height): (f64, f64),
    ) -> Self {
        let em = units::to_f32(em);
        let (pad_x, pad_y, item_pad) = (
            em * units::to_f32(PAD_X_EM),
            em * units::to_f32(PAD_Y_EM),
            em * units::to_f32(ITEM_PAD_EM),
        );
        let mut rows = Vec::with_capacity(sizes.len());
        let mut separator = None;
        let mut top = pad_y;
        let mut widest = em * units::to_f32(MIN_WIDTH_EM) - 2.0 * pad_x;
        let last = sizes.len().saturating_sub(1);
        for (index, &(text_width, text_height)) in sizes.iter().enumerate() {
            if separated && index == last && index > 0 {
                let space = em * units::to_f32(SEPARATOR_EM);
                separator = Some(top + space / 2.0);
                top += space;
            }

            widest = widest.max(units::to_f32(f64::from(text_width)));
            let row = units::to_f32(f64::from(text_height)) + 2.0 * item_pad;
            rows.push((top, row));
            top += row;
        }
        let (menu_width, menu_height) = (widest + 2.0 * pad_x, top + pad_y);
        let (view_width, view_height) = (units::to_f32(width), units::to_f32(height));
        let (x, y) = (units::to_f32(x), units::to_f32(y));
        let left = if x + menu_width <= view_width {
            x
        } else {
            (x - menu_width).max(0.0)
        };
        let top = if y + menu_height <= view_height {
            y
        } else {
            (y - menu_height).max(0.0)
        };
        Self {
            layouts: Vec::new(),
            bounds: graphene::Rect::new(left, top, menu_width, menu_height),
            rows,
            separator,
            pad_x,
            item_pad,
            radius: em * units::to_f32(RADIUS_EM),
            highlighted: None,
        }
    }

    fn place(&self, x: f64, y: f64) -> Place {
        let (x, y) = (units::to_f32(x), units::to_f32(y));
        if !self.bounds.contains_point(&graphene::Point::new(x, y)) {
            return Place::Outside;
        }
        let y = y - self.bounds.y();
        self.rows
            .iter()
            .position(|&(top, height)| (top..top + height).contains(&y))
            .map_or(Place::Inside, Place::Item)
    }

    /// Moves the highlight `step` items, wrapping; from none, Down goes to the first and Up to
    /// the last.
    fn step(&mut self, step: isize) {
        let count = self.rows.len();
        if count == 0 {
            return;
        }
        let next = match self.highlighted {
            None if step > 0 => 0,
            None => count - 1,
            Some(at) => (at + count).saturating_add_signed(step) % count,
        };
        self.highlighted = Some(next);
    }
}

impl DocumentView {
    /// Opens the menu of `labels` at a point of the view; the last label is set apart when
    /// `separated`.
    pub(in crate::document_view) fn open_menu(
        &self,
        labels: &[&str],
        separated: bool,
        at: (f64, f64),
    ) {
        if labels.is_empty() {
            self.close_menu();
            return;
        }
        let widget = self.obj();
        let context = widget.pango_context();
        let size = (f64::from(widget.width()), f64::from(widget.height()));
        let menu = {
            let document = self.document.borrow();
            let Some(doc) = document.as_ref() else {
                return;
            };
            let probe = std::rc::Rc::clone(&doc.probe);
            let typesetter = doc.typesetter(context);
            let layouts = labels
                .iter()
                .map(|label| {
                    typesetter.caption(label, i64::MAX / 4, &mut |bytes| {
                        probe.layout_created(bytes);
                    })
                })
                .collect();
            Menu::new(layouts, separated, doc.setup.metrics().em_px, at, size)
        };
        self.copying.borrow_mut().menu = Some(menu);
        widget.set_cursor_from_name(None);
        widget.set_tooltip_text(None);
        widget.queue_draw();
    }

    pub(in crate::document_view) fn menu_open(&self) -> bool {
        self.copying.borrow().menu.is_some()
    }

    pub(in crate::document_view) fn close_menu(&self) {
        if self.copying.borrow_mut().menu.take().is_some() {
            self.obj().queue_draw();
        }
    }

    /// The pointer moved: the item under it is highlighted. Whether the menu is open (and so
    /// took the motion).
    pub(in crate::document_view) fn menu_hover(&self, at: Option<(f64, f64)>) -> bool {
        let mut copying = self.copying.borrow_mut();
        let Some(menu) = copying.menu.as_mut() else {
            return false;
        };
        let item = at.and_then(|(x, y)| match menu.place(x, y) {
            Place::Item(item) => Some(item),
            Place::Inside | Place::Outside => None,
        });
        // Leaving the menu keeps what the keys chose.
        if item.is_some() && menu.highlighted != item {
            menu.highlighted = item;
            drop(copying);
            self.obj().queue_draw();
        }
        true
    }

    /// A button went down: inside the menu it waits for the release; outside it closes the
    /// menu, and that click does nothing else (a right-click opens the menu there instead).
    /// Whether the press was the menu's.
    pub(in crate::document_view) fn menu_press(&self, button: u32, x: f64, y: f64) -> bool {
        let place = self
            .copying
            .borrow()
            .menu
            .as_ref()
            .map(|menu| menu.place(x, y));
        match place {
            None => false,
            Some(Place::Item(_) | Place::Inside) => true,
            Some(Place::Outside) => {
                self.close_menu();
                self.swallow_release.set(true);
                if button == gdk::BUTTON_SECONDARY {
                    self.show_menu(x, y);
                }
                true
            }
        }
    }

    /// A button came up: on an item, that item is chosen. Whether the release was the menu's.
    pub(in crate::document_view) fn menu_release(&self, x: f64, y: f64) -> bool {
        if self.swallow_release.replace(false) {
            return true;
        }
        let place = self
            .copying
            .borrow()
            .menu
            .as_ref()
            .map(|menu| menu.place(x, y));
        match place {
            None => false,
            Some(Place::Item(item)) => {
                self.choose(item);
                true
            }
            Some(Place::Inside | Place::Outside) => true,
        }
    }

    /// A key, while the menu is open: Up, Down, Home and End move, Return and space choose,
    /// Escape closes; any other key closes it and does what it does. `None` when no menu is
    /// open.
    pub(in crate::document_view) fn menu_key(&self, key: gdk::Key) -> Option<glib::Propagation> {
        if !self.menu_open() {
            return None;
        }
        let moved = |step: isize| {
            if let Some(menu) = self.copying.borrow_mut().menu.as_mut() {
                menu.step(step);
            }
            self.obj().queue_draw();
            Some(glib::Propagation::Stop)
        };
        match key {
            gdk::Key::Down | gdk::Key::KP_Down | gdk::Key::Tab => moved(1),
            gdk::Key::Up | gdk::Key::KP_Up | gdk::Key::ISO_Left_Tab => moved(-1),
            gdk::Key::Home | gdk::Key::KP_Home => {
                if let Some(menu) = self.copying.borrow_mut().menu.as_mut() {
                    menu.highlighted = None;
                }
                moved(1)
            }
            gdk::Key::End | gdk::Key::KP_End => {
                if let Some(menu) = self.copying.borrow_mut().menu.as_mut() {
                    menu.highlighted = None;
                }
                moved(-1)
            }
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter | gdk::Key::space => {
                let chosen = self
                    .copying
                    .borrow()
                    .menu
                    .as_ref()
                    .and_then(|menu| menu.highlighted);
                match chosen {
                    Some(item) => self.choose(item),
                    None => self.close_menu(),
                }
                Some(glib::Propagation::Stop)
            }
            gdk::Key::Escape => {
                self.close_menu();
                Some(glib::Propagation::Stop)
            }
            // Modifiers alone change nothing yet.
            gdk::Key::Shift_L
            | gdk::Key::Shift_R
            | gdk::Key::Control_L
            | gdk::Key::Control_R
            | gdk::Key::Alt_L
            | gdk::Key::Alt_R => Some(glib::Propagation::Stop),
            _ => {
                self.close_menu();
                None
            }
        }
    }

    /// The menu's item `item` is chosen: it copies, and the menu closes.
    fn choose(&self, item: usize) {
        self.close_menu();
        self.copy_item(item);
    }

    /// For the probe: the centre of the open menu's item `item`, in the view.
    pub(in crate::document_view) fn menu_item_centre(&self, item: usize) -> Option<(i64, i64)> {
        let copying = self.copying.borrow();
        let menu = copying.menu.as_ref()?;
        let &(top, height) = menu.rows.get(item)?;
        Some((
            units::round_px(f64::from(menu.bounds.x() + menu.bounds.width() / 2.0)),
            units::round_px(f64::from(menu.bounds.y() + top + height / 2.0)),
        ))
    }

    /// Draws the open menu: a box of the colour opposite the text's, with a shadow; the
    /// highlighted item tinted with the link colour.
    pub(in crate::document_view) fn draw_menu(&self, snapshot: &gtk::Snapshot, palette: Palette) {
        let copying = self.copying.borrow();
        let Some(menu) = copying.menu.as_ref() else {
            return;
        };
        let bounds = menu.bounds;
        let rounded = gsk::RoundedRect::from_rect(bounds, menu.radius);
        snapshot.append_outset_shadow(
            &rounded,
            &gdk::RGBA::new(0.0, 0.0, 0.0, 0.3),
            0.0,
            menu.radius / 2.0,
            0.0,
            menu.radius * 2.0,
        );
        snapshot.push_rounded_clip(&rounded);
        snapshot.append_color(&palette.inverse(), &bounds);
        if let Some(&(top, height)) = menu.highlighted.and_then(|item| menu.rows.get(item)) {
            let inset = menu.item_pad;
            let row = graphene::Rect::new(
                bounds.x() + inset,
                bounds.y() + top,
                bounds.width() - 2.0 * inset,
                height,
            );
            snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(row, menu.radius / 2.0));
            snapshot.append_color(&palette.link.with_alpha(0.18), &row);
            snapshot.pop();
        }
        if let Some(line) = menu.separator {
            snapshot.append_color(
                &palette.faint(0.15),
                &graphene::Rect::new(
                    bounds.x() + menu.pad_x / 2.0,
                    bounds.y() + line,
                    bounds.width() - menu.pad_x,
                    1.0,
                ),
            );
        }
        for (layout, &(top, _height)) in menu.layouts.iter().zip(&menu.rows) {
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                bounds.x() + menu.pad_x,
                bounds.y() + top + menu.item_pad,
            ));
            snapshot.append_layout(layout, &palette.foreground);
            snapshot.restore();
        }
        snapshot.pop();
        let border = palette.faint(0.25);
        snapshot.append_border(&rounded, &[1.0; 4], &[border, border, border, border]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn menu(count: usize, separated: bool, at: (f64, f64), view: (f64, f64)) -> Menu {
        // "Copy paragraph" at 16 px, about.
        Menu::arranged(&vec![(110, 19); count], separated, 16.0, at, view)
    }

    #[test]
    fn a_menu_stays_inside_the_view() {
        let opened = menu(3, true, (100.0, 100.0), (800.0, 600.0));
        assert_eq!((opened.bounds.x(), opened.bounds.y()), (100.0, 100.0));
        // Near the right and bottom edges: up and left from the point instead.
        let flipped = menu(3, true, (790.0, 590.0), (800.0, 600.0));
        assert!(flipped.bounds.x() + flipped.bounds.width() <= 790.0);
        assert!(flipped.bounds.y() + flipped.bounds.height() <= 590.0);
        // Every item fits: nothing to scroll.
        let (top, height) = *opened.rows.last().expect("rows");
        assert!(top + height < opened.bounds.height());
        assert!(opened.separator.is_some());
    }

    #[test]
    fn places_and_steps() {
        let mut opened = menu(3, true, (100.0, 100.0), (800.0, 600.0));
        assert_eq!(opened.place(50.0, 50.0), Place::Outside);
        let (top, height) = opened.rows[1];
        let y = 100.0 + f64::from(top + height / 2.0);
        assert_eq!(opened.place(150.0, y), Place::Item(1));
        // Down from none is the first; Up wraps to the last.
        opened.step(1);
        assert_eq!(opened.highlighted, Some(0));
        opened.step(-1);
        assert_eq!(opened.highlighted, Some(2));
        opened.step(1);
        assert_eq!(opened.highlighted, Some(0));
        let mut fresh = menu(2, false, (0.0, 0.0), (800.0, 600.0));
        fresh.step(-1);
        assert_eq!(fresh.highlighted, Some(1));
        assert!(fresh.separator.is_none());
    }
}
