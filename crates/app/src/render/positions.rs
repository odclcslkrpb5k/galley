//! Where every leaf is, and how the view stays on the same content while heights change
//! (docs/plan.md §4.5). Pure: no GTK, so all of it is unit-tested.
//!
//! Heights start as estimates and become exact when a leaf is laid out (here, on the main
//! thread) or measured by the worker. All height changes go through `set`, which says how far
//! the scroll position must move so that the leaf at the top of the viewport (the anchor) stays
//! where it is. While the user is scrolling, changes above the anchor are held instead:
//! `GtkScrolledWindow`'s kinetic scrolling overwrites outside changes to the scroll position, so
//! a correction made mid-fling would be lost and the content would jump.
//!
//! Coordinates are document pixels: 0 is the top of the first leaf.

use std::ops::Range;

use super::fenwick::Fenwick;

#[derive(Debug, Clone)]
pub(crate) struct Positions {
    heights: Fenwick,
    /// Whether a leaf's height is measured, not estimated.
    exact: Vec<bool>,
    /// Exact heights for leaves above the anchor, held while scrolling.
    held: Vec<(usize, i64)>,
}

/// Where the top of the viewport is, as a leaf and the fraction of it above the top: kept across
/// a change of width, when every height changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Spot {
    leaf: usize,
    fraction: f64,
}

impl Positions {
    pub(crate) fn new(estimates: Vec<i64>) -> Self {
        let count = estimates.len();
        Self {
            heights: Fenwick::new(estimates),
            exact: vec![false; count],
            held: Vec::new(),
        }
    }

    pub(crate) fn total(&self) -> i64 {
        self.heights.total()
    }

    pub(crate) fn start(&self, leaf: usize) -> i64 {
        self.heights.start(leaf)
    }

    #[cfg(test)]
    fn height(&self, leaf: usize) -> Option<i64> {
        self.heights.height(leaf)
    }

    pub(crate) fn is_exact(&self, leaf: usize) -> bool {
        self.exact.get(leaf).copied().unwrap_or(true)
    }

    /// The leaf at the top of a viewport whose top is at `y`.
    pub(crate) fn anchor(&self, y: i64) -> Option<usize> {
        self.heights.leaf_at(y.max(0))
    }

    /// The leaves overlapping `[top, top + height)`.
    pub(crate) fn visible(&self, top: i64, height: i64) -> Range<usize> {
        let Some(first) = self.anchor(top) else {
            return 0..0;
        };
        let bottom = top + height;
        let mut end = first;
        while end < self.heights.len() && self.heights.start(end) < bottom {
            end += 1;
        }
        first..end
    }

    /// The leaves of `range` nearest the anchor first: from it downwards, then upwards.
    pub(crate) fn nearest_first(
        range: Range<usize>,
        anchor: Option<usize>,
    ) -> impl Iterator<Item = usize> {
        let split = anchor.map_or(range.start, |anchor| anchor.clamp(range.start, range.end));
        (split..range.end).chain((range.start..split).rev())
    }

    /// Sets a leaf's exact height. Returns how far the scroll position must move to keep the
    /// anchor still: the change, for a leaf above the anchor. While `scrolling`, such a change
    /// is held (and 0 returned) until `release`.
    pub(crate) fn set(
        &mut self,
        leaf: usize,
        height: i64,
        anchor: Option<usize>,
        scrolling: bool,
    ) -> i64 {
        if let Some(slot) = self.exact.get_mut(leaf) {
            *slot = true;
        }
        let above = anchor.is_some_and(|anchor| leaf < anchor);
        if above && scrolling {
            self.held.push((leaf, height));
            return 0;
        }
        let delta = self.heights.set(leaf, height).unwrap_or(0);
        if above { delta } else { 0 }
    }

    /// Like `set`, for a height from the worker: ignored if the leaf is already exact (laid out
    /// here, or already measured).
    pub(crate) fn set_measured(
        &mut self,
        leaf: usize,
        height: i64,
        anchor: Option<usize>,
        scrolling: bool,
    ) -> i64 {
        if self.is_exact(leaf) {
            return 0;
        }
        self.set(leaf, height, anchor, scrolling)
    }

    /// Applies held heights once scrolling has stopped; returns the scroll shift.
    pub(crate) fn release(&mut self, anchor: Option<usize>) -> i64 {
        std::mem::take(&mut self.held)
            .into_iter()
            .map(|(leaf, height)| self.set(leaf, height, anchor, false))
            .sum()
    }

