// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Handing a running subprocess what is wired after its setup command.

use std::sync::Arc;

use crate::core::error::{Error, PortDirection, Result};
use crate::core::processors::{OutOfProcessFarSideLinkDelivery, OutOfProcessLinkWireReply};

use super::{
    ParentSideOfOneSubprocessBridgeSharedByItsThreads,
    refusal_of_a_link_into_a_helper_process_that_failed,
};

/// How links wired after a subprocess's setup command reach it: `wire_link`
/// and `unwire_link` frames written straight onto its bridge's socket.
pub struct SubprocessBridgeLinkDelivery {
    pub(super) processor_display_name: String,
    pub(super) parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
}

impl OutOfProcessFarSideLinkDelivery for SubprocessBridgeLinkDelivery {
    /// The answer arrives on its own rpc tag, which the reader routes to
    /// `answer_cell`. The send never waits on that answer: the compiler hands
    /// links over holding the graph's write lock.
    fn hand_over_a_link_wired_after_setup(
        &self,
        port_direction: PortDirection,
        link_wiring: &serde_json::Value,
        answer_cell: Arc<OutOfProcessLinkWireReply>,
    ) -> Result<()> {
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Err(Error::Runtime(
                refusal_of_a_link_into_a_helper_process_that_failed(
                    &self.processor_display_name,
                    &self.parent_side.processor_id,
                ),
            ));
        }
        let Some(link_id) = link_wiring.get("link_id").and_then(|id| id.as_str()) else {
            return Err(Error::Configuration(format!(
                "the wiring handed to processor '{}' ({}) names no link, so its helper \
                 process could not answer for one",
                self.processor_display_name, self.parent_side.processor_id
            )));
        };
        // Registered before the send: the subprocess can answer the moment the
        // frame lands.
        self.parent_side
            .links_awaiting_their_wire_reply
            .await_an_answer_for_link(link_id.to_string(), answer_cell);
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(&serde_json::json!({
                "cmd": "wire_link",
                "direction": port_direction.as_wire_str(),
                "link": link_wiring,
            }))
    }

    /// Unanswered, like `run`: a reply nobody reads is read as the answer to
    /// the next lifecycle command.
    fn tell_the_far_side_a_link_was_unwired(
        &self,
        port_direction: PortDirection,
        local_port_name: &str,
        link_id: &str,
    ) -> Result<()> {
        // A link on its way out is owed no answer; left waiting, a subprocess
        // that dies later would refuse a link the graph no longer has.
        self.parent_side
            .links_awaiting_their_wire_reply
            .stop_awaiting_an_answer_for_link(link_id);
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Ok(());
        }
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(&serde_json::json!({
                "cmd": "unwire_link",
                "direction": port_direction.as_wire_str(),
                "port": local_port_name,
                "link_id": link_id,
            }))
    }

    /// Answered like `wire_link`, on the island's link id: the reader imports
    /// and compiles the island's nodes before it says it evaluates them.
    fn hand_over_a_function_node_island_wired_after_setup(
        &self,
        function_node_island: &serde_json::Value,
        answer_cell: Arc<OutOfProcessLinkWireReply>,
    ) -> Result<()> {
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Err(Error::Runtime(
                refusal_of_a_link_into_a_helper_process_that_failed(
                    &self.processor_display_name,
                    &self.parent_side.processor_id,
                ),
            ));
        }
        let Some(link_id) = function_node_island
            .get("link_id")
            .and_then(|id| id.as_str())
        else {
            return Err(Error::Configuration(format!(
                "the function node island handed to processor '{}' ({}) names no link, so its \
                 helper process could not answer for it",
                self.processor_display_name, self.parent_side.processor_id
            )));
        };
        self.parent_side
            .links_awaiting_their_wire_reply
            .await_an_answer_for_link(link_id.to_string(), answer_cell);
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(&serde_json::json!({
                "cmd": "feed_island",
                "island": function_node_island,
            }))
    }

    /// Unanswered, like `unwire_link`.
    fn tell_the_far_side_a_function_node_island_was_unwired(
        &self,
        input_port: &str,
        link_id: &str,
    ) -> Result<()> {
        self.parent_side
            .links_awaiting_their_wire_reply
            .stop_awaiting_an_answer_for_link(link_id);
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Ok(());
        }
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(&serde_json::json!({
                "cmd": "unfeed_island",
                "port": input_port,
                "link_id": link_id,
            }))
    }

    /// Unanswered, like `unwire_link`.
    fn hand_over_the_pin_values_of_its_input_ports(
        &self,
        pin_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Ok(());
        }
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(&serde_json::json!({
                "cmd": "set_pin_values",
                "pins": pin_values,
            }))
    }

    /// Unanswered, like `unwire_link`.
    fn hand_over_the_config_fields_its_node_exposes_as_inputs(
        &self,
        exposed_config_fields: &[String],
    ) -> Result<()> {
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Ok(());
        }
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(&serde_json::json!({
                "cmd": "set_exposed_config_fields",
                "exposed_config_fields": exposed_config_fields,
            }))
    }

    fn refuse_every_link_still_awaiting_the_far_sides_answer(&self, reason: &str) {
        self.parent_side
            .links_awaiting_their_wire_reply
            .refuse_every_link_still_awaiting_an_answer(reason);
    }
}
