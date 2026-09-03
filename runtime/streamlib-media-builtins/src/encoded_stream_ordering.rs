// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Ordering and loss-doctrine machinery shared by every encoded stream,
//! whatever the medium.
//!
//! An encoded link carries the ordering pair `group_index` /
//! `sequence_index` and a sync-point flag, and both halves of the loss
//! doctrine read only those: a producer accounts the pair it publishes, and
//! a consumer gates on the gaps that pair exposes. Neither reads a codec, a
//! bitstream or a payload, so one counter and one gate serve video's access
//! units and audio's packets alike.
//!
//! The vocabulary here is *bag*, not *frame*: Opus spends the word "frame"
//! on a subdivision of one packet, so a shared type that said "frame" would
//! mean two things at the one seam both media cross.

/// The ordering pair `(group_index, sequence_index)` an encoded bag carries,
/// accounted per published bag by its producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedStreamOrderingPair {
    /// Index of the sync-point-delimited group the bag opens or extends.
    pub group_index: u64,
    /// Publication-order index of the bag within the session.
    pub sequence_index: u64,
}

/// Per-producer counter for the ordering pair: a sync point after the first
/// bag opens the next group, and `sequence_index` never resets — the
/// property a consumer's gap detection rests on.
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

    /// How many bags this producer has published, for its progress and
    /// teardown lines.
    pub fn bags_accounted(&self) -> u64 {
        self.bags_accounted
    }
}

/// What the loss doctrine says to do with one arriving encoded bag, given
/// everything the gate has seen on its link before it.
///
/// `#[must_use]` because dropping it is a silent bug: a reader that ignores
/// the disposition decodes a bag the doctrine said to discard, and nothing
/// downstream can tell.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrivingEncodedBagDisposition {
    /// Feed it: it continues a stream whose continuity is intact.
    Decode,
    /// Reset the reader's state, then feed it: this bag is the sync point
    /// that re-enters a stream whose continuity was broken.
    ReEnterAtThisSyncPoint,
    /// Discard it: the stream's continuity is broken and this bag is not a
    /// re-entry point, so what it refers back to was never seen.
    DiscardUntilTheNextSyncPoint,
}

/// Per-link gate applying the decided loss doctrine to an encoded stream: a
/// consumer that sees a `sequence_index` gap discards until the producer's
/// next sync point, and never forwards a stream it knows is broken.
///
/// Consumer-side twin of [`EncodedStreamOrderingPairCounter`], and
/// medium-free for the same reason it is: both read only the convention's
/// own ordering fields, so every decoder of every codec shares one of each.
///
/// A gate opens broken — [`Default`] says so, because that is the invariant
/// and not a step a caller can forget. The first bag a subscriber receives is
/// not necessarily the first bag the producer published: an attach mid-group
/// hands over bags whose sync point is already gone, and feeding those is
/// exactly how a decoder ends a run having decoded nothing.
#[derive(Debug)]
pub struct EncodedStreamSyncPointGate {
    /// `None` until the first bag arrives; afterwards the newest
    /// `sequence_index` seen, decoded or discarded.
    newest_sequence_index_seen: Option<u64>,
    awaiting_a_sync_point: bool,
    bags_lost_to_gaps: u64,
    bags_discarded_awaiting_a_sync_point: u64,
    sync_points_entered_at: u64,
}

impl Default for EncodedStreamSyncPointGate {
    fn default() -> Self {
        Self::opening_at_the_next_sync_point()
    }
}

impl EncodedStreamSyncPointGate {
    /// Open a gate that has seen nothing and is therefore waiting for a sync
    /// point to enter the stream at.
    pub fn opening_at_the_next_sync_point() -> Self {
        Self {
            newest_sequence_index_seen: None,
            awaiting_a_sync_point: true,
            bags_lost_to_gaps: 0,
            bags_discarded_awaiting_a_sync_point: 0,
            sync_points_entered_at: 0,
        }
    }

