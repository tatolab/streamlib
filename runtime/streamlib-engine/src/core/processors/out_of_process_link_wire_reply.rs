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
use std::sync::{Arc, Mutex};

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
#[derive(Debug, Default)]
pub struct OutOfProcessLinkWireReply {
    answer: Mutex<Option<OutOfProcessLinkWireOutcome>>,
}

impl OutOfProcessLinkWireReply {
    /// A cell for a link just handed to a far side, with nothing answered yet.
    pub fn awaiting_the_far_sides_answer() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Note what the far side answered. The first answer stands: a far side
    /// whose death is noticed after it already answered must not overwrite the
    /// outcome the link has been reporting.
    pub fn note_the_far_sides_answer(&self, outcome: OutOfProcessLinkWireOutcome) {
        let Ok(mut answer) = self.answer.lock() else {
            return;
        };
        if answer.is_none() {
            *answer = Some(outcome);
        }
    }

    /// What the far side answered, or `None` while it still has not.
    pub fn the_far_sides_answer(&self) -> Option<OutOfProcessLinkWireOutcome> {
        self.answer.lock().ok().and_then(|answer| answer.clone())
    }
}

/// Every link one far side was told to wire and has not answered for yet, kept
/// by that far side's bridge so its reader thread can route an answer to the
/// link it names.
#[derive(Debug, Default)]
pub struct LinksAwaitingTheirOutOfProcessWireReply {
    replies_by_link_id: Mutex<HashMap<String, Arc<OutOfProcessLinkWireReply>>>,
}

impl LinksAwaitingTheirOutOfProcessWireReply {
    /// Start waiting on one link's answer.
    pub fn await_an_answer_for_link(&self, link_id: String, reply: Arc<OutOfProcessLinkWireReply>) {
        if let Ok(mut replies) = self.replies_by_link_id.lock() {
            replies.insert(link_id, reply);
        }
    }

    /// Note an answer that names a link, reporting whether any link was
    /// waiting for it — a far side answering for a link nobody is waiting on
    /// is worth a log line rather than a silent drop.
    pub fn note_the_far_sides_answer_for_link(
        &self,
        link_id: &str,
        outcome: OutOfProcessLinkWireOutcome,
    ) -> bool {
        let Ok(mut replies) = self.replies_by_link_id.lock() else {
            return false;
        };
        let Some(reply) = replies.remove(link_id) else {
            return false;
        };
        reply.note_the_far_sides_answer(outcome);
        true
    }

    /// Stop waiting on a link that is going away before it was ever answered
    /// for, so a disconnect leaves nothing behind for its far side to answer.
    pub fn stop_awaiting_an_answer_for_link(&self, link_id: &str) {
        if let Ok(mut replies) = self.replies_by_link_id.lock() {
            replies.remove(link_id);
        }
    }

    /// Refuse every link still waiting, because the far side is gone. A link
    /// whose far side died unanswered never reads `wired`.
    pub fn refuse_every_link_still_awaiting_an_answer(&self, reason: &str) {
        let Ok(mut replies) = self.replies_by_link_id.lock() else {
            return;
        };
        for (_, reply) in replies.drain() {
            reply.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                reason: reason.to_string(),
            });
        }
    }

    /// How many links are still waiting. Test and log surface only.
    pub fn count_still_awaiting_an_answer(&self) -> usize {
        self.replies_by_link_id
            .lock()
            .map(|replies| replies.len())
            .unwrap_or(0)
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
        assert_eq!(awaiting.count_still_awaiting_an_answer(), 1);
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

        awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died");

        assert_eq!(
            unanswered.the_far_sides_answer(),
            Some(refusal("its helper process died")),
            "a link whose far side died unanswered must never read wired"
        );
        assert_eq!(awaiting.count_still_awaiting_an_answer(), 0);
    }

    #[test]
    fn a_link_that_stopped_awaiting_is_not_refused_when_its_far_side_dies() {
        let awaiting = LinksAwaitingTheirOutOfProcessWireReply::default();
        let disconnected = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        awaiting.await_an_answer_for_link("L-disconnected".to_string(), Arc::clone(&disconnected));

        awaiting.stop_awaiting_an_answer_for_link("L-disconnected");
        awaiting.refuse_every_link_still_awaiting_an_answer("its helper process died");

        assert_eq!(disconnected.the_far_sides_answer(), None);
    }
}
