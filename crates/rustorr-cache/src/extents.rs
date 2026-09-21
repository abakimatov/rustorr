use std::{collections::BTreeMap, ops::Bound};

use rustorr_domain::ByteRange;

/// Set of byte ranges that have been stored, kept as disjoint, non-adjacent
/// spans. It is what lets a store refuse to read bytes it never received.
#[derive(Debug, Default)]
pub(crate) struct Extents {
    /// start -> end, half-open.
    spans: BTreeMap<u64, u64>,
    total: u64,
}

impl Extents {
    /// Adds `range` and returns how many bytes were not covered before.
    pub fn insert(&mut self, range: ByteRange) -> u64 {
        if range.is_empty() {
            return 0;
        }
        let mut start = range.start();
        let mut end = range.end();
        let mut absorbed = 0;
        let mut absorbed_starts = Vec::new();

        // A span that begins earlier but reaches into, or touches, the range.
        let predecessor = self
            .spans
            .range(..=range.start())
            .next_back()
            .map(|(&s, &e)| (s, e));
        if let Some((s, e)) = predecessor.filter(|&(_, e)| e >= range.start()) {
            absorbed_starts.push(s);
            absorbed += e - s;
            start = s;
            end = end.max(e);
        }
        // Spans that begin inside the range, or exactly where it ends.
        let following = (Bound::Excluded(range.start()), Bound::Included(range.end()));
        for (&s, &e) in self.spans.range(following) {
            absorbed_starts.push(s);
            absorbed += e - s;
            end = end.max(e);
        }

        for s in absorbed_starts {
            self.spans.remove(&s);
        }
        self.spans.insert(start, end);
        let added = (end - start) - absorbed;
        self.total += added;
        added
    }

    /// Whether every byte of `range` is covered. An empty range always is.
    pub fn covers(&self, range: ByteRange) -> bool {
        if range.is_empty() {
            return true;
        }
        self.spans
            .range(..=range.start())
            .next_back()
            .is_some_and(|(_, &end)| end >= range.end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: u64, end: u64) -> ByteRange {
        ByteRange::new(start, end).unwrap()
    }

    #[test]
    fn overlapping_and_adjacent_ranges_merge_into_one_span() {
        let mut extents = Extents::default();
        assert_eq!(extents.insert(range(10, 20)), 10);
        assert_eq!(extents.insert(range(15, 30)), 10);
        assert_eq!(extents.insert(range(30, 35)), 5);
        assert_eq!(extents.insert(range(0, 10)), 10);
        assert_eq!(extents.spans.len(), 1);
        assert_eq!(extents.total, 35);
        assert!(extents.covers(range(0, 35)));
    }

    #[test]
    fn a_gap_is_not_covered() {
        let mut extents = Extents::default();
        extents.insert(range(0, 10));
        extents.insert(range(20, 30));
        assert!(extents.covers(range(2, 8)));
        assert!(!extents.covers(range(5, 25)));
        assert!(!extents.covers(range(10, 20)));
        assert!(!extents.covers(range(30, 31)));
    }

    #[test]
    fn rewriting_stored_bytes_adds_nothing() {
        let mut extents = Extents::default();
        extents.insert(range(0, 100));
        assert_eq!(extents.insert(range(20, 40)), 0);
        assert_eq!(extents.total, 100);
    }

    #[test]
    fn one_range_can_bridge_several_spans() {
        let mut extents = Extents::default();
        extents.insert(range(0, 5));
        extents.insert(range(10, 15));
        extents.insert(range(20, 25));
        assert_eq!(extents.insert(range(3, 22)), 10);
        assert_eq!(extents.spans.len(), 1);
        assert!(extents.covers(range(0, 25)));
    }

    #[test]
    fn empty_ranges_change_nothing_and_are_always_covered() {
        let mut extents = Extents::default();
        assert_eq!(extents.insert(range(7, 7)), 0);
        assert!(extents.spans.is_empty());
        assert!(extents.covers(range(50, 50)));
    }

    /// Compares against a plain bitmap over random operations, checking the
    /// return value, the total, coverage queries and the span invariants.
    #[test]
    fn matches_a_bitmap_model() {
        const SIZE: u64 = 96;
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move |bound: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % bound
        };

        for _ in 0..200 {
            let mut extents = Extents::default();
            let mut bitmap = [false; SIZE as usize];
            for _ in 0..30 {
                let start = next(SIZE);
                let end = start + next(SIZE - start + 1);
                let newly = (start..end).filter(|&i| !bitmap[i as usize]).count() as u64;
                (start..end).for_each(|i| bitmap[i as usize] = true);

                assert_eq!(extents.insert(range(start, end)), newly);
                assert_eq!(extents.total, bitmap.iter().filter(|&&b| b).count() as u64);

                let probe_start = next(SIZE);
                let probe_end = probe_start + next(SIZE - probe_start + 1);
                let expected = (probe_start..probe_end).all(|i| bitmap[i as usize]);
                assert_eq!(extents.covers(range(probe_start, probe_end)), expected);

                let mut previous_end = None;
                for (&s, &e) in &extents.spans {
                    assert!(s < e);
                    assert!(previous_end.is_none_or(|p| p < s), "spans touch or overlap");
                    previous_end = Some(e);
                }
            }
        }
    }
}
