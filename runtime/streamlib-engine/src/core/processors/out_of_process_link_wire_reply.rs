// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What an out-of-process far side answers about a link it was told to wire.
//!
//! A link handed to a running far side is not wired when the engine sends it —
//! it is wired when the far side has opened its own port for it. `connect` does
//! not wait for that answer (`docs/plan/ARCHITECTURE.md` §Processor model, the
//! `[local-transport-hardening]` entry): it returns with the link pending, and
//! these cells are what `graph` reads afterwards.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

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
}

/// Every link one far side was told to wire and has not answered for yet, kept
/// by that far side's bridge so its reader thread can route an answer to the
/// link it names.
///
/// A link id maps to *several* outstanding answers, not one: both ends of a
/// link whose source and destination are the same processor — an output wired
/// to that processor's own input, which `connect` accepts — are handed to one
/// far side over one bridge under one link id, and it answers each. Keyed one
/// deep, the second registration would evict the first and that end would wait
/// for an answer already spent, leaving the link `pending` for good.
#[derive(Debug, Default)]
pub(crate) struct LinksAwaitingTheirOutOfProcessWireReply {
    replies_by_link_id: Mutex<HashMap<String, Vec<Arc<OutOfProcessLinkWireReply>>>>,
}

impl LinksAwaitingTheirOutOfProcessWireReply {
    /// Start waiting on one more answer for a link.
    pub(crate) fn await_an_answer_for_link(
        &self,
        link_id: String,
        reply: Arc<OutOfProcessLinkWireReply>,
    ) {
        self.replies_by_link_id
            .lock()
            .entry(link_id)
            .or_default()
            .push(reply);
    }

    /// Note an answer that names a link, reporting whether any end was waiting
    /// for it — a far side answering for a link nobody is waiting on is worth a
    /// log line rather than a silent drop.
    ///
    /// One answer settles one of that link's outstanding ends, and which one
    /// does not matter: a link is `wired` only when every end opened and
    /// `error` the moment any end refused, so what the link reports depends on
    /// the answers it got and never on which end sent which.
    pub(crate) fn note_the_far_sides_answer_for_link(
        &self,
        link_id: &str,
        outcome: OutOfProcessLinkWireOutcome,
    ) -> bool {
        let reply = {
            let mut replies = self.replies_by_link_id.lock();
            let Some(ends_still_waiting) = replies.get_mut(link_id) else {
                return false;
            };
            let reply = ends_still_waiting.pop();
            if ends_still_waiting.is_empty() {
                replies.remove(link_id);
            }
            reply
        };
        let Some(reply) = reply else {
            return false;
        };
        reply.note_the_far_sides_answer(outcome);
        true
    }

    /// Stop waiting on a link that is going away before it was ever answered
    /// for, so a disconnect leaves nothing behind for its far side to answer.
    /// Every end of it goes, since the whole link is being taken down.
    pub(crate) fn stop_awaiting_an_answer_for_link(&self, link_id: &str) {
        self.replies_by_link_id.lock().remove(link_id);
    }

