// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Entering a grouped stream and surviving its losses: a reader whose messages
//! only decode from the start of a group — an encoded video link, where a
//! group opens at a keyframe — admits nothing until a group starts, and after a
//! message reports a loss discards to the next group start.
//!
//! The message reporting the loss counts when it starts a group itself: the
//! loss sits before it, and a group start decodes on its own.

use crate::iceoryx2::MessageGroupBoundary;

/// What a reader does with one arriving message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageGroupLossRecoveryDisposition {
    /// Unbroken since this reader entered the stream: read it.
    Read,
    /// The group start this reader enters the stream at, first or after a
    /// loss: drop the state the broken stream left, then read it.
    EnterAtThisGroupStart,
    /// Before a group start: the message depends on one this reader never saw.
    DiscardUntilTheNextGroupStart,
}

/// Admits a grouped stream's messages from a group start, and back to one
/// after every loss.
#[derive(Debug, Default)]
pub struct MessageGroupLossRecoveryGate {
    inside_an_entered_group: bool,
    messages_lost_on_the_link: u64,
    messages_discarded_awaiting_a_group_start: u64,
    group_starts_entered_at: u64,
}

impl MessageGroupLossRecoveryGate {
    /// Decide one message from its group flag and the loss its read reported.
    pub fn admit_message(
        &mut self,
        group_boundary: MessageGroupBoundary,
        lost_since_last_read: u64,
    ) -> MessageGroupLossRecoveryDisposition {
        if lost_since_last_read > 0 {
            self.messages_lost_on_the_link += lost_since_last_read;
            self.inside_an_entered_group = false;
        }
        match (self.inside_an_entered_group, group_boundary) {
            (true, _) => MessageGroupLossRecoveryDisposition::Read,
            (false, MessageGroupBoundary::StartsGroup) => {
                self.inside_an_entered_group = true;
                self.group_starts_entered_at += 1;
                MessageGroupLossRecoveryDisposition::EnterAtThisGroupStart
            }
            (false, MessageGroupBoundary::ContinuesGroup) => {
                self.messages_discarded_awaiting_a_group_start += 1;
                MessageGroupLossRecoveryDisposition::DiscardUntilTheNextGroupStart
            }
        }
    }

    /// Give up the group the reader is in: a message it admitted could not be
    /// read or decoded, so what follows in that group references something the
    /// reader never had. Messages discard until the next group start.
    pub fn discard_the_rest_of_the_current_group(&mut self) {
        self.inside_an_entered_group = false;
    }

    /// Messages the link reported lost, summed over every read.
    pub fn messages_lost_on_the_link(&self) -> u64 {
        self.messages_lost_on_the_link
    }

    /// Messages that arrived but were discarded waiting for a group start.
    pub fn messages_discarded_awaiting_a_group_start(&self) -> u64 {
        self.messages_discarded_awaiting_a_group_start
    }

