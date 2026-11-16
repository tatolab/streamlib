// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use std::sync::Arc;
use std::time::Duration;

use crate::core::child_process_lifetime::a_child_process_has_exited_without_being_reaped;
use crate::core::compiler::compiler_ops::processor_interpreter_shutdown_ladder::HelperProcessShutdownOutcome;
use crate::core::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::descriptors::ProcessorDescriptor;
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::media_clock::MediaClock;
use crate::core::processors::{
    DynGeneratedProcessor, FunctionNodeCallUnderWayInTheFarSide,
    FunctionNodePromotionAwaitedBeforeTheReaderRestarts, OutOfProcessLinkWiringEnvelope,
};

use super::{
    HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE,
    LONGEST_WAIT_FOR_A_COMPILE_TO_CLAIM_A_FUNCTION_NODE_PROMOTION,
    ProcessorInterpreterSpawnHostProcessor, lifecycle_reply_tag, reported_reason,
    standard_error_tail_as_a_refusal_quotes_it,
};

impl DynGeneratedProcessor for ProcessorInterpreterSpawnHostProcessor {
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.set_up_holding_every_later_link_until_the_setup_command_goes_out(|host| {
            host.start_the_helper_process_and_await_its_registration(ctx)
        })
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.child_is_gone {
            return Ok(());
        }
        // `run` enters the child's execution loop and is deliberately
        // unanswered — a reply would be read as the answer to the next command.
        self.send_to_child(&serde_json::json!({
            "cmd": "run",
            "capability": "limited",
            "execution": self.child_execution_mode(),
            "interval_ms": self
                .link_wiring
                .far_side_process_execution()
                .interval_ms()
                .unwrap_or(0),
        }))
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.ask_the_helper_to_stop_and_tear_down();
        Ok(())
    }

    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_the_helper_process_on_the_shutdown_ladder();
        Ok(())
    }

    fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.exchange_with_child_expecting(
            &serde_json::json!({"cmd": "on_pause", "capability": "limited"}),
            "on_pause",
            "ok",
        );
        Ok(())
    }

    fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.exchange_with_child_expecting(
            &serde_json::json!({"cmd": "on_resume", "capability": "limited"}),
            "on_resume",
            "ok",
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &str {
        &self.processor_display_name
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        Some(self.descriptor.clone())
    }

    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::new(ProcessExecution::Manual)
    }

    /// Notice a helper process that died on its own, and take its group with it.
    ///
    /// Asked of the process rather than of the escalate socket, which is what
    /// the engine means by "a crash the engine
    /// detects by the process itself rather than by its socket": a descendant
    /// holding that socket keeps its EOF from ever arriving, so a helper whose
    /// pid is gone went on reading as alive and issuing its privileged
    /// operations.
    fn detect_and_clean_up_after_an_out_of_process_helper_that_died(&mut self) {
        let Some(child) = self.child.as_ref() else {
            return;
        };
        let helper_process_id = child.id();
        let the_helper_died = a_child_process_has_exited_without_being_reaped(helper_process_id);
        // Read after the exit check: once the helper has exited, the board holds
        // the calls it had under way when it died.
        let function_node_call_under_way = self
            .link_wiring
            .function_node_call_its_far_side_has_under_way();
        let stalled_function_node_call = function_node_call_under_way
            .as_ref()
            .filter(|_| !the_helper_died)
            .and_then(|call_under_way| {
                let call_has_run_for = Duration::from_nanos(
                    (MediaClock::now().as_nanos() as u64)
                        .saturating_sub(call_under_way.call_started_at_monotonic_ns),
                );
                let watchdog_budget = self.function_node_call_watchdog_budget();
                (call_has_run_for > watchdog_budget).then_some((call_has_run_for, watchdog_budget))
            })
            .filter(|_| {
                // Read again after the budget was judged: a call that returned
                // in between is not the stall the first read timed.
                let read_again = self
                    .link_wiring
                    .function_node_call_its_far_side_has_under_way();
                read_again
                    .zip(function_node_call_under_way.as_ref())
                    .is_some_and(|(read_again, first_read)| {
                        read_again.is_the_same_call_as(first_read)
                    })
            });
        let function_node_that_overran = (!the_helper_died && stalled_function_node_call.is_none())
            .then(|| {
                self.link_wiring
                    .function_node_that_overran_its_far_sides_frame_interval()
            })
            .flatten();
        if !the_helper_died && stalled_function_node_call.is_none() {
            let Some(function_node_that_overran) = function_node_that_overran else {
                return;
            };
            self.end_the_helper_whose_function_node_overran_its_frame_interval(
                helper_process_id,
                function_node_that_overran,
            );
            return;
        }
        tracing::error!(
            "[{}] its helper process (pid={helper_process_id}) {}; taking its process group \
             down with it",
            self.processor_display_name,
            if the_helper_died {
                "died"
            } else {
                "stalled in a function node"
            },
        );
        let helper_process_shutdown_outcome = self.take_the_helper_process_group_down();
        let how_the_helper_process_ended = helper_process_shutdown_outcome
            .as_ref()
            .map(HelperProcessShutdownOutcome::how_the_helper_process_ended)
            .unwrap_or_else(|| "its helper process ended".to_string());
        // A helper still writing its core dump reads as alive, so a crash can
        // outlast the watchdog's budget; the signal it died of says it crashed.
        let stalled_function_node_call = stalled_function_node_call.filter(|_| {
            !helper_process_shutdown_outcome
                .as_ref()
                .is_some_and(HelperProcessShutdownOutcome::ended_on_a_signal_the_ladder_never_sends)
        });
        let why_the_helper_was_lost =
            match (&function_node_call_under_way, &stalled_function_node_call) {
                (Some(call_under_way), Some((call_has_run_for, watchdog_budget))) => format!(
                    "function node `{}` stalled: its call had run for {} ms, past the {} ms its \
                 reader's frame interval allows, so its helper process was ended. ",
                    call_under_way.function_node_name,
                    call_has_run_for.as_millis(),
                    watchdog_budget.as_millis(),
                ),
                (Some(call_under_way), None)
                    if call_under_way.function_node_calls_under_way <= 1 =>
                {
                    format!(
                        "function node `{}` crashed its helper process during its call. ",
                        call_under_way.function_node_name,
                    )
                }
                (Some(call_under_way), None) => format!(
                    "its helper process died with {} function-node calls under way, the oldest \
                     to `{}`, so which crashed it is not known. ",
                    call_under_way.function_node_calls_under_way, call_under_way.function_node_name,
                ),
                (None, _) => String::new(),
            };
        let standard_error_tail = self
            .child_standard_error_tail
            .as_ref()
            .map(|tail| {
                tail.text_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE)
            })
            .unwrap_or_default();
        let the_call_is_known_to_have_lost_the_helper = stalled_function_node_call.is_some()
            || function_node_call_under_way
                .as_ref()
                .is_some_and(|call_under_way| call_under_way.function_node_calls_under_way <= 1);
        self.function_node_call_that_lost_the_helper = function_node_call_under_way
            .filter(|_| the_call_is_known_to_have_lost_the_helper)
            .filter(|call_under_way| call_under_way.function_node_id != self.processor_id);
        self.why_the_helper_process_died_on_its_own = Some(format!(
            "{why_the_helper_was_lost}{how_the_helper_process_ended} (pid={helper_process_id}). {}",
            standard_error_tail_as_a_refusal_quotes_it(&standard_error_tail)
        ));
    }

    fn why_the_out_of_process_helper_died_on_its_own(&self) -> Option<String> {
        self.why_the_helper_process_died_on_its_own.clone()
    }

    fn ask_to_promote_the_function_node_that_lost_the_helper_that_died(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> Option<FunctionNodePromotionAwaitedBeforeTheReaderRestarts> {
        let FunctionNodeCallUnderWayInTheFarSide {
            function_node_id,
            function_node_name,
            ..
        } = self.function_node_call_that_lost_the_helper.take()?;
        let why_the_helper_process_died = self.why_the_helper_process_died_on_its_own.clone()?;
        if self.child.is_some() {
            self.take_the_helper_process_group_down();
        }
        // The links the promotion wires into this reader ride its next setup
        // command only once the envelope no longer answers for the dead helper.
        self.link_wiring
            .reopen_for_a_new_far_side_in_place_of_one_that_died();
        let promotion_request_number = self
            .link_wiring
            .await_a_function_node_promotion_out_of_its_islands();
        if let Err(refusal) = ctx
            .runtime()
            .request_a_function_node_promotion_out_of_the_islands_of_the_reader_it_crashed(
                &function_node_id.as_str().into(),
                &self.processor_id.as_str().into(),
                promotion_request_number,
                &why_the_helper_process_died,
            )
        {
            self.link_wiring
                .give_up_the_function_node_promotion_out_of_its_islands_unless_claimed();
            tracing::warn!(
                "[{}] function node `{function_node_name}` stays in its islands: its promotion \
                 could not be asked for: {refusal}",
                self.processor_display_name
            );
            return None;
        }
        let shutdown_escalation = self
            .the_stream_this_helper_belongs_to
            .shutdown_escalation
            .clone();
        Some(FunctionNodePromotionAwaitedBeforeTheReaderRestarts {
            reader_link_wiring: Arc::clone(&self.link_wiring),
            reader_name: self.processor_display_name.clone(),
            function_node_name,
            longest_wait_for_a_compile_to_claim_it:
                LONGEST_WAIT_FOR_A_COMPILE_TO_CLAIM_A_FUNCTION_NODE_PROMOTION,
            stop_waiting: Box::new(move || shutdown_escalation.is_requested()),
        })
    }

    fn restart_the_out_of_process_helper_that_died(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> Result<()> {
        if self.why_the_helper_process_died_on_its_own.is_none() {
            return Err(Error::Runtime(format!(
                "[{}] its helper process did not die on its own, so it is not restarted",
                self.processor_display_name
            )));
        }
        if self.child.is_some() {
            self.take_the_helper_process_group_down();
        }
        self.why_the_helper_process_died_on_its_own = None;
        self.what_the_node_reaches
            .process_raising_the_node_reports
            .record(None);
        self.what_the_node_reaches
            .node_not_ready_reports
            .record(None);
        self.child_is_gone = false;
        self.shutdown_was_already_asked_of_this_helper = false;
        self.child_standard_error_tail = None;
        self.bridge = None;
        self.link_wiring
            .reopen_for_a_new_far_side_in_place_of_one_that_died();
        tracing::info!(
            "[{}] starting a new helper process in place of the one that died",
            self.processor_display_name
        );
        let restarted = self
            .set_up_holding_every_later_link_until_the_setup_command_goes_out(|host| {
                host.start_the_helper_process_and_await_its_registration(ctx)
            })
            .and_then(|()| match self.failure_the_node_reported_of_itself() {
                Some(reason_the_node_gave) => Err(Error::Runtime(format!(
                    "[{}] the node failed itself while its new helper process set up: \
                     {reason_the_node_gave}",
                    self.processor_display_name
                ))),
                None => self.start(ctx),
            });
        if let Err(restart_refusal) = &restarted {
            // A restart that failed is one more death, and the next restart
            // starts from it.
            self.why_the_helper_process_died_on_its_own = Some(restart_refusal.to_string());
        }
        restarted
    }

    fn failure_the_node_reported_of_itself(&self) -> Option<String> {
        self.what_the_node_reaches
            .failure_the_node_reported_of_itself
            .reason()
    }

    fn why_process_is_raising_as_its_helper_reports(&self) -> Option<String> {
        self.what_the_node_reaches
            .process_raising_the_node_reports
            .raising_because()
    }

    fn why_the_node_is_not_ready_as_its_helper_reports(&self) -> Option<String> {
        self.what_the_node_reaches
            .node_not_ready_reports
            .not_ready_because()
    }

    fn has_failed_unrecoverably(&self) -> bool {
        // The bridge as well as this host's own flag: between `run` and
        // teardown the parent sends nothing, so a child that dies mid-run is
        // never noticed by a failed exchange. The bridge's reader thread sees
        // EOF the moment the child's *last* fd closes, which a surviving
        // descendant defers indefinitely — so the poll above asks the process
        // and this reads what either of them found.
        self.child_is_gone || self.bridge.as_ref().is_some_and(|bridge| bridge.is_dead())
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        false
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        false
    }

    fn out_of_process_link_wiring(&self) -> Option<Arc<OutOfProcessLinkWiringEnvelope>> {
        Some(Arc::clone(&self.link_wiring))
    }

    fn set_iceoryx2_resources(
        &mut self,
        _output_writer: Option<crate::iceoryx2::OutputWriter>,
        _input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> Result<()> {
        Ok(())
    }

    fn iceoryx2_output_writer_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::OutputWriterInner>> {
        None
    }

    fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<std::sync::Arc<crate::iceoryx2::InputMailboxesInner>> {
        None
    }

    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> Result<()> {
        // Unlike the lifecycle hooks, which swallow a lost child because
        // teardown must proceed regardless, a config update has no such
        // constraint — reporting success while the child runs the old
        // configuration would be a lie the control plane passes on.
        let reply = self
            .exchange_with_child(
                &serde_json::json!({"cmd": "update_config", "config": config_json}),
                "update_config",
            )
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "[{}] its helper process did not answer update_config",
                    self.processor_display_name
                ))
            })?;
        if lifecycle_reply_tag(&reply) != Some("ok") {
            let reported = reported_reason(&reply);
            return Err(Error::Runtime(format!(
                "[{}] its helper process refused update_config: {reported}",
                self.processor_display_name
            )));
        }
        Ok(())
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        serde_json::json!({
            "helper_process_pid": self.child.as_ref().map(|child| child.id()),
            "entrypoint": self.processor_class_import_path,
            "interpreter": self.stream_environment.interpreter.to_string_lossy(),
            "helper_process_is_gone": self.child_is_gone,
        })
    }

    fn config_json(&self) -> serde_json::Value {
        self.processor_configuration
            .clone()
            .unwrap_or(serde_json::Value::Null)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