    /// Admit one arriving bag, accounting the gap it exposes.
    pub fn admit(
        &mut self,
        sequence_index: u64,
        is_sync_point: bool,
    ) -> ArrivingEncodedBagDisposition {
        if let Some(newest_seen) = self.newest_sequence_index_seen
            && sequence_index.checked_sub(newest_seen) != Some(1)
        {
            // Any step other than exactly one breaks continuity: a forward
            // jump is loss, and a repeat or a step backwards is a producer
            // this reader's decode state cannot describe either way. The
            // indices come off the wire unchecked, so the arithmetic that
            // measures the gap must survive any pair of them.
            self.bags_lost_to_gaps = self
                .bags_lost_to_gaps
                .saturating_add(sequence_index.saturating_sub(newest_seen).saturating_sub(1));
            self.awaiting_a_sync_point = true;
        }
        self.newest_sequence_index_seen = Some(sequence_index);

        if !self.awaiting_a_sync_point {
            return ArrivingEncodedBagDisposition::Decode;
        }
        if is_sync_point {
            self.awaiting_a_sync_point = false;
            self.sync_points_entered_at += 1;
            return ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint;
        }
        self.bags_discarded_awaiting_a_sync_point += 1;
        ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint
    }

    /// Break continuity deliberately, so the next sync point is entered as a
    /// fresh stream. For a reader that has learned something the ordering
    /// pair cannot tell it — a producer that renegotiated its extent or its
    /// channel count, say.
    pub fn break_continuity(&mut self) {
        self.awaiting_a_sync_point = true;
    }

    /// How many bags the `sequence_index` gaps say the link lost.
    pub fn bags_lost_to_gaps(&self) -> u64 {
        self.bags_lost_to_gaps
    }

    /// How many arriving bags were discarded because they were not a
    /// re-entry point into a broken stream.
    pub fn bags_discarded_awaiting_a_sync_point(&self) -> u64 {
        self.bags_discarded_awaiting_a_sync_point
    }

    /// How many times the gate has entered the stream — once in a healthy
    /// run, once more per break.
    pub fn sync_points_entered_at(&self) -> u64 {
        self.sync_points_entered_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The #1077 shape, as logic: a subscriber that attaches mid-GOP is
    /// handed slices whose IDR is already gone. Feeding those is what ends
    /// a run at `frames_decoded = 0`; the gate discards them and enters at
    /// the producer's next sync point instead.
    #[test]
    fn a_stream_joined_mid_group_is_discarded_until_its_next_sync_point() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();

        assert_eq!(
            gate.admit(7, false),
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(8, false),
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(9, true),
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(gate.admit(10, false), ArrivingEncodedBagDisposition::Decode);
        assert_eq!(gate.bags_discarded_awaiting_a_sync_point(), 2);
        // Contiguous arrivals are not loss, however late the join was.
        assert_eq!(gate.bags_lost_to_gaps(), 0);
    }

    /// The decided loss doctrine: a `sequence_index` gap breaks the stream,
    /// and every frame until the producer's next sync point is discarded
    /// rather than decoded against reference frames that were never seen.
    #[test]
    fn a_sequence_index_gap_discards_until_the_next_sync_point() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(0, true),
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(gate.admit(1, false), ArrivingEncodedBagDisposition::Decode);

        // 2 and 3 were overwritten in the ring; 4 is a non-sync-point.
        assert_eq!(
            gate.admit(4, false),
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(5, false),
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(6, true),
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(gate.admit(7, false), ArrivingEncodedBagDisposition::Decode);

        assert_eq!(gate.bags_lost_to_gaps(), 2);
        assert_eq!(gate.bags_discarded_awaiting_a_sync_point(), 2);
    }

    /// A gap landing exactly on a sync point costs nothing but the gap: the
    /// sync point is itself the re-entry point, so nothing is discarded.
    #[test]
    fn a_gap_landing_on_a_sync_point_re_enters_without_discarding_anything() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(0, true),
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(30, true),
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(gate.bags_lost_to_gaps(), 29);
        assert_eq!(gate.bags_discarded_awaiting_a_sync_point(), 0);
    }