    /// Times the reader entered the stream at a group start, the first entry
    /// included.
    pub fn group_starts_entered_at(&self) -> u64 {
        self.group_starts_entered_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MessageGroupBoundary::{ContinuesGroup, StartsGroup};
    use MessageGroupLossRecoveryDisposition::{
        DiscardUntilTheNextGroupStart, EnterAtThisGroupStart, Read,
    };

    #[test]
    fn a_reader_attaching_mid_group_discards_until_the_first_group_start() {
        let mut gate = MessageGroupLossRecoveryGate::default();
        assert_eq!(
            gate.admit_message(ContinuesGroup, 0),
            DiscardUntilTheNextGroupStart
        );
        assert_eq!(
            gate.admit_message(ContinuesGroup, 0),
            DiscardUntilTheNextGroupStart
        );
        assert_eq!(gate.admit_message(StartsGroup, 0), EnterAtThisGroupStart);
        assert_eq!(gate.admit_message(ContinuesGroup, 0), Read);
        assert_eq!(gate.messages_discarded_awaiting_a_group_start(), 2);
        assert_eq!(gate.group_starts_entered_at(), 1);
    }

    #[test]
    fn a_loss_mid_group_discards_the_rest_of_it_and_re_enters_at_the_next_start() {
        let mut gate = MessageGroupLossRecoveryGate::default();
        assert_eq!(gate.admit_message(StartsGroup, 0), EnterAtThisGroupStart);
        assert_eq!(gate.admit_message(ContinuesGroup, 0), Read);
        assert_eq!(
            gate.admit_message(ContinuesGroup, 3),
            DiscardUntilTheNextGroupStart
        );
        assert_eq!(
            gate.admit_message(ContinuesGroup, 0),
            DiscardUntilTheNextGroupStart
        );
        assert_eq!(gate.admit_message(StartsGroup, 0), EnterAtThisGroupStart);
        assert_eq!(gate.admit_message(ContinuesGroup, 0), Read);
        assert_eq!(gate.messages_lost_on_the_link(), 3);
        assert_eq!(gate.messages_discarded_awaiting_a_group_start(), 2);
        assert_eq!(gate.group_starts_entered_at(), 2);
    }

    /// The loss sits before the group start reporting it, so that start is
    /// entered at rather than discarded with a whole group for nothing.
    #[test]
    fn a_loss_reported_on_a_group_start_re_enters_at_that_very_message() {
        let mut gate = MessageGroupLossRecoveryGate::default();
        assert_eq!(gate.admit_message(StartsGroup, 0), EnterAtThisGroupStart);
        assert_eq!(gate.admit_message(ContinuesGroup, 0), Read);
        assert_eq!(gate.admit_message(StartsGroup, 5), EnterAtThisGroupStart);
        assert_eq!(gate.admit_message(ContinuesGroup, 0), Read);
        assert_eq!(gate.messages_lost_on_the_link(), 5);
        assert_eq!(gate.messages_discarded_awaiting_a_group_start(), 0);
    }

    /// A group start in unbroken flow is read, not re-entered: re-entering
    /// would flush a decoder at every keyframe.
    #[test]
    fn a_group_start_in_unbroken_flow_is_read_rather_than_re_entered() {
        let mut gate = MessageGroupLossRecoveryGate::default();
        assert_eq!(gate.admit_message(StartsGroup, 0), EnterAtThisGroupStart);
        assert_eq!(gate.admit_message(ContinuesGroup, 0), Read);
        assert_eq!(gate.admit_message(StartsGroup, 0), Read);
        assert_eq!(gate.group_starts_entered_at(), 1);
    }

    /// An audio link starts a group on every message, so a loss costs nothing
    /// but the lost messages themselves.
    #[test]
    fn a_stream_of_group_starts_loses_only_what_the_link_lost() {
        let mut gate = MessageGroupLossRecoveryGate::default();
        let dispositions: Vec<_> = [0, 0, 2, 0]
            .into_iter()
            .map(|lost_since_last_read| gate.admit_message(StartsGroup, lost_since_last_read))
            .collect();
        assert_eq!(
            dispositions,
            [EnterAtThisGroupStart, Read, EnterAtThisGroupStart, Read]
        );
        assert_eq!(gate.messages_lost_on_the_link(), 2);
        assert_eq!(gate.messages_discarded_awaiting_a_group_start(), 0);
    }

    /// An admitted group start the reader could not decode leaves nothing for
    /// the group's continuations to reference.
    #[test]
    fn a_group_given_up_discards_its_continuations_until_the_next_start() {
        let mut gate = MessageGroupLossRecoveryGate::default();
        assert_eq!(gate.admit_message(StartsGroup, 0), EnterAtThisGroupStart);
        gate.discard_the_rest_of_the_current_group();
        assert_eq!(
            gate.admit_message(ContinuesGroup, 0),
            DiscardUntilTheNextGroupStart
        );
        assert_eq!(gate.admit_message(StartsGroup, 0), EnterAtThisGroupStart);
        assert_eq!(gate.admit_message(ContinuesGroup, 0), Read);
    }
}
