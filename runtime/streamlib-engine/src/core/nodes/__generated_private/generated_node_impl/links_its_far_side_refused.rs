// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The answers an envelope keeps of the links its far side owes one for, so a link or a
//! function node island the far side refused, or one whose handover failed, is wired again.

use std::sync::Arc;

use super::{
    LinkAwaitingHandoverBehindTheSetupCommand, OutOfProcessLinkWiringEnvelope,
    RecordedLinksAndHowTheNextReachesTheFarSide,
};
use crate::core::Result;
use crate::core::nodes::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};

impl OutOfProcessLinkWiringEnvelope {
    /// Whether the far side refused a link or a function node island handed over to it past
    /// its setup command that the envelope still records, or one whose handover behind the
    /// setup command failed, so it is wired again.
    pub(crate) fn holds_a_link_its_far_side_refused(&self) -> bool {
        self.recorded_links_and_how_the_next_reaches_the_far_side
            .lock()
            .answer_cells_of_links_handed_over_past_the_setup_command
            .iter()
            .any(|(_, answer_cell)| answer_cell.the_far_side_refused())
    }
}

impl RecordedLinksAndHowTheNextReachesTheFarSide {
    /// Keep `answer_cell` as the one answer owed for `link_id`.
    pub(super) fn keep_the_answer_owed_for(
        &mut self,
        link_id: Option<String>,
        answer_cell: Arc<OutOfProcessLinkWireReply>,
    ) {
        self.forget_the_answer_owed_for(link_id.as_deref());
        self.answer_cells_of_links_handed_over_past_the_setup_command
            .push((link_id, answer_cell));
    }

    /// Keep `answer_cell` as the one answer owed for `link_id` after its handover, refused in
    /// the words of the failure when `handed_over` failed, so the link is wired again.
    pub(super) fn keep_the_answer_owed_after(
        &mut self,
        link_id: Option<String>,
        answer_cell: &Arc<OutOfProcessLinkWireReply>,
        handed_over: &Result<()>,
    ) {
        if let Err(handover_failure) = handed_over {
            refuse_because(answer_cell, &handover_failure.to_string());
        }
        self.keep_the_answer_owed_for(link_id, Arc::clone(answer_cell));
    }

    /// Forget the answer owed for `link_id`.
    pub(super) fn forget_the_answer_owed_for(&mut self, link_id: Option<&str>) {
        self.answer_cells_of_links_handed_over_past_the_setup_command
            .retain(|(handed_over_link_id, _)| handed_over_link_id.as_deref() != link_id);
    }
}

fn refuse_because(answer_cell: &OutOfProcessLinkWireReply, reason: &str) {
    answer_cell.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
        reason: reason.to_string(),
    });
}

/// Refuse every link `waiting` behind the setup command with `reason`.
pub(super) fn refuse_every_link_waiting_behind_the_setup_command(
    waiting: Vec<LinkAwaitingHandoverBehindTheSetupCommand>,
    reason: &str,
) {
    for waiting_link in waiting {
        refuse_because(&waiting_link.answer_cell, reason);
    }
}
