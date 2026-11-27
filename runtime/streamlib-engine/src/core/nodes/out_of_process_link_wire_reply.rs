// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What an out-of-process far side answers about a link it was told to wire.
//!
//! A link handed to a running far side is not wired when the engine sends it —
//! it is wired when the far side has opened its own port for it. `connect` does
//! not wait for that answer: it returns with the link pending, and
//! these cells are what `graph` reads afterwards.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use crate::core::error::PortDirection;

/// What one out-of-process far side answered about one link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutOfProcessLinkWireOutcome {
    /// The far side opened its port for the link.
    OpenedByTheFarSide,
    /// The far side could not open its port, and said why.
    RefusedByTheFarSide {
        /// The far side's own reason, rendered in `graph` until the link is
        /// disconnected.
        reason: String,
    },
}

/// One far side's answer for one link, shared between the compiler op that
/// handed the link over and the bridge reader thread that hears the answer.
///
/// A `OnceLock` rather than a settable cell, because first-answer-wins is the
/// contract and not a convention: a far side whose death is noticed after it
/// already answered must not rewrite the outcome the link has been reporting.
#[derive(Debug, Default)]
pub struct OutOfProcessLinkWireReply {
    answer: OnceLock<OutOfProcessLinkWireOutcome>,
}

impl OutOfProcessLinkWireReply {
    /// A cell for a link just handed to a far side, with nothing answered yet.
    pub fn awaiting_the_far_sides_answer() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Note what the far side answered. A second answer is dropped.
    pub fn note_the_far_sides_answer(&self, outcome: OutOfProcessLinkWireOutcome) {
        let _ = self.answer.set(outcome);
    }

    /// What the far side answered, or `None` while it still has not.
    pub fn the_far_sides_answer(&self) -> Option<OutOfProcessLinkWireOutcome> {
        self.answer.get().cloned()
    }

    /// Whether the far side answered that it refused the link.
    pub fn the_far_side_refused(&self) -> bool {
        matches!(
            self.answer.get(),
            Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide { .. })
        )
    }
}

/// The id the engine stamps on one command it sends an out-of-process far side
/// that expects an answer, and which the far side echoes on that answer.
///
/// Minted per bridge, so it names one command to one far side and never
/// repeats for the life of that bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct OutOfProcessCommandId(pub(crate) u64);

impl std::fmt::Display for OutOfProcessCommandId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What became of one far side's answer to a `wire_link` or `feed_island`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutOfProcessLinkWireAnswerNoted {
    /// It settled the attempt it answers.
    SettledItsAttempt,
    /// It answers an attempt a newer one for the same link end, or the link's
    /// unwiring, cancelled; it settles nothing.
    DroppedAsTheAnswerToACancelledAttempt,
    /// It answers a command this far side was never sent, or one already
    /// answered.
    DroppedAsTheAnswerToNoCommandAwaited,
    /// It names a link other than the one its command wired; it settles
    /// nothing.
    DroppedForNamingAnotherLink {
        /// The link the command it answers wired.
        link_id_the_command_wired: String,
    },
}

/// One `wire_link` or `feed_island` still waiting on its far side's answer.
#[derive(Debug)]
struct LinkWireAttemptAwaitingItsAnswer {
    link_id: String,
    port_direction: PortDirection,
    reply: Arc<OutOfProcessLinkWireReply>,
}

#[derive(Debug, Default)]
struct LinkWireAttemptsOfOneFarSide {
    awaiting_by_command_id: HashMap<OutOfProcessCommandId, LinkWireAttemptAwaitingItsAnswer>,
    /// The attempt each link end is waiting on, which a newer one replaces.
    latest_command_id_by_link_end: HashMap<(String, PortDirection), OutOfProcessCommandId>,
    /// Each attempt cancelled before its answer arrived, by the link it wired,
    /// held until that answer arrives so it is dropped by name. The far side
    /// answers every command it is sent, so each entry leaves with its answer.
    link_id_by_cancelled_command_id: HashMap<OutOfProcessCommandId, String>,
}

impl LinkWireAttemptsOfOneFarSide {
    fn cancel(&mut self, command_id: OutOfProcessCommandId) {
        if let Some(cancelled) = self.awaiting_by_command_id.remove(&command_id) {
            self.link_id_by_cancelled_command_id
                .insert(command_id, cancelled.link_id);
        }
    }
}

