// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The ordering pair every encoded bag carries.
//!
//! Spelled here rather than imported: an extension links no engine crate, and
//! this is the producer half of a contract the engine's consumers read. The
//! semantics are the engine's own — `sequence_index` never resets, so a step
//! other than exactly one is loss and never a restart.

/// `(group_index, sequence_index)` for one published bag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedStreamOrderingPair {
    pub group_index: u64,
    pub sequence_index: u64,
}

/// Per-producer counter for the pair.
#[derive(Debug, Default)]
pub struct EncodedStreamOrderingPairCounter {
    bags_accounted: u64,
    current_group_index: u64,
}

impl EncodedStreamOrderingPairCounter {
    /// Account one published bag, handing back the pair it carries.
    pub fn account_published_bag(&mut self, is_sync_point: bool) -> EncodedStreamOrderingPair {
        if is_sync_point && self.bags_accounted > 0 {
            self.current_group_index += 1;
        }
        let pair = EncodedStreamOrderingPair {
            group_index: self.current_group_index,
            sequence_index: self.bags_accounted,
        };
        self.bags_accounted += 1;
        pair
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_bag_opens_group_zero_whether_or_not_it_is_a_sync_point() {
        for opens_with_a_sync_point in [true, false] {
            let mut counter = EncodedStreamOrderingPairCounter::default();
            let first = counter.account_published_bag(opens_with_a_sync_point);
            assert_eq!(first.group_index, 0);
            assert_eq!(first.sequence_index, 0);
        }
    }

    #[test]
    fn the_sequence_index_never_resets_at_a_group_boundary() {
        let mut counter = EncodedStreamOrderingPairCounter::default();

        let pairs: Vec<_> = [true, false, false, true, false]
            .into_iter()
            .map(|is_sync_point| counter.account_published_bag(is_sync_point))
            .collect();

        assert_eq!(
            pairs.iter().map(|pair| pair.sequence_index).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(
            pairs.iter().map(|pair| pair.group_index).collect::<Vec<_>>(),
            vec![0, 0, 0, 1, 1]
        );
    }

    #[test]
    fn a_stream_of_nothing_but_sync_points_makes_the_two_indices_equal() {
        // Which is the shape every Opus packet takes: each is its own group.
        let mut counter = EncodedStreamOrderingPairCounter::default();

        for _ in 0..4 {
            let pair = counter.account_published_bag(true);
            assert_eq!(pair.group_index, pair.sequence_index);
        }
    }
}
