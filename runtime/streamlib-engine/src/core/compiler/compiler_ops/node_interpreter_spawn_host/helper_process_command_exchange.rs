// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::time::Duration;

use crate::core::compiler::compiler_ops::function_node_pool::FUNCTION_NODE_POOL_NODE_TYPE;
use crate::core::compiler::compiler_ops::subprocess_bridge::{
    LifecycleCommandAwaitingItsReply, LifecycleCommandSent, SubprocessBridge,
};
use crate::core::error::{Error, Result};

use super::{
    FUNCTION_NODE_CALL_WATCHDOG_FLOOR, FUNCTION_NODE_CALL_WATCHDOG_FRAME_INTERVALS,
    NodeInterpreterSpawnHost, REPLY_DEADLINE, lifecycle_reply_tag, reported_reason,
};

impl NodeInterpreterSpawnHost {
    /// Send the helper a command it answers nothing to, such as `run`.
    pub(super) fn send_to_child_a_command_answered_by_nothing(
        &mut self,
        message: &serde_json::Value,
    ) -> Result<()> {
        let bridge = self.bridge_to_send_to()?;
        let sent = bridge.send_a_command_answered_by_nothing(message);
        if sent.is_err() {
            self.give_up_on_the_helper_process();
        }
        sent
    }

    /// Send the helper `command` with the fields of `frame`, stamped with the
    /// id its reply is matched to.
    pub(super) fn send_to_child_a_command_awaiting_its_reply(
        &mut self,
        command: LifecycleCommandAwaitingItsReply,
        frame: serde_json::Value,
    ) -> Result<LifecycleCommandSent> {
        let bridge = self.bridge_to_send_to()?;
        let sent = bridge.send_a_lifecycle_command_awaiting_its_reply(command, frame);
        if sent.is_err() {
            self.give_up_on_the_helper_process();
        }
        sent
    }

    fn bridge_to_send_to(&self) -> Result<&SubprocessBridge> {
        self.bridge.as_ref().ok_or_else(|| {
            Error::Runtime(format!(
                "[{}] there is no helper process to send to",
                self.node_display_name
            ))
        })
    }

    /// Send `command` and wait for the reply carrying its id, giving up on the
    /// helper when that reply does not come.
    pub(super) fn exchange_with_child(
        &mut self,
        command: LifecycleCommandAwaitingItsReply,
        frame: serde_json::Value,
    ) -> Option<serde_json::Value> {
        if self.child_is_gone {
            return None;
        }
        let sent = match self.send_to_child_a_command_awaiting_its_reply(command, frame) {
            Ok(sent) => sent,
            Err(send_failure) => {
                tracing::warn!(
                    "[{}] helper process stopped listening during {}: {send_failure}",
                    self.node_display_name,
                    command.command_tag(),
                );
                return None;
            }
        };
        let bridge = self.bridge.as_ref()?;
        match bridge.await_the_reply_to(sent, REPLY_DEADLINE) {
            Ok(reply) => Some(reply),
            Err(no_reply) => {
                bridge.abandon_the_reply_to(sent);
                tracing::warn!("[{}] {no_reply}; giving up on it", self.node_display_name,);
                self.give_up_on_the_helper_process();
                None
            }
        }
    }

    /// Send `command`, wait for its reply, and warn if the helper refused it.
    pub(super) fn exchange_with_child_expecting_ok(
        &mut self,
        command: LifecycleCommandAwaitingItsReply,
        frame: serde_json::Value,
    ) {
        let Some(reply) = self.exchange_with_child(command, frame) else {
            return;
        };
        if lifecycle_reply_tag(&reply) != Some("ok") {
            tracing::warn!(
                "[{}] helper process refused {}: {}",
                self.node_display_name,
                command.command_tag(),
                reported_reason(&reply),
            );
        }
    }

    /// How long one function-node call may run before its helper is ended as
    /// stalled: three of its reader's frame intervals, and never less than a
    /// floor the death poll can resolve.
    pub(super) fn function_node_call_watchdog_budget(&self) -> Duration {
        let reader_frame_interval = Duration::from_millis(
            self.link_wiring
                .far_side_process_execution()
                .interval_ms()
                .unwrap_or(0) as u64,
        );
        (reader_frame_interval * FUNCTION_NODE_CALL_WATCHDOG_FRAME_INTERVALS)
            .max(FUNCTION_NODE_CALL_WATCHDOG_FLOOR)
    }

    /// The frame interval the helper times each function-node call against: the
    /// pool's own interval, since a node too slow to keep the pool's pace holds
    /// back every island the pool carries; `None` for any other helper.
    pub(super) fn frame_interval_its_helper_times_function_node_calls_against(
        &self,
    ) -> Option<Duration> {
        if self.node_class_import_path != FUNCTION_NODE_POOL_NODE_TYPE {
            return None;
        }
        self.link_wiring
            .far_side_process_execution()
            .interval_ms()
            .filter(|interval_ms| *interval_ms > 0)
            .map(|interval_ms| Duration::from_millis(interval_ms as u64))
    }
}