/// The `wire_link` and `feed_island` attempts one far side still owes an
/// answer for, matched to their answers by [`OutOfProcessCommandId`].
///
/// The last attempt wins, per link end: a newer attempt for the same link in
/// the same direction, or the link's unwiring, cancels the one before it, and
/// an answer to a cancelled attempt settles nothing. A link whose two ends are
/// both this far side waits on one attempt per end.
#[derive(Debug, Default)]
pub(crate) struct LinksAwaitingTheirOutOfProcessWireReply {
    attempts: Mutex<LinkWireAttemptsOfOneFarSide>,
}

impl LinksAwaitingTheirOutOfProcessWireReply {
    /// Start waiting on the answer to the attempt `command_id` that wires
    /// `link_id`'s end facing `port_direction`, cancelling that end's earlier
    /// attempt.
    pub(crate) fn await_the_answer_to_a_link_wire_attempt(
        &self,
        command_id: OutOfProcessCommandId,
        link_id: String,
        port_direction: PortDirection,
        reply: Arc<OutOfProcessLinkWireReply>,
    ) {
        let mut attempts = self.attempts.lock();
        if let Some(superseded) = attempts
            .latest_command_id_by_link_end
            .insert((link_id.clone(), port_direction), command_id)
        {
            attempts.cancel(superseded);
        }
        attempts.awaiting_by_command_id.insert(
            command_id,
            LinkWireAttemptAwaitingItsAnswer {
                link_id,
                port_direction,
                reply,
            },
        );
    }

    /// Note the far side's answer to the attempt `command_id`, which names
    /// `link_id`, saying what became of it so an answer that settles nothing
    /// is logged rather than dropped silently.
    pub(crate) fn note_the_far_sides_answer_to(
        &self,
        command_id: OutOfProcessCommandId,
        link_id: &str,
        outcome: OutOfProcessLinkWireOutcome,
    ) -> OutOfProcessLinkWireAnswerNoted {
        let reply = {
            let mut attempts = self.attempts.lock();
            if attempts
                .link_id_by_cancelled_command_id
                .remove(&command_id)
                .is_some()
            {
                return OutOfProcessLinkWireAnswerNoted::DroppedAsTheAnswerToACancelledAttempt;
            }
            let Entry::Occupied(awaiting) = attempts.awaiting_by_command_id.entry(command_id)
            else {
                return OutOfProcessLinkWireAnswerNoted::DroppedAsTheAnswerToNoCommandAwaited;
            };
            if awaiting.get().link_id != link_id {
                return OutOfProcessLinkWireAnswerNoted::DroppedForNamingAnotherLink {
                    link_id_the_command_wired: awaiting.get().link_id.clone(),
                };
            }
            let answered = awaiting.remove();
            attempts
                .latest_command_id_by_link_end
                .remove(&(answered.link_id, answered.port_direction));
            answered.reply
        };
        reply.note_the_far_sides_answer(outcome);
        OutOfProcessLinkWireAnswerNoted::SettledItsAttempt
    }

    /// Cancel every attempt still waiting to wire an end of a link that is
    /// going away, so a disconnect leaves nothing for its far side's answer to
    /// settle. Every end of it goes, since the whole link is being taken down.
    pub(crate) fn cancel_every_attempt_to_wire_link(&self, link_id: &str) {
        let mut attempts = self.attempts.lock();
        let ends_of_the_link: Vec<(String, PortDirection)> = attempts
            .latest_command_id_by_link_end
            .keys()
            .filter(|(end_link_id, _)| end_link_id == link_id)
            .cloned()
            .collect();
        for end in ends_of_the_link {
            if let Some(cancelled) = attempts.latest_command_id_by_link_end.remove(&end) {
                attempts.cancel(cancelled);
            }
        }
    }

