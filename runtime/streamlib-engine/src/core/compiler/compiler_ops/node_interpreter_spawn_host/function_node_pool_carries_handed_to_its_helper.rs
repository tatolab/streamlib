// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The carries a running function node pool's helper takes without a restart.

use crate::core::descriptors::NodeDescriptor;
use crate::core::error::{Error, Result};

use super::{NodeInterpreterSpawnHost, lifecycle_reply_tag, reported_reason};

impl NodeInterpreterSpawnHost {
    /// Hand the running pool's helper `config_naming_every_carry`, whose carries it does not
    /// hold yet it declares and reads from its next `process` on, and take `descriptor`, the
    /// pool's ports with those carries' added, so a restart of the helper sets it up with every
    /// carry. An output a late carry adds keeps its last message as one the helper started with
    /// does; its loss counts land on the board from the helper's next start.
    pub(crate) fn hand_the_running_function_node_pool_its_carries(
        &mut self,
        descriptor: NodeDescriptor,
        config_naming_every_carry: serde_json::Value,
    ) -> Result<()> {
        let output_ports_keeping_their_last_message: Vec<&str> = descriptor
            .outputs
            .iter()
            .filter(|output_port| output_port.flows_as_a_value())
            .filter(|output_port| {
                !self
                    .descriptor
                    .outputs
                    .iter()
                    .any(|held| held.name == output_port.name)
            })
            .map(|output_port| output_port.name.as_str())
            .collect();
        let reply = self
            .exchange_with_child(
                &serde_json::json!({
                    "cmd": "update_config",
                    "config": config_naming_every_carry,
                    "output_ports_keeping_their_last_message":
                        output_ports_keeping_their_last_message,
                }),
                "update_config",
            )
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "[{}] its helper process did not answer the carries it was handed",
                    self.node_display_name
                ))
            })?;
        if lifecycle_reply_tag(&reply) != Some("ok") {
            return Err(Error::Runtime(format!(
                "[{}] its helper process refused the carries it was handed: {}",
                self.node_display_name,
                reported_reason(&reply)
            )));
        }
        self.node_configuration = Some(config_naming_every_carry);
        self.descriptor = descriptor;
        Ok(())
    }
}
