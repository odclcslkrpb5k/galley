//! Leaf heights and where each leaf starts, in O(log n) (docs/plan.md §4.5).
//!
//! Heights are whole logical pixels, so sums are exact however many updates are applied (a
//! float tree would drift). Positions are i64: a document of a million leaves at a few thousand
//! pixels each is far inside its range.

#[derive(Debug, Clone, Default)]
pub(crate) struct Fenwick {
    /// 1-based Fenwick tree over `heights`.
    tree: Vec<i64>,
    heights: Vec<i64>,
}

impl Fenwick {
    pub(crate) fn new(heights: Vec<i64>) -> Self {
        let mut tree = vec![0; heights.len() + 1];
        for (index, &height) in heights.iter().enumerate() {
            let mut node = index + 1;
            while let Some(slot) = tree.get_mut(node) {
                *slot += height;
                node += node.isolate_lowest_one();
            }
        }
        Self { tree, heights }
    }

    pub(crate) fn len(&self) -> usize {
        self.heights.len()
    }

    pub(crate) fn height(&self, leaf: usize) -> Option<i64> {
        self.heights.get(leaf).copied()
    }

    /// Sets a leaf's height; returns the change, or `None` if there is no such leaf.
    pub(crate) fn set(&mut self, leaf: usize, height: i64) -> Option<i64> {
        let old = self.heights.get_mut(leaf)?;
        let delta = height - *old;
        *old = height;
        if delta != 0 {
            let mut node = leaf + 1;
            while let Some(slot) = self.tree.get_mut(node) {
                *slot += delta;
                node += node.isolate_lowest_one();
            }
        }
        Some(delta)
    }

    /// Where `leaf` starts: the sum of the heights before it. `start(len)` is the total.
    pub(crate) fn start(&self, leaf: usize) -> i64 {
        let mut node = leaf.min(self.heights.len());
        let mut sum = 0;
        while node > 0 {
            sum += self.tree.get(node).copied().unwrap_or(0);
            node &= node - 1;
        }
        sum
    }

    pub(crate) fn total(&self) -> i64 {
        self.start(self.heights.len())
    }

    /// The leaf containing position `y`: the last leaf starting at or before it. `None` for an
    /// empty tree; positions past the end give the last leaf, before the start the first.
    pub(crate) fn leaf_at(&self, y: i64) -> Option<usize> {
        if self.heights.is_empty() {
            return None;
        }
        // Binary lifting: the largest prefix whose sum is <= y.
        let mut index = 0;
        let mut remaining = y;
        let mut step = self.heights.len().bit_width();
        while step > 0 {
            step -= 1;
            let next = index + (1 << step);
            if let Some(&sum) = self.tree.get(next)
                && sum <= remaining
            {
                index = next;
                remaining -= sum;
            }
        }
        Some(index.min(self.heights.len() - 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Fenwick {
        // Leaves at [0,10), [10,30), [30,60), [60,100).
        Fenwick::new(vec![10, 20, 30, 40])
    }

    #[test]
    fn starts_and_total() {
        let tree = tree();
        assert_eq!(tree.len(), 4);
        let starts: Vec<i64> = (0..=4).map(|leaf| tree.start(leaf)).collect();
        assert_eq!(starts, vec![0, 10, 30, 60, 100]);
        assert_eq!(tree.total(), 100);
        assert_eq!(tree.start(9), 100);
        assert_eq!(tree.height(2), Some(30));
        assert_eq!(tree.height(4), None);
    }

    #[test]
    fn leaf_at_positions() {
        let tree = tree();
        let found: Vec<Option<usize>> = [-5, 0, 9, 10, 29, 30, 59, 60, 99, 100, 1000]
            .iter()
            .map(|&y| tree.leaf_at(y))
            .collect();
        assert_eq!(
            found,
            vec![
                Some(0),
                Some(0),
                Some(0),
                Some(1),
                Some(1),
                Some(2),
                Some(2),
                Some(3),
                Some(3),
                Some(3),
                Some(3)
            ]
        );
        assert_eq!(Fenwick::default().leaf_at(0), None);
    }

    #[test]
    fn updates_move_later_leaves() {
        let mut tree = tree();
        assert_eq!(tree.set(1, 25), Some(5));
        assert_eq!(tree.start(2), 35);
        assert_eq!(tree.total(), 105);
        assert_eq!(tree.leaf_at(34), Some(1));
        assert_eq!(tree.leaf_at(35), Some(2));
        assert_eq!(tree.set(7, 1), None);
        assert_eq!(tree.set(0, 10), Some(0));
    }

    #[test]
    fn zero_heights_are_skipped_by_leaf_at() {
        let tree = Fenwick::new(vec![0, 10, 0, 0, 10]);
        assert_eq!(tree.leaf_at(0), Some(1));
        assert_eq!(tree.leaf_at(10), Some(4));
    }

    #[test]
    fn matches_a_naive_sum_on_many_leaves() {
        let heights: Vec<i64> = (0..1000).map(|i| (i * 37) % 101).collect();
        let mut tree = Fenwick::new(heights.clone());
        let mut naive = heights;
        for i in (0..1000).step_by(7) {
            let height = i64::try_from(i).expect("small") * 13 % 97;
            tree.set(i, height);
            if let Some(slot) = naive.get_mut(i) {
                *slot = height;
            }
        }
        let mut sum = 0;
        for (leaf, height) in naive.iter().enumerate() {
            assert_eq!(tree.start(leaf), sum, "start of {leaf}");
            sum += height;
        }
        for y in (0..sum).step_by(53) {
            let leaf = tree.leaf_at(y).expect("leaf");
            assert!(tree.start(leaf) <= y && (y < tree.start(leaf + 1) || leaf == 999));
        }
    }
}