    /// `sequence_index` is monotonic for the life of a producer, so a repeat
    /// or a step backwards describes a stream this reader's decode state
    /// cannot continue — it re-enters rather than decoding on.
    #[test]
    fn a_sequence_index_that_does_not_advance_by_one_breaks_continuity_too() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(0, true),
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(gate.admit(1, false), ArrivingEncodedBagDisposition::Decode);
        assert_eq!(
            gate.admit(1, false),
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(0, false),
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint
        );
        // Neither backwards step is counted as frames lost — no frame went
        // missing, the producer's numbering stopped making sense.
        assert_eq!(gate.bags_lost_to_gaps(), 0);
        assert_eq!(gate.bags_discarded_awaiting_a_sync_point(), 2);
    }

    /// The invariant the type exists for, stated where a caller cannot skip
    /// it: a gate nobody configured is still waiting for a sync point.
    ///
    /// Mental revert: `#[derive(Default)]` on the gate. The default becomes
    /// permissive, a reader that never calls the named constructor admits
    /// whatever bag arrives first, and this is what notices.
    #[test]
    fn a_gate_nobody_configured_still_opens_at_the_next_sync_point() {
        let mut gate = EncodedStreamSyncPointGate::default();
        assert_eq!(
            gate.admit(41, false),
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(42, true),
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
        );
    }

    /// The indices come off the wire unchecked, so the gap arithmetic must
    /// survive any pair of them rather than overflowing on a hostile one.
    #[test]
    fn a_sequence_index_at_the_top_of_its_range_does_not_overflow_the_gap_arithmetic() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(u64::MAX, true),
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(
            gate.admit(u64::MAX, false),
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(gate.bags_lost_to_gaps(), 0);

        // A hostile producer alternating the extremes accumulates two
        // near-u64::MAX gaps; the tally saturates rather than wrapping or
        // panicking under overflow checks.
        let mut alternating = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        for hostile_index in [0, u64::MAX, 0, u64::MAX] {
            assert_eq!(
                alternating.admit(hostile_index, true),
                ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
            );
        }
        assert_eq!(alternating.bags_lost_to_gaps(), u64::MAX);
    }

    /// A reader that learned of a discontinuity the ordering pair cannot
    /// show it — a producer that renegotiated its extent — re-enters the
    /// same way a gap does.
    #[test]
    fn a_deliberately_broken_continuity_re_enters_at_the_next_sync_point() {
        let mut gate = EncodedStreamSyncPointGate::opening_at_the_next_sync_point();
        assert_eq!(
            gate.admit(0, true),
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(gate.admit(1, false), ArrivingEncodedBagDisposition::Decode);
        gate.break_continuity();
        assert_eq!(
            gate.admit(2, false),
            ArrivingEncodedBagDisposition::DiscardUntilTheNextSyncPoint
        );
        assert_eq!(
            gate.admit(3, true),
            ArrivingEncodedBagDisposition::ReEnterAtThisSyncPoint
        );
        assert_eq!(gate.sync_points_entered_at(), 2);
        // A break is not loss: nothing went missing on the wire.
        assert_eq!(gate.bags_lost_to_gaps(), 0);
    }

    /// The ordering pair a consumer's gap detection rests on: the sequence
    /// index never resets, and a sync point after the first frame opens the
    /// next group.
    #[test]
    fn a_sync_point_opens_the_next_group_and_the_sequence_never_resets() {
        let mut counter = EncodedStreamOrderingPairCounter::default();
        let published: Vec<EncodedStreamOrderingPair> = [true, false, false, true, false, true]
            .into_iter()
            .map(|is_sync_point| counter.account_published_bag(is_sync_point))
            .collect();

        let group_indices: Vec<u64> = published.iter().map(|pair| pair.group_index).collect();
        let sequence_indices: Vec<u64> = published.iter().map(|pair| pair.sequence_index).collect();
        assert_eq!(group_indices, vec![0, 0, 0, 1, 1, 2]);
        assert_eq!(sequence_indices, vec![0, 1, 2, 3, 4, 5]);
    }
}
