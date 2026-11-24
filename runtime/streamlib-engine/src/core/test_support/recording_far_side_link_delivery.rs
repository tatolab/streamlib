// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A far side past its setup command that records what the engine hands it.

use std::sync::Arc;

/// One port a far side was asked to drop. Named rather than a tuple so a
/// swapped port and link id fails the assert instead of passing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReclaimedLink {
    pub(crate) port_direction: crate::core::PortDirection,
    pub(crate) local_port_name: String,
    pub(crate) link_id: String,
}

/// A far side past its setup command that records every link it is handed and
/// every port it is told to drop, holding each handed link's answer cell for the
/// test to answer.
///
/// Clones share what they record, which is how a test reads what the engine
/// asked of a far side it handed to an envelope.
#[derive(Clone, Default)]
pub(crate) struct RecordingOutOfProcessFarSideLinkDelivery {
    /// Every link handed over, with the direction it was wired in.
    pub(crate) late_wired_links:
        Arc<parking_lot::Mutex<Vec<(crate::core::PortDirection, serde_json::Value)>>>,
    /// The answer cells handed over, in the order their links were — how a
    /// test plays a far side that has not answered yet, opened its port, or
    /// refused.
    pub(crate) wire_answers_owed:
        Arc<parking_lot::Mutex<Vec<Arc<crate::core::nodes::OutOfProcessLinkWireReply>>>>,
    /// Every port the far side was told to drop.
    pub(crate) reclaimed_links: Arc<parking_lot::Mutex<Vec<ReclaimedLink>>>,
    /// Every function node island handed over, as the far side received it.
    pub(crate) late_fed_function_node_islands: Arc<parking_lot::Mutex<Vec<serde_json::Value>>>,
    /// Every `(input port, link id)` whose island the far side was told to stop evaluating.
    pub(crate) unfed_function_node_islands: Arc<parking_lot::Mutex<Vec<(String, String)>>>,
    /// Every set of unlinked input values handed over, in order.
    pub(crate) input_values_handed_over:
        Arc<parking_lot::Mutex<Vec<serde_json::Map<String, serde_json::Value>>>>,
    /// The Trigger inputs of each hand-over that fired any, in order.
    pub(crate) fires_handed_over: Arc<parking_lot::Mutex<Vec<Vec<String>>>>,
    /// Whether the far side refuses the input values handed over, as a helper that died does.
    pub(crate) refuses_input_values: Arc<std::sync::atomic::AtomicBool>,
    /// Whether the far side refuses the next input values handed over, and takes those after.
    pub(crate) refuses_input_values_once: Arc<std::sync::atomic::AtomicBool>,
    /// Run once, as the next input values are handed over: how a test changes a value while a
    /// node is handed its values.
    pub(crate) while_input_values_are_handed_over:
        Arc<parking_lot::Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    /// Whether the far side refuses the function node islands handed over, as a helper that died
    /// does.
    pub(crate) refuses_function_node_islands: Arc<std::sync::atomic::AtomicBool>,
}

impl crate::core::nodes::OutOfProcessFarSideLinkDelivery
    for RecordingOutOfProcessFarSideLinkDelivery
{
    fn hand_over_a_link_wired_after_setup(
        &self,
        port_direction: crate::core::PortDirection,
        link_wiring: &serde_json::Value,
        answer_cell: Arc<crate::core::nodes::OutOfProcessLinkWireReply>,
    ) -> crate::core::Result<()> {
        self.late_wired_links
            .lock()
            .push((port_direction, link_wiring.clone()));
        self.wire_answers_owed.lock().push(answer_cell);
        Ok(())
    }

    fn tell_the_far_side_a_link_was_unwired(
        &self,
        port_direction: crate::core::PortDirection,
        local_port_name: &str,
        link_id: &str,
    ) -> crate::core::Result<()> {
        self.reclaimed_links.lock().push(ReclaimedLink {
            port_direction,
            local_port_name: local_port_name.to_string(),
            link_id: link_id.to_string(),
        });
        Ok(())
    }

    fn hand_over_a_function_node_island_wired_after_setup(
        &self,
        function_node_island: &serde_json::Value,
        answer_cell: Arc<crate::core::nodes::OutOfProcessLinkWireReply>,
    ) -> crate::core::Result<()> {
        if self
            .refuses_function_node_islands
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(crate::core::Error::Configuration(
                "the far side refuses function node islands in this test".to_string(),
            ));
        }
        self.late_fed_function_node_islands
            .lock()
            .push(function_node_island.clone());
        self.wire_answers_owed.lock().push(answer_cell);
        Ok(())
    }

    fn tell_the_far_side_a_function_node_island_was_unwired(
        &self,
        input_port: &str,
        link_id: &str,
    ) -> crate::core::Result<()> {
        self.unfed_function_node_islands
            .lock()
            .push((input_port.to_string(), link_id.to_string()));
        Ok(())
    }

    fn hand_over_the_unlinked_input_values(
        &self,
        input_values: &serde_json::Map<String, serde_json::Value>,
        fired: &[String],
    ) -> crate::core::Result<()> {
        if self
            .refuses_input_values
            .load(std::sync::atomic::Ordering::SeqCst)
            || self
                .refuses_input_values_once
                .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(crate::core::Error::Configuration(
                "the far side refuses input values in this test".to_string(),
            ));
        }
        if !fired.is_empty() {
            self.fires_handed_over.lock().push(fired.to_vec());
            return Ok(());
        }
        self.input_values_handed_over
            .lock()
            .push(input_values.clone());
        let while_handed_over = self.while_input_values_are_handed_over.lock().take();
        if let Some(while_handed_over) = while_handed_over {
            while_handed_over();
        }
        Ok(())
    }

    fn refuse_every_link_still_awaiting_the_far_sides_answer(&self, reason: &str) {
        for answer_cell in self.wire_answers_owed.lock().iter() {
            answer_cell.note_the_far_sides_answer(
                crate::core::nodes::OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                    reason: reason.to_string(),
                },
            );
        }
    }
}