    /// Refuse every attempt still waiting, because the far side is gone, and
    /// hand back how many there were so the caller can say so once. A link
    /// whose far side died unanswered never reads `wired`.
    pub(crate) fn refuse_every_link_still_awaiting_an_answer(&self, reason: &str) -> usize {
        let refused: Vec<Arc<OutOfProcessLinkWireReply>> = {
            let mut attempts = self.attempts.lock();
            attempts.latest_command_id_by_link_end.clear();
            attempts.link_id_by_cancelled_command_id.clear();
            attempts
                .awaiting_by_command_id
                .drain()
                .map(|(_, attempt)| attempt.reply)
                .collect()
        };
        for reply in &refused {
            reply.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                reason: reason.to_string(),
            });
        }
        refused.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(reason: &str) -> OutOfProcessLinkWireOutcome {
        OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
            reason: reason.to_string(),
        }
    }

    fn command(id: u64) -> OutOfProcessCommandId {
        OutOfProcessCommandId(id)
    }

    /// A board waiting on `link_id`'s input end under `command_id`, and that
    /// attempt's cell.
    fn awaiting_an_input_end(
        awaiting: &LinksAwaitingTheirOutOfProcessWireReply,
        command_id: u64,
        link_id: &str,
    ) -> Arc<OutOfProcessLinkWireReply> {
        let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        awaiting.await_the_answer_to_a_link_wire_attempt(
            command(command_id),
            link_id.to_string(),
            PortDirection::Input,
            Arc::clone(&reply),
        );
        reply
    }

    #[test]
    fn a_link_just_handed_over_has_no_answer_yet() {
        let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        assert_eq!(reply.the_far_sides_answer(), None);
    }

    #[test]
    fn the_first_answer_stands_against_a_later_one() {
        let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        reply.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        reply.note_the_far_sides_answer(refusal("its helper process died"));
        assert_eq!(
            reply.the_far_sides_answer(),
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide),
            "a death noticed after the far side answered must not rewrite the link's outcome"
        );
    }

    #[test]
    fn an_answer_reaches_the_attempt_its_id_names_and_no_other() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let answered = awaiting_an_input_end(&awaiting, 1, "L-answered");
        let untouched = awaiting_an_input_end(&awaiting, 2, "L-untouched");

        assert_eq!(
            awaiting.note_the_far_sides_answer_to(
                command(1),
                "L-answered",
                OutOfProcessLinkWireOutcome::OpenedByTheFarSide
            ),
            OutOfProcessLinkWireAnswerNoted::SettledItsAttempt
        );

        assert_eq!(
            answered.the_far_sides_answer(),
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide)
        );
        assert_eq!(untouched.the_far_sides_answer(), None);
        assert_eq!(
            awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died"),
            1,
            "an answered attempt is off the board, so only the untouched one is left to refuse"
        );
    }

    /// The race this board exists for: a link unwired and wired again before
    /// the far side's answer to the first attempt was read. The first answer
    /// arrives first and says the port opened; the second says it did not.
    ///
    /// Fail-without-fix: match answers by link id alone and the first answer
    /// settles the second attempt — the link reads `wired` though its new
    /// wiring failed — and the second answer, the true one, is dropped.
    #[test]
    fn a_link_wired_again_before_its_first_answer_reads_as_the_second_attempt_answered() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let first_attempt = awaiting_an_input_end(&awaiting, 1, "L-reconnected");
        awaiting.cancel_every_attempt_to_wire_link("L-reconnected");
        let second_attempt = awaiting_an_input_end(&awaiting, 2, "L-reconnected");

        assert_eq!(
            awaiting.note_the_far_sides_answer_to(
                command(1),
                "L-reconnected",
                OutOfProcessLinkWireOutcome::OpenedByTheFarSide
            ),
            OutOfProcessLinkWireAnswerNoted::DroppedAsTheAnswerToACancelledAttempt
        );
        assert_eq!(
            second_attempt.the_far_sides_answer(),
            None,
            "the first attempt's answer settles nothing"
        );
        assert_eq!(
            awaiting.note_the_far_sides_answer_to(
                command(2),
                "L-reconnected",
                refusal("no such service")
            ),
            OutOfProcessLinkWireAnswerNoted::SettledItsAttempt
        );

        assert_eq!(
            second_attempt.the_far_sides_answer(),
            Some(refusal("no such service")),
            "the link reads the second attempt's own answer"
        );
        assert_eq!(first_attempt.the_far_sides_answer(), None);
    }

    /// A newer attempt for the same end supersedes the older one with no
    /// unwiring in between: the last request wins.
    #[test]
    fn a_newer_attempt_for_the_same_end_cancels_the_one_before_it() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        awaiting_an_input_end(&awaiting, 1, "L-retried");
        let retried = awaiting_an_input_end(&awaiting, 2, "L-retried");

        assert_eq!(
            awaiting.note_the_far_sides_answer_to(
                command(1),
                "L-retried",
                refusal("the first attempt failed")
            ),
            OutOfProcessLinkWireAnswerNoted::DroppedAsTheAnswerToACancelledAttempt
        );
        assert_eq!(
            awaiting.note_the_far_sides_answer_to(
                command(2),
                "L-retried",
                OutOfProcessLinkWireOutcome::OpenedByTheFarSide
            ),
            OutOfProcessLinkWireAnswerNoted::SettledItsAttempt
        );
        assert_eq!(
            retried.the_far_sides_answer(),
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide)
        );
        assert_eq!(
            awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died"),
            0,
            "the superseded attempt is not left on the board to be refused"
        );
    }

    /// A link whose source and destination are the same far side is handed
    /// over once per end under one link id, and both ends wait within one
    /// attempt: neither end supersedes the other.
    #[test]
    fn a_link_whose_two_ends_are_one_far_side_has_both_of_them_answered() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let source_end = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        let destination_end = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        awaiting.await_the_answer_to_a_link_wire_attempt(
            command(1),
            "L-self".to_string(),
            PortDirection::Output,
            Arc::clone(&source_end),
        );
        awaiting.await_the_answer_to_a_link_wire_attempt(
            command(2),
            "L-self".to_string(),
            PortDirection::Input,
            Arc::clone(&destination_end),
        );

        for command_id in [1, 2] {
            assert_eq!(
                awaiting.note_the_far_sides_answer_to(
                    command(command_id),
                    "L-self",
                    OutOfProcessLinkWireOutcome::OpenedByTheFarSide
                ),
                OutOfProcessLinkWireAnswerNoted::SettledItsAttempt
            );
        }

        assert_eq!(
            source_end.the_far_sides_answer(),
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide)
        );
        assert_eq!(
            destination_end.the_far_sides_answer(),
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide)
        );
    }

    /// A disconnect cancels every end of the link, not one of them.
    #[test]
    fn unwiring_a_self_link_cancels_both_of_its_ends() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        for (command_id, port_direction) in [(1, PortDirection::Output), (2, PortDirection::Input)]
        {
            awaiting.await_the_answer_to_a_link_wire_attempt(
                command(command_id),
                "L-self".to_string(),
                port_direction,
                OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
            );
        }

        awaiting.cancel_every_attempt_to_wire_link("L-self");

        assert_eq!(
            awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died"),
            0
        );
    }

    #[test]
    fn an_answer_to_a_command_never_sent_is_reported_rather_than_dropped_silently() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        assert_eq!(
            awaiting.note_the_far_sides_answer_to(
                command(7),
                "L-nobody-waits",
                OutOfProcessLinkWireOutcome::OpenedByTheFarSide
            ),
            OutOfProcessLinkWireAnswerNoted::DroppedAsTheAnswerToNoCommandAwaited
        );
    }

    #[test]
    fn an_answer_naming_another_link_than_its_command_wired_settles_nothing() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let reply = awaiting_an_input_end(&awaiting, 1, "L-wired");

        assert_eq!(
            awaiting.note_the_far_sides_answer_to(
                command(1),
                "L-other",
                OutOfProcessLinkWireOutcome::OpenedByTheFarSide
            ),
            OutOfProcessLinkWireAnswerNoted::DroppedForNamingAnotherLink {
                link_id_the_command_wired: "L-wired".to_string()
            }
        );
        assert_eq!(reply.the_far_sides_answer(), None);
    }

    #[test]
    fn a_far_side_that_dies_refuses_every_link_it_never_answered_for() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let unanswered = awaiting_an_input_end(&awaiting, 1, "L-unanswered");

        assert_eq!(
            awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died"),
            1
        );

        assert_eq!(
            unanswered.the_far_sides_answer(),
            Some(refusal("its helper process died")),
            "a link whose far side died unanswered must never read wired"
        );
    }

    #[test]
    fn a_link_unwired_is_not_refused_when_its_far_side_dies() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let disconnected = awaiting_an_input_end(&awaiting, 1, "L-disconnected");

        awaiting.cancel_every_attempt_to_wire_link("L-disconnected");

        assert_eq!(
            awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died"),
            0
        );
        assert_eq!(disconnected.the_far_sides_answer(), None);
    }
}
