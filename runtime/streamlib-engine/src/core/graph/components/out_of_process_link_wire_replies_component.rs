// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use crate::core::processors::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};

/// The answers one link is waiting on from the out-of-process ends it was
/// handed to, live rather than copied at wiring time — the far side answers on
/// its bridge's reader thread, which holds no graph lock.
///
/// A link with no such component was never handed to a running far side: it is
/// wholly in the app process, or it was carried in a far side's startup
/// envelope, which that far side's `ready` confirms. A link between two helpers
/// waits on both of them.
pub struct OutOfProcessLinkWireRepliesComponent(pub Vec<Arc<OutOfProcessLinkWireReply>>);

/// What the out-of-process ends of one link have answered so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutOfProcessLinkWireProgress {
    /// An end has not answered yet, and none has refused.
    AnEndHasNotAnsweredYet,
    /// Every end opened its port.
    EveryEndOpenedItsPort,
    /// An end refused, and said why.
    AnEndRefused {
        /// That end's own reason.
        reason: String,
    },
}

impl OutOfProcessLinkWireRepliesComponent {
    /// Read every end's answer into one progress reading.
    ///
    /// A refusal outranks an unanswered end: a link one end will never open is
    /// an error the moment that end says so, rather than a pending link waiting
    /// out the other end's silence.
    pub fn what_its_out_of_process_ends_have_answered(&self) -> OutOfProcessLinkWireProgress {
        let mut every_end_opened_its_port = true;
        for reply in &self.0 {
            match reply.the_far_sides_answer() {
                Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide { reason }) => {
                    return OutOfProcessLinkWireProgress::AnEndRefused { reason };
                }
                Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide) => {}
                None => every_end_opened_its_port = false,
            }
        }
        if every_end_opened_its_port {
            OutOfProcessLinkWireProgress::EveryEndOpenedItsPort
        } else {
            OutOfProcessLinkWireProgress::AnEndHasNotAnsweredYet
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened() -> Arc<OutOfProcessLinkWireReply> {
        let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        reply.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        reply
    }

    fn refused(reason: &str) -> Arc<OutOfProcessLinkWireReply> {
        let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        reply.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
            reason: reason.to_string(),
        });
        reply
    }

    #[test]
    fn a_link_neither_end_has_answered_for_is_still_waiting() {
        let replies = OutOfProcessLinkWireRepliesComponent(vec![
            OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
            OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
        ]);
        assert_eq!(
            replies.what_its_out_of_process_ends_have_answered(),
            OutOfProcessLinkWireProgress::AnEndHasNotAnsweredYet
        );
    }

    #[test]
    fn a_helper_to_helper_link_waits_on_both_ends_before_it_is_open() {
        let replies = OutOfProcessLinkWireRepliesComponent(vec![
            opened(),
            OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
        ]);
        assert_eq!(
            replies.what_its_out_of_process_ends_have_answered(),
            OutOfProcessLinkWireProgress::AnEndHasNotAnsweredYet,
            "one end's answer must not report a link the other end never opened as open"
        );
    }

    #[test]
    fn a_link_every_end_opened_is_open() {
        let replies = OutOfProcessLinkWireRepliesComponent(vec![opened(), opened()]);
        assert_eq!(
            replies.what_its_out_of_process_ends_have_answered(),
            OutOfProcessLinkWireProgress::EveryEndOpenedItsPort
        );
    }

    #[test]
    fn a_refusal_outranks_an_end_that_has_not_answered() {
        let replies = OutOfProcessLinkWireRepliesComponent(vec![
            OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
            refused("no such service"),
        ]);
        assert_eq!(
            replies.what_its_out_of_process_ends_have_answered(),
            OutOfProcessLinkWireProgress::AnEndRefused {
                reason: "no such service".to_string()
            }
        );
    }
}
