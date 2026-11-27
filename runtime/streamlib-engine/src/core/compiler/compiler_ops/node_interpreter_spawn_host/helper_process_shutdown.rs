// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::compiler::compiler_ops::node_interpreter_shutdown_ladder::{
    HelperProcessShutdownLadder, HelperProcessShutdownOutcome, LifecycleReplyAwaited,
};
use crate::core::compiler::compiler_ops::subprocess_bridge::{
    ShutdownCommandsSent, refusal_of_a_link_into_a_helper_process_that_failed,
};
use crate::core::nodes::{
    FunctionNodeCallUnderWayInTheFarSide, FunctionNodeThatOverranItsFarSidesFrameInterval,
};
use streamlib_ipc_types::FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED;

use super::{NodeInterpreterSpawnHost, await_the_reply_to};

impl NodeInterpreterSpawnHost {
    /// Stop treating the child as reachable: no lifecycle command is exchanged
    /// with it again, and every link wired into it from now on is refused.
    pub(super) fn give_up_on_the_helper_process(&mut self) {
        self.child_is_gone = true;
        self.link_wiring
            .refuse_every_later_link_because_the_far_side_is_gone(
                refusal_of_a_link_into_a_helper_process_that_failed(
                    &self.node_display_name,
                    &self.node_run_id,
                ),
            );
    }

    /// Ask the helper for both shutdown rungs at once: `stop` and `teardown`
    /// are sent together.
    ///
    /// Queued together and timed apart: a helper that misses its `stopped` —
    /// because a callback outran the budget and took the interrupt — already
    /// holds the command that runs its `teardown()`, so the interrupt costs it
    /// the bag in flight rather than its teardown.
    ///
    /// Idempotent, because the engine's `stop()` is not the only way onto the
    /// ladder: a helper interrupted while it was still importing never reached
    /// that hook, and it is owed a `teardown()` all the same.
    pub(super) fn ask_the_helper_to_stop_and_tear_down(&mut self) {
        if self.shutdown_was_already_asked_of_this_helper {
            return;
        }
        self.shutdown_was_already_asked_of_this_helper = true;
        // No channel is no failure to report: a helper whose crash was already
        // noticed had its bridge dropped then, and warning twice more about a
        // command nobody could have taken is the stream of noise
        // `exchange_with_child` exists to avoid. A helper that is merely
        // unresponsive still has its bridge, and is still asked.
        if self.bridge.is_none() {
            return;
        }
        // Warned once rather than per command: a helper that did not take the
        // first will not take the second either, and one unusable child owes
        // one line, which is the posture `exchange_with_child` already keeps.
        let mut shutdown_commands_sent = ShutdownCommandsSent::default();
        let sent = self.sending_to_child(|bridge| {
            bridge.send_the_shutdown_commands(&mut shutdown_commands_sent)
        });
        self.shutdown_commands_sent = shutdown_commands_sent;
        if let Err(unreachable_helper) = sent {
            tracing::warn!(
                "[{}] its helper process did not take its shutdown commands: \
                 {unreachable_helper}",
                self.node_display_name,
            );
        }
    }

    /// Walk the shutdown ladder: a callback that outran its budget interrupted,
    /// `teardown()` given its five seconds, then the helper's whole process
    /// group down and the child reaped.
    pub(super) fn stop_the_helper_process_on_the_shutdown_ladder(&mut self) {
        self.ask_the_helper_to_stop_and_tear_down();
        let outcome = self.child.take().map(|child| {
            let bridge = self.bridge.as_ref();
            let shutdown_commands_sent = &self.shutdown_commands_sent;
            let node_display_name = &self.node_display_name;
            HelperProcessShutdownLadder::taking_over(
                self.node_display_name.clone(),
                child,
                self.the_stream_this_helper_belongs_to
                    .shutdown_escalation
                    .clone(),
            )
            .walk_every_rung(|command, slice| match bridge {
                Some(bridge) => await_the_reply_to(
                    node_display_name,
                    bridge,
                    shutdown_commands_sent,
                    command,
                    slice,
                ),
                None => LifecycleReplyAwaited::NoReplyCanArrive,
            })
        });
        self.close_the_engines_end_of_the_helper_process(outcome);
    }

