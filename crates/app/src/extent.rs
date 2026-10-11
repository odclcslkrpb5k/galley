//! The document's vertical extent: each leaf's height, and where each leaf starts.
//!
//! Heights are logical pixels. M0 recomputes the starts on every change (O(n)); M2 replaces this
//! with a Fenwick tree (docs/plan.md §4.5).

use std::ops::Range;

#[derive(Debug, Clone, Default)]
pub(crate) struct Extent {
    heights: Vec<f64>,
    /// `starts[i]` is where leaf `i` starts; one more entry than leaves, the last being the total.
    starts: Vec<f64>,
}

impl Extent {
    pub(crate) fn new(heights: Vec<f64>) -> Self {
        let mut extent = Self {
            heights,
            starts: Vec::new(),
        };
        extent.recompute();
        extent
    }

    pub(crate) fn len(&self) -> usize {
        self.heights.len()
    }

    pub(crate) fn total(&self) -> f64 {
        self.starts.last().copied().unwrap_or(0.0)
    }

    pub(crate) fn start(&self, leaf: usize) -> Option<f64> {
        self.starts.get(leaf).copied()
    }

    /// Sets a leaf's height. Returns false if there is no such leaf.
    pub(crate) fn set_height(&mut self, leaf: usize, height: f64) -> bool {
        match self.heights.get_mut(leaf) {
            Some(slot) => {
                *slot = height;
                self.recompute();
                true
            }
            None => false,
        }
    }

    /// The leaves overlapping `[top, bottom)`.
    pub(crate) fn overlapping(&self, top: f64, bottom: f64) -> Range<usize> {
        let leaves = self.heights.len();
        let ends = self.starts.get(1..).unwrap_or_default();
        let leaf_starts = self.starts.get(..leaves).unwrap_or_default();
        let first = ends.partition_point(|&end| end <= top);
        let after_last = leaf_starts.partition_point(|&start| start < bottom);
        first..after_last.max(first)
    }

    fn recompute(&mut self) {
        self.starts.clear();
        self.starts.push(0.0);
        let mut sum = 0.0;
        for height in &self.heights {
            sum += height;
            self.starts.push(sum);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extent() -> Extent {
        // Leaves at [0,10), [10,30), [30,60).
        Extent::new(vec![10.0, 20.0, 30.0])
    }

    #[test]
    fn starts_and_total() {
        let extent = extent();
        assert_eq!(extent.len(), 3);
        assert_eq!(extent.start(0), Some(0.0));
        assert_eq!(extent.start(2), Some(30.0));
        assert_eq!(extent.start(3), Some(60.0));
        assert_eq!(extent.start(4), None);
        assert!((extent.total() - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn overlapping_ranges() {
        let extent = extent();
        assert_eq!(extent.overlapping(0.0, 10.0), 0..1);
        assert_eq!(extent.overlapping(0.0, 10.5), 0..2);
        assert_eq!(extent.overlapping(10.0, 30.0), 1..2);
        assert_eq!(extent.overlapping(9.9, 30.1), 0..3);
        assert_eq!(extent.overlapping(60.0, 100.0), 3..3);
        assert_eq!(extent.overlapping(-5.0, 0.0), 0..0);
    }

    #[test]
    fn set_height_moves_later_leaves() {
        let mut extent = extent();
        assert!(extent.set_height(0, 15.0));
        assert_eq!(extent.start(1), Some(15.0));
        assert!((extent.total() - 65.0).abs() < f64::EPSILON);
        assert!(!extent.set_height(3, 1.0));
    }

    #[test]
    fn empty_extent() {
        let extent = Extent::default();
        assert_eq!(extent.len(), 0);
        assert!(extent.total().abs() < f64::EPSILON);
        assert_eq!(extent.overlapping(0.0, 100.0), 0..0);
    }
}
