//! The scroll indicator's geometry (docs/plan.md §4.5). Pure, so it is unit-tested.
//!
//! The view draws its own indicator instead of using `GtkScrollbar`: moving a scrollbar's
//! slider queues an allocation, which in GTK 4 makes the window present itself again, and on X11
//! that costs about 16 synchronous X round trips per scroll frame (M2 review: dropped frames
//! went from 25-131 to 0-8 without the scrollbar). Drawing a rectangle costs nothing.

/// Width of the indicator's hit area at the right edge, in logical pixels.
pub(crate) const HIT_WIDTH: f64 = 16.0;
/// Width drawn, idle and while hovered or dragged.
pub(crate) const WIDTH: f64 = 6.0;
pub(crate) const ACTIVE_WIDTH: f64 = 10.0;
/// Space between the indicator and the view's edges.
pub(crate) const INSET: f64 = 2.0;
/// The thumb's least length, so it stays easy to grab in long documents.
const MIN_LENGTH: f64 = 32.0;

/// The scroll position, as `GtkAdjustment` has it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scroll {
    pub(crate) value: f64,
    pub(crate) page: f64,
    pub(crate) upper: f64,
}

/// The thumb: its top and length, in view pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Thumb {
    pub(crate) top: f64,
    pub(crate) length: f64,
}

impl Scroll {
    fn range(self) -> f64 {
        self.upper - self.page
    }

    fn track(self) -> f64 {
        (self.page - 2.0 * INSET).max(0.0)
    }

    /// The thumb, or `None` when everything fits and there is nothing to scroll.
    pub(crate) fn thumb(self) -> Option<Thumb> {
        if self.range() <= 0.0 || self.track() <= 0.0 {
            return None;
        }
        let track = self.track();
        let length = (track * self.page / self.upper).max(MIN_LENGTH).min(track);
        let fraction = (self.value / self.range()).clamp(0.0, 1.0);
        Some(Thumb {
            top: INSET + fraction * (track - length),
            length,
        })
    }

    /// The scroll value after dragging the thumb `dy` pixels from where it was at `start`.
    pub(crate) fn dragged(self, start: f64, dy: f64) -> f64 {
        let Some(thumb) = self.thumb() else {
            return start;
        };
        let travel = self.track() - thumb.length;
        if travel <= 0.0 {
            return start;
        }
        (start + dy * self.range() / travel).clamp(0.0, self.range())
    }

    /// The scroll value that centres the thumb on `y`: a click in the track jumps there.
    pub(crate) fn centred_on(self, y: f64) -> f64 {
        let Some(thumb) = self.thumb() else {
            return self.value;
        };
        let start = Self { value: 0.0, ..self };
        start.dragged(0.0, y - INSET - thumb.length / 2.0)
    }

    /// Whether `y` falls on the thumb.
    pub(crate) fn on_thumb(self, y: f64) -> bool {
        self.thumb()
            .is_some_and(|thumb| y >= thumb.top && y <= thumb.top + thumb.length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCROLL: Scroll = Scroll {
        value: 0.0,
        page: 1004.0,
        upper: 10_040.0,
    };

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn the_thumb_shows_the_page_in_the_document() {
        let thumb = SCROLL.thumb().expect("scrollable");
        assert!(close(thumb.top, INSET));
        assert!(close(thumb.length, 100.0));
        let end = Scroll {
            value: 9036.0,
            ..SCROLL
        }
        .thumb()
        .expect("scrollable");
        assert!(close(end.top + end.length, 1004.0 - INSET));
    }

    #[test]
    fn short_documents_have_no_thumb_and_long_ones_a_least_length() {
        let fits = Scroll {
            upper: 1004.0,
            ..SCROLL
        };
        assert_eq!(fits.thumb(), None);
        assert!(close(fits.dragged(5.0, 100.0), 5.0));
        assert!(close(fits.centred_on(500.0), fits.value));
        let long = Scroll {
            upper: 10_000_000.0,
            ..SCROLL
        };
        assert!(close(long.thumb().expect("scrollable").length, MIN_LENGTH));
    }

    #[test]
    fn dragging_moves_in_proportion_and_stops_at_the_ends() {
        // The thumb travels 1000 - 100 = 900 px over 9036 px of scrolling.
        assert!(close(SCROLL.dragged(0.0, 90.0), 903.6));
        assert!(close(SCROLL.dragged(0.0, -50.0), 0.0));
        assert!(close(SCROLL.dragged(9000.0, 500.0), 9036.0));
    }

    #[test]
    fn a_click_centres_the_thumb() {
        let value = SCROLL.centred_on(502.0);
        let thumb = Scroll { value, ..SCROLL }.thumb().expect("scrollable");
        assert!(close(thumb.top + thumb.length / 2.0, 502.0));
        assert!(SCROLL.on_thumb(50.0));
        assert!(!SCROLL.on_thumb(500.0));
    }
}
