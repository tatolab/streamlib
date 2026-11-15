// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Exchanging commands with a running processor interpreter, and the budget a
//! function node call is watched against.

use std::time::Duration;

use crate::core::compiler::compiler_ops::function_node_pool::FUNCTION_NODE_POOL_NODE_TYPE;
use crate::core::error::{Error, Result};

use super::{
    FUNCTION_NODE_CALL_WATCHDOG_FLOOR, FUNCTION_NODE_CALL_WATCHDOG_FRAME_INTERVALS,
    ProcessorInterpreterSpawnHostProcessor, REPLY_DEADLINE, lifecycle_reply_tag, reported_reason,
};

impl ProcessorInterpreterSpawnHostProcessor {
    pub(super) fn send_to_child(&mut self, message: &serde_json::Value) -> Result<()> {
        let bridge = self.bridge.as_ref().ok_or_else(|| {
            Error::Runtime(format!(
                "[{}] there is no helper process to send to",
                self.processor_display_name
            ))
        })?;
        let sent = bridge.send(message);
        if sent.is_err() {
            self.give_up_on_the_helper_process();
        }
        sent
    }

    /// Send a command, wait a bounded time for the child's reply, and give up
    /// on the child if either half fails.
    ///
    /// Every command that expects a reply goes through here, so one unusable
    /// child produces one warning rather than a stream of them — and, the
    /// reason the wait is bounded, so no command can park the engine's thread
    /// forever. A child that has died disconnects the channel and is noticed at
    /// once; a child that is *alive but not reading* — a user callback blocked
    /// on a socket, a wedged `teardown` — would never reply, and this runs on
    /// the lifecycle thread holding the processor's lock, so waiting forever
    /// there hangs the runtime's teardown, with the kill that would have
    /// resolved it sitting unreachable further down it.
    pub(super) fn exchange_with_child(
        &mut self,
        message: &serde_json::Value,
        lifecycle_command_name: &str,
    ) -> Option<serde_json::Value> {
        if self.child_is_gone {
            return None;
        }
        if let Err(send_failure) = self.send_to_child(message) {
            tracing::warn!(
                "[{}] helper process stopped listening during {lifecycle_command_name}: \
                 {send_failure}",
                self.processor_display_name
            );
            self.give_up_on_the_helper_process();
            return None;
        }
        let reply = self
            .bridge
            .as_ref()
            .map(|bridge| bridge.recv_lifecycle_timeout(REPLY_DEADLINE));
        match reply {
            Some(Ok(reply)) => Some(reply),
            Some(Err(no_reply)) => {
                tracing::warn!(
                    "[{}] helper process did not answer {lifecycle_command_name} within {}s \
                     ({no_reply}); giving up on it",
                    self.processor_display_name,
                    REPLY_DEADLINE.as_secs(),
                );
                self.give_up_on_the_helper_process();
                None
            }
            None => None,
        }
    }

    /// Send a command, wait for its reply, and warn if the child answered with
    /// something other than the tag this command expects.
    ///
    /// A mismatch means the child's lifecycle has desynchronized from the
    /// parent's — the reply in hand belongs to an earlier command — which is
    /// worth saying out loud even though there is nothing to do about it here.
    pub(super) fn exchange_with_child_expecting(
        &mut self,
        message: &serde_json::Value,
        lifecycle_command_name: &str,
        expected_reply_tag: &str,
    ) {
        let Some(reply) = self.exchange_with_child(message, lifecycle_command_name) else {
            return;
        };
        let reply_tag = lifecycle_reply_tag(&reply).unwrap_or("");
        if reply_tag != expected_reply_tag {
            let reported = reported_reason(&reply);
            tracing::warn!(
                "[{}] helper process answered {lifecycle_command_name} with {reply_tag:?} rather \
                 than {expected_reply_tag:?}: {reported}",
                self.processor_display_name
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
        if self.processor_class_import_path != FUNCTION_NODE_POOL_NODE_TYPE {
            return None;
        }
        self.link_wiring
            .far_side_process_execution()
            .interval_ms()
            .filter(|interval_ms| *interval_ms > 0)
            .map(|interval_ms| Duration::from_millis(interval_ms as u64))
    }
}
