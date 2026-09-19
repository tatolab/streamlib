// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Reading loss off a producer's own numbering.
//!
//! Every bag carries a number its producer gave it, so a jump in what a reader
//! receives names exactly the bags that never reached it. Two readers ask this:
//! a channel subscriber, whose ring overwrites under pressure, and a remote
//! link's ingress, whose numbers arrive from another runtime. They differ only
//! in what identifies one unbroken run of numbering — the publishing port's id
//! locally, the generation the sending runtime carried across the mesh.

/// The last number one reader received, and the run it belonged to.
#[derive(Clone, Copy)]
struct TheLastNumberReceived<OneUnbrokenRun> {
    run: OneUnbrokenRun,
    sequence_number: u64,
}

/// What a reader remembers so a jump in the numbering reads as the bags it lost.
///
/// One slot rather than one per run: a run's numbers arrive in the order they
/// were sent, so a number from any other run is a new baseline either way.
pub struct BagsAGapInTheNumberingSaysWereLost<OneUnbrokenRun> {
    last: Option<TheLastNumberReceived<OneUnbrokenRun>>,
}

impl<OneUnbrokenRun> Default for BagsAGapInTheNumberingSaysWereLost<OneUnbrokenRun> {
    fn default() -> Self {
        Self { last: None }
    }
}

impl<OneUnbrokenRun: Copy + PartialEq> BagsAGapInTheNumberingSaysWereLost<OneUnbrokenRun> {
    /// How many bags were lost before the one numbered `sequence_number` in
    /// `run`, remembering it as the last received.
    ///
    /// The first bag of all, and the first of a run this reader has not seen,
    /// is a baseline and never a gap: a producer numbers its own sends from
    /// zero, so a replaced one says nothing about the one before it. A number
    /// that does not advance — a duplicate, or one arriving behind its
    /// successor — is no loss either, rather than the enormous one an unsigned
    /// subtraction below zero would report.
    pub fn how_many_were_lost_before(&mut self, run: OneUnbrokenRun, sequence_number: u64) -> u64 {
        let lost = match self.last {
            Some(last) if last.run == run => sequence_number
                .saturating_sub(last.sequence_number)
                .saturating_sub(1),
            _ => 0,
        };
        self.last = Some(TheLastNumberReceived {
            run,
            sequence_number,
        });
        lost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a run of numbers, each with the run it belonged to, says was lost.
    fn what_a_run_of(numbered: &[(u64, u64)]) -> Vec<u64> {
        let mut lost = BagsAGapInTheNumberingSaysWereLost::default();
        numbered
            .iter()
            .map(|(sequence_number, run)| lost.how_many_were_lost_before(*run, *sequence_number))
            .collect()
    }

    /// An unbroken run loses nothing, and the first number of all is a baseline
    /// rather than everything the producer sent before this reader existed.
    #[test]
    fn an_unbroken_run_loses_nothing_and_its_first_number_is_a_baseline() {
        assert_eq!(what_a_run_of(&[(500, 0), (501, 0), (502, 0)]), [0, 0, 0]);
    }

    /// A jump names exactly the bags between the two that arrived — the
    /// arithmetic every loss count built on a sequence number rests on.
    #[test]
    fn a_jump_counts_exactly_the_bags_between_the_two_that_arrived() {
        assert_eq!(
            what_a_run_of(&[(0, 0), (1, 0), (5, 0), (6, 0), (100, 0)]),
            [0, 0, 3, 0, 93]
        );
    }

    /// A new run is a baseline even once its numbering has overtaken the run
    /// before it, which is the case the run identity exists for: without it the
    /// jump from 4 to 7 reads as two lost bags that were never sent.
    #[test]
    fn a_new_run_is_a_baseline_even_once_its_numbering_has_overtaken() {
        assert_eq!(
            what_a_run_of(&[(3, 0), (4, 0), (7, 1), (8, 1)]),
            [0, 0, 0, 0]
        );
        assert_eq!(
            what_a_run_of(&[(3, 0), (4, 0), (0, 1), (1, 1)]),
            [0, 0, 0, 0]
        );
    }

    /// A gap inside the new run still counts: a new run resets the baseline, it
    /// does not stop the counting.
    #[test]
    fn a_gap_after_a_new_run_begins_is_still_counted() {
        assert_eq!(what_a_run_of(&[(9, 0), (0, 1), (4, 1)]), [0, 0, 3]);
    }

    /// A number that does not advance reads as no loss.
    #[test]
    fn a_number_that_does_not_advance_reads_as_no_loss() {
        assert_eq!(
            what_a_run_of(&[(7, 0), (7, 0), (3, 0), (4, 0)]),
            [0, 0, 0, 0]
        );
    }

    /// The counting survives the numbering wrapping: a number is a `u64` that
    /// wraps, and the wrap must not read as the whole range lost.
    #[test]
    fn the_numbering_wrapping_is_not_read_as_the_whole_range_lost() {
        assert_eq!(
            what_a_run_of(&[(u64::MAX - 1, 0), (u64::MAX, 0), (0, 0), (1, 0)]),
            [0, 0, 0, 0]
        );
    }
}