    /// Applies held heights of leaves at or below the anchor: scrolling up brought them into
    /// view, so they must be drawn at their real size. No shift: they are not above the anchor.
    pub(crate) fn release_from(&mut self, anchor: Option<usize>) {
        let Some(anchor) = anchor else {
            return;
        };
        let (now, still): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|&(leaf, _)| leaf >= anchor);
        self.held = still;
        for (leaf, height) in now {
            self.heights.set(leaf, height);
        }
    }

    /// Where `y` is, relative to the leaf there; `None` at or above the document's top.
    pub(crate) fn spot(&self, y: i64) -> Option<Spot> {
        if y <= 0 {
            return None;
        }
        let leaf = self.heights.leaf_at(y)?;
        let height = self.heights.height(leaf)?.max(1);
        let into = y - self.heights.start(leaf);
        Some(Spot {
            leaf,
            fraction: crate::units::px_to_f64(into) / crate::units::px_to_f64(height),
        })
    }

    /// The `y` of a spot taken before heights changed.
    pub(crate) fn y_of(&self, spot: Spot) -> i64 {
        let height = self.heights.height(spot.leaf).unwrap_or(0);
        self.heights.start(spot.leaf)
            + crate::units::round_px(spot.fraction * crate::units::px_to_f64(height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five leaves of 100 px, the viewport's top 50 px into leaf 2.
    fn positions() -> Positions {
        Positions::new(vec![100; 5])
    }

    #[test]
    fn a_change_above_the_anchor_shifts_by_its_delta() {
        let mut positions = positions();
        let anchor = positions.anchor(250);
        assert_eq!(anchor, Some(2));
        assert_eq!(positions.set(0, 130, anchor, false), 30);
        assert_eq!(positions.set(1, 90, anchor, false), -10);
        assert_eq!(positions.start(2), 220);
        assert_eq!(positions.total(), 520);
    }

    #[test]
    fn a_change_at_or_below_the_anchor_does_not_shift() {
        let mut positions = positions();
        let anchor = Some(2);
        assert_eq!(positions.set(2, 300, anchor, false), 0);
        assert_eq!(positions.set(4, 10, anchor, true), 0);
        assert_eq!(positions.total(), 100 + 100 + 300 + 100 + 10);
        assert_eq!(positions.set(0, 150, None, false), 0);
    }

    #[test]
    fn changes_above_the_anchor_wait_while_scrolling() {
        let mut positions = positions();
        let anchor = Some(2);
        assert_eq!(positions.set(0, 150, anchor, true), 0);
        assert_eq!(positions.set(1, 50, anchor, true), 0);
        assert_eq!(positions.total(), 500);
        assert!(positions.is_exact(0));
        assert_eq!(positions.release(anchor), 0);
        assert_eq!(positions.total(), 500);
        assert_eq!(positions.height(0), Some(150));
        assert_eq!(positions.release(anchor), 0);
    }

    #[test]
    fn released_changes_shift_relative_to_the_anchor_then() {
        let mut positions = positions();
        positions.set(0, 160, Some(2), true);
        positions.set(3, 40, Some(4), true);
        // Scrolling stopped with leaf 1 at the top: only leaf 0 is above it now.
        assert_eq!(positions.release(Some(1)), 60);
        assert_eq!(positions.height(3), Some(40));
    }

    #[test]
    fn held_heights_come_into_view_without_a_shift() {
        let mut positions = positions();
        positions.set(0, 160, Some(3), true);
        positions.set(2, 40, Some(3), true);
        positions.release_from(Some(2));
        assert_eq!(positions.height(2), Some(40));
        assert_eq!(positions.height(0), Some(100));
        assert_eq!(positions.release(Some(2)), 60);
        positions.release_from(None);
    }

    #[test]
    fn worker_heights_do_not_replace_exact_ones() {
        let mut positions = positions();
        positions.set(1, 120, None, false);
        assert_eq!(positions.set_measured(1, 999, None, false), 0);
        assert_eq!(positions.height(1), Some(120));
        assert_eq!(positions.set_measured(0, 80, Some(2), false), -20);
        assert!(positions.is_exact(0));
        assert!(!positions.is_exact(4));
        assert!(positions.is_exact(99));
    }

    #[test]
    fn visible_ranges_cover_the_viewport() {
        let positions = positions();
        assert_eq!(positions.visible(250, 100), 2..4);
        assert_eq!(positions.visible(-50, 100), 0..1);
        assert_eq!(positions.visible(200, 100), 2..3);
        assert_eq!(positions.visible(450, 1000), 4..5);
        // Past the end: the last leaf (it is drawn off screen, harmlessly).
        assert_eq!(positions.visible(600, 100), 4..5);
        assert_eq!(Positions::new(Vec::new()).visible(0, 100), 0..0);
    }

    #[test]
    fn nearest_first_goes_down_from_the_anchor_then_up() {
        let order: Vec<_> = Positions::nearest_first(1..6, Some(3)).collect();
        assert_eq!(order, vec![3, 4, 5, 2, 1]);
        let order: Vec<_> = Positions::nearest_first(1..4, None).collect();
        assert_eq!(order, vec![1, 2, 3]);
        let order: Vec<_> = Positions::nearest_first(1..4, Some(9)).collect();
        assert_eq!(order, vec![3, 2, 1]);
    }

    #[test]
    fn a_spot_survives_new_heights() {
        let mut positions = positions();
        let spot = positions.spot(250).expect("inside leaf 2");
        positions = Positions::new(vec![50, 50, 200, 50, 50]);
        assert_eq!(positions.y_of(spot), 100 + 100);
        assert_eq!(positions.spot(0), None);
        assert_eq!(positions.spot(-5), None);
        let past = positions.spot(9999).expect("past the end");
        assert_eq!(positions.y_of(past), 9999);
        let end = positions.spot(399).expect("last leaf");
        assert_eq!(positions.y_of(end), 399);
    }
}