    /// End a helper that named a function node for overrunning its frame
    /// interval, keeping the node for promotion as a crash's would be.
    pub(super) fn end_the_helper_whose_function_node_overran_its_frame_interval(
        &mut self,
        helper_process_id: u32,
        function_node_that_overran: FunctionNodeThatOverranItsFarSidesFrameInterval,
    ) {
        let FunctionNodeThatOverranItsFarSidesFrameInterval {
            function_node_id,
            function_node_name,
            longest_overrunning_call,
        } = function_node_that_overran;
        let frame_interval = self
            .frame_interval_its_helper_times_function_node_calls_against()
            .unwrap_or_default();
        tracing::error!(
            "[{}] function node `{function_node_name}` overran its island's {} ms frame interval \
             {FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED} times \
             running; ending its helper process (pid={helper_process_id})",
            self.node_display_name,
            frame_interval.as_millis(),
        );
        let how_the_helper_process_ended = self
            .take_the_helper_process_group_down()
            .as_ref()
            .map(HelperProcessShutdownOutcome::how_the_helper_process_ended)
            .unwrap_or_else(|| "its helper process ended".to_string());
        self.why_the_helper_process_died_on_its_own = Some(format!(
            "function node `{function_node_name}` overran its island's {} ms frame interval \
             {FUNCTION_NODE_CALLS_OVERRUNNING_THE_FRAME_INTERVAL_BEFORE_IT_IS_NAMED} times \
             running, the longest call {} ms, so its helper process was ended. \
             {how_the_helper_process_ended} (pid={helper_process_id}).",
            frame_interval.as_millis(),
            longest_overrunning_call.as_millis(),
        ));
        self.function_node_call_that_lost_the_helper = (function_node_id != self.node_run_id)
            .then_some(FunctionNodeCallUnderWayInTheFarSide {
                function_node_id,
                function_node_name,
                call_started_at_monotonic_ns: 0,
                function_node_calls_under_way: 1,
            });
    }

    /// Take the group down with no cooperative rung at all — see
    /// [`HelperProcessShutdownLadder::skip_to_terminating_the_process_group`].
    pub(super) fn take_the_helper_process_group_down(
        &mut self,
    ) -> Option<HelperProcessShutdownOutcome> {
        let outcome = self.child.take().map(|child| {
            HelperProcessShutdownLadder::taking_over(
                self.node_display_name.clone(),
                child,
                self.the_stream_this_helper_belongs_to
                    .shutdown_escalation
                    .clone(),
            )
            .skip_to_terminating_the_process_group()
        });
        self.close_the_engines_end_of_the_helper_process(outcome.clone());
        outcome
    }

    /// Let go of everything this end held for the helper, whichever way it went.
    ///
    /// Dropping the bridge shuts the engine's end of the escalate socket, so a
    /// descendant that outlived the group kill can issue no privileged
    /// operation. The standard-output and standard-error readers are left
    /// running rather than closed: they are detached threads, so nothing waits
    /// on them, and a survivor's writes are still logged instead of raising
    /// SIGPIPE at it.
    pub(super) fn close_the_engines_end_of_the_helper_process(
        &mut self,
        outcome: Option<HelperProcessShutdownOutcome>,
    ) {
        self.give_up_on_the_helper_process();
        self.bridge.take();
        self.link_wiring
            .keep_every_function_node_state_its_far_side_wrote();
        self.link_wiring
            .how_its_far_side_ran_each_function_node()
            .forget_every_raise_the_helper_reported();
        if let Some(HelperProcessShutdownOutcome::Reaped(exit_status)) = outcome {
            tracing::debug!(
                "[{}] helper process exited: {exit_status}",
                self.node_display_name
            );
            #[cfg(target_os = "macos")]
            self.surface_share_admission_of_the_helper_process.take();
        }
        if outcome.is_some() {
            self.reclaim_the_iceoryx2_nodes_the_helper_left();
        }
    }

    /// Reclaim the iceoryx2 nodes a helper left registered.
    ///
    /// A dead node holds its slot in every service it had opened, so a channel
    /// whose destination is gone keeps counting it against the cap until a
    /// sweep takes it out. Run at every helper exit and not only at a detected
    /// crash: a helper the ladder had to kill never finalized its interpreter,
    /// so its engine half never dropped the node either.
    ///
    /// The engine's own domain configuration, never the ambient one: the global
    /// lookup path would sweep another domain, or none.
    pub(super) fn reclaim_the_iceoryx2_nodes_the_helper_left(&self) {
        let Some(iceoryx2_domain_root) = self.iceoryx2_domain_root.as_deref() else {
            return;
        };
        match crate::iceoryx2::reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(
            iceoryx2_domain_root,
        ) {
            Ok(reclaimed_node_count) if reclaimed_node_count > 0 => tracing::info!(
                "[{}] reclaimed {reclaimed_node_count} iceoryx2 node(s) its helper left",
                self.node_display_name,
            ),
            Ok(_) => {}
            Err(sweep_failure) => tracing::warn!(
                "[{}] could not reclaim the iceoryx2 nodes its helper left: {sweep_failure}",
                self.node_display_name,
            ),
        }
    }
}