    /// Refuse every link still waiting, because the far side is gone, and hand
    /// back how many there were so the caller can say so once. A link whose far
    /// side died unanswered never reads `wired`.
    pub(crate) fn refuse_every_link_still_awaiting_an_answer(&self, reason: &str) -> usize {
        let refused: Vec<Arc<OutOfProcessLinkWireReply>> = self
            .replies_by_link_id
            .lock()
            .drain()
            .flat_map(|(_, ends)| ends)
            .collect();
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
    fn an_answer_reaches_the_link_it_names_and_no_other() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let answered = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        let untouched = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        awaiting.await_an_answer_for_link("L-answered".to_string(), Arc::clone(&answered));
        awaiting.await_an_answer_for_link("L-untouched".to_string(), Arc::clone(&untouched));

        assert!(awaiting.note_the_far_sides_answer_for_link(
            "L-answered",
            OutOfProcessLinkWireOutcome::OpenedByTheFarSide
        ));

        assert_eq!(
            answered.the_far_sides_answer(),
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide)
        );
        assert_eq!(untouched.the_far_sides_answer(), None);
        assert_eq!(
            awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died"),
            1,
            "an answered link is off the board, so only the untouched one is left to refuse"
        );
    }

    /// A link whose source and destination are the same helper is handed over
    /// twice under one link id, and the helper answers each. Both ends have to
    /// land, or the link reads `pending` for the life of the runtime.
    ///
    /// Fail-without-fix: key the board one deep and the second registration
    /// evicts the first — the source end below stays `None`, the second answer
    /// routes to nothing, and `graph` never leaves `pending`.
    #[test]
    fn a_link_whose_two_ends_are_one_helper_has_both_of_them_answered() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let source_end = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        let destination_end = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        awaiting.await_an_answer_for_link("L-self".to_string(), Arc::clone(&source_end));
        awaiting.await_an_answer_for_link("L-self".to_string(), Arc::clone(&destination_end));

        for _ in 0..2 {
            assert!(awaiting.note_the_far_sides_answer_for_link(
                "L-self",
                OutOfProcessLinkWireOutcome::OpenedByTheFarSide
            ));
        }

        assert_eq!(
            source_end.the_far_sides_answer(),
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide)
        );
        assert_eq!(
            destination_end.the_far_sides_answer(),
            Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide)
        );
        assert_eq!(
            awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died"),
            0,
            "both ends were answered, so the board holds nothing for this link"
        );
    }

    /// One end refusing is what the link reports, whichever end it was.
    #[test]
    fn one_end_of_a_self_link_refusing_still_reaches_a_cell() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let ends: Vec<Arc<OutOfProcessLinkWireReply>> = (0..2)
            .map(|_| OutOfProcessLinkWireReply::awaiting_the_far_sides_answer())
            .collect();
        for end in &ends {
            awaiting.await_an_answer_for_link("L-self".to_string(), Arc::clone(end));
        }

        awaiting.note_the_far_sides_answer_for_link(
            "L-self",
            OutOfProcessLinkWireOutcome::OpenedByTheFarSide,
        );
        awaiting.note_the_far_sides_answer_for_link("L-self", refusal("no such service"));

        let answers: Vec<Option<OutOfProcessLinkWireOutcome>> =
            ends.iter().map(|end| end.the_far_sides_answer()).collect();
        assert!(
            answers.contains(&Some(refusal("no such service"))),
            "the refusal has to land on one of the link's ends, so the link reads error: \
             {answers:?}"
        );
        assert!(
            answers.iter().all(|answer| answer.is_some()),
            "no end may be left unanswered: {answers:?}"
        );
    }

    /// A disconnect takes every end of the link, not one of them.
    #[test]
    fn stopping_a_self_link_leaves_neither_end_on_the_board() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        for _ in 0..2 {
            awaiting.await_an_answer_for_link(
                "L-self".to_string(),
                OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
            );
        }

        awaiting.stop_awaiting_an_answer_for_link("L-self");

        assert_eq!(
            awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died"),
            0
        );
    }

    #[test]
    fn an_answer_naming_a_link_nobody_awaits_is_reported_rather_than_dropped_silently() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        assert!(!awaiting.note_the_far_sides_answer_for_link(
            "L-nobody-waits",
            OutOfProcessLinkWireOutcome::OpenedByTheFarSide
        ));
    }

    #[test]
    fn a_far_side_that_dies_refuses_every_link_it_never_answered_for() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let unanswered = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        awaiting.await_an_answer_for_link("L-unanswered".to_string(), Arc::clone(&unanswered));

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
    fn a_link_that_stopped_awaiting_is_not_refused_when_its_far_side_dies() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let disconnected = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        awaiting.await_an_answer_for_link("L-disconnected".to_string(), Arc::clone(&disconnected));

        awaiting.stop_awaiting_an_answer_for_link("L-disconnected");

        assert_eq!(
            awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died"),
            0
        );
        assert_eq!(disconnected.the_far_sides_answer(), None);
    }
}
