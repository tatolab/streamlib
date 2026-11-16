// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::fd::OwnedFd;

use parking_lot::Mutex;

use crate::core::RuntimeContext;
use crate::core::child_process_restart_backoff::{
    CHILD_PROCESS_RUN_THAT_RESETS_THE_RESTART_BACKOFF, ChildProcessRestartBackoff,
};
use crate::core::context::StreamClock;
use crate::core::context::{IsolationTier, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::core::execution::processor_scheduling_loop::{
    ProcessorSchedulingCallback, ProcessorSchedulingLoopControl, ProcessorSchedulingLoopHost,
    run_continuous_scheduling_loop_on_its_stream_clock,
    run_continuous_scheduling_loop_on_real_time, run_reactive_scheduling_loop,
};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::{ObservableProcessorState, ProcessorUniqueId, StepProgressOfOneNode};
use crate::core::processors::{ProcessorInstance, ProcessorState};

#[tracing::instrument(name = "processor.lifecycle", skip(processor, shutdown_rx, shutdown_wake_fd, state, pause_gate, step_progress, exec_config, runtime_ctx), fields(processor_id = %id, isolation_tier = isolation_tier.as_str()))]
pub fn run_processor_loop(
    id: ProcessorUniqueId,
    processor: Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: crossbeam_channel::Receiver<()>,
    #[cfg(unix)] shutdown_wake_fd: Option<OwnedFd>,
    state: Arc<ObservableProcessorState>,
    pause_gate: Arc<AtomicBool>,
    step_progress: Arc<StepProgressOfOneNode>,
    exec_config: ExecutionConfig,
    runtime_ctx: RuntimeContext,
    isolation_tier: IsolationTier,
) {
    tracing::info!(
        "[{}] Thread started ({})",
        id,
        exec_config.execution.description()
    );

    match exec_config.execution {
        ProcessExecution::Continuous { interval_ms } => {
            let mut host = scheduling_loop_host_on_this_thread(
                &id,
                &processor,
                &shutdown_rx,
                #[cfg(unix)]
                None,
                &pause_gate,
                &runtime_ctx,
                &state,
                &step_progress,
            );
            if step_progress.stands_outside_the_step_barrier() {
                run_continuous_scheduling_loop_on_real_time(&mut host, interval_ms);
            } else {
                run_continuous_scheduling_loop_on_its_stream_clock(
                    &mut host,
                    &*runtime_ctx.stream_clock,
                    interval_ms,
                );
            }
            tracing::info!("[{}] Received shutdown signal", id);
        }
        ProcessExecution::Reactive => {
            let mut host = scheduling_loop_host_on_this_thread(
                &id,
                &processor,
                &shutdown_rx,
                #[cfg(unix)]
                shutdown_wake_fd,
                &pause_gate,
                &runtime_ctx,
                &state,
                &step_progress,
            );
            run_reactive_scheduling_loop(id.as_str(), &mut host);
        }
        ProcessExecution::Manual => {
            run_manual_mode(
                &id,
                &processor,
                &shutdown_rx,
                &state,
                &pause_gate,
                &runtime_ctx,
                isolation_tier,
            );
        }
    }

    match isolation_tier.grant_full_access() {
        Some(full_access_grant) => {
            tracing::info!("[{}] Invoking teardown()...", id);
            let full_ctx = RuntimeContextFullAccess::new(&runtime_ctx, full_access_grant);
            let mut guard = processor.lock();
            match guard.teardown(&full_ctx) {
                Ok(()) => tracing::info!("[{}] teardown() completed successfully", id),
                Err(e) => tracing::warn!("[{}] teardown() failed: {}", id, e),
            }
        }
        None => {
            tracing::debug!(
                "[{}] Untrusted isolation tier ({}): skipping in-process teardown()",
                id,
                isolation_tier.as_str(),
            );
        }
    }

    state.transition_to_unless_already_failed(ProcessorState::Stopped);
    tracing::info!("[{}] Thread stopped", id);
}

struct ProcessorSchedulingLoopHostOnAnEngineThread<'a, CallProcessor> {
    processor: &'a Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &'a crossbeam_channel::Receiver<()>,
    /// Owned for as long as the loop runs, which registers it by its raw fd.
    #[cfg(unix)]
    shutdown_wake_fd: Option<OwnedFd>,
    pause_gate: &'a AtomicBool,
    stream_clock: &'a StreamClock,
    step_progress: &'a StepProgressOfOneNode,
    call_processor: CallProcessor,
}

impl<CallProcessor> ProcessorSchedulingLoopHost
    for ProcessorSchedulingLoopHostOnAnEngineThread<'_, CallProcessor>
where
    CallProcessor: FnMut(ProcessorSchedulingCallback, bool),
{
    fn call_processor(
        &mut self,
        callback: ProcessorSchedulingCallback,
        counted_on_the_step_ledger: bool,
    ) {
        (self.call_processor)(callback, counted_on_the_step_ledger);
    }

    fn note_step_finished(&mut self, step_index: u64) {
        self.step_progress.note_step_finished(step_index);
        self.stream_clock.note_step_progress();
    }

    fn pause_requested(&self) -> bool {
        self.pause_gate.load(Ordering::Acquire)
    }

    fn input_mailboxes(&self) -> Option<Arc<crate::iceoryx2::InputMailboxesInner>> {
        self.processor.lock().iceoryx2_input_mailboxes_inner()
    }

    #[cfg(unix)]
    fn control_arrival_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.shutdown_wake_fd.as_ref().map(AsRawFd::as_raw_fd)
    }

    fn take_arrived_control(&mut self) -> ProcessorSchedulingLoopControl {
        if self.shutdown_rx.try_recv().is_ok() {
            ProcessorSchedulingLoopControl::Stop
        } else {
            ProcessorSchedulingLoopControl::KeepRunning
        }
    }

    /// Only a shutdown makes the wake fd readable, and it stays readable.
    fn take_the_control_its_fd_announced(&mut self) -> ProcessorSchedulingLoopControl {
        ProcessorSchedulingLoopControl::Stop
    }

    fn after_a_signal_interrupted_a_wait(&mut self) -> ProcessorSchedulingLoopControl {
        ProcessorSchedulingLoopControl::KeepRunning
    }

    fn wait_on_the_machine_clock_or_a_control(
        &mut self,
        machine_wait: Duration,
    ) -> ProcessorSchedulingLoopControl {
        match self.shutdown_rx.recv_timeout(machine_wait) {
            Ok(()) => ProcessorSchedulingLoopControl::Stop,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                ProcessorSchedulingLoopControl::KeepRunning
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                let _ = crossbeam_channel::after(machine_wait).recv();
                ProcessorSchedulingLoopControl::KeepRunning
            }
        }
    }

    fn wait_for_a_step_opened_after(
        &mut self,
        after: Option<u64>,
        longest_wait: Duration,
    ) -> Option<u64> {
        self.stream_clock
            .wait_for_a_step_opened_after(after, longest_wait)
    }
}

fn dispatch_processor_scheduling_callback(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    runtime_ctx: &RuntimeContext,
    state: &ObservableProcessorState,
    step_progress: &StepProgressOfOneNode,
    callback: ProcessorSchedulingCallback,
    counted_on_the_step_ledger: bool,
) {
    let counted = counted_on_the_step_ledger && !runtime_ctx.stream_clock.spec().is_real();
    {
        let _callback_in_flight = counted.then(|| step_progress.callback_in_flight());
        match callback {
            ProcessorSchedulingCallback::OnPause => dispatch_on_pause(id, processor, runtime_ctx),
            ProcessorSchedulingCallback::OnResume => dispatch_on_resume(id, processor, runtime_ctx),
            ProcessorSchedulingCallback::Process => {
                dispatch_process(id, processor, runtime_ctx, state)
            }
        }
    }
    if counted {
        runtime_ctx.stream_clock.note_step_progress();
    }
}

fn scheduling_loop_host_on_this_thread<'a>(
    id: &'a ProcessorUniqueId,
    processor: &'a Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &'a crossbeam_channel::Receiver<()>,
    #[cfg(unix)] shutdown_wake_fd: Option<OwnedFd>,
    pause_gate: &'a AtomicBool,
    runtime_ctx: &'a RuntimeContext,
    state: &'a ObservableProcessorState,
    step_progress: &'a StepProgressOfOneNode,
) -> ProcessorSchedulingLoopHostOnAnEngineThread<'a, impl FnMut(ProcessorSchedulingCallback, bool)>
{
    ProcessorSchedulingLoopHostOnAnEngineThread {
        processor,
        shutdown_rx,
        #[cfg(unix)]
        shutdown_wake_fd,
        pause_gate,
        stream_clock: &runtime_ctx.stream_clock,
        step_progress,
        call_processor: move |callback, counted_on_the_step_ledger| {
            dispatch_processor_scheduling_callback(
                id,
                processor,
                runtime_ctx,
                state,
                step_progress,
                callback,
                counted_on_the_step_ledger,
            )
        },
    }
}

fn run_manual_mode(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &crossbeam_channel::Receiver<()>,
    state: &Arc<ObservableProcessorState>,
    pause_gate: &Arc<AtomicBool>,
    runtime_ctx: &RuntimeContext,
    isolation_tier: IsolationTier,
) {
    let Some(start_grant) = isolation_tier.grant_full_access() else {
        tracing::warn!(
            "[{}] Untrusted isolation tier ({}): in-process FullAccess denied by \
             construction — refusing privileged start() (belongs behind the \
             subprocess sandbox)",
            id,
            isolation_tier.as_str(),
        );
        state.transition_to(ProcessorState::Error);
        return;
    };
    tracing::info!("[{}] Invoking start()...", id);
    {
        let full_ctx = RuntimeContextFullAccess::new(runtime_ctx, start_grant);
        let mut guard = processor.lock();
        match guard.start(&full_ctx) {
            Ok(()) => tracing::info!("[{}] start() completed successfully", id),
            Err(e) => {
                tracing::error!("[{}] start() failed: {}", id, e);
                state.fail_with(format!("start() failed: {e}"));
                return;
            }
        }
    }

    // Wait for shutdown signal - this thread is just a lifecycle manager
    // Real work happens on OS-managed callback threads
    let mut was_paused = false;
    let mut already_reported_failure = false;
    let mut restart_backoff = ChildProcessRestartBackoff::new();
    let mut current_helper_process_started_at = Instant::now();
    let mut next_helper_process_restart_at: Option<Instant> = None;
    let mut helper_process_stopped_answering_at: Option<Instant> = None;

    loop {
        // Check for shutdown
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("[{}] Received shutdown signal", id);
            break;
        }

        // Periodic check for pause/resume state changes
        let is_paused = pause_gate.load(Ordering::Acquire);

        if is_paused && !was_paused {
            dispatch_on_pause(id, processor, runtime_ctx);
            // A pause that landed while the node read `Restarting` set the
            // gate but could not move the state.
            state.transition_to_only_from(ProcessorState::Running, ProcessorState::Paused);
            was_paused = true;
        } else if !is_paused && was_paused {
            dispatch_on_resume(id, processor, runtime_ctx);
            was_paused = false;
        }

        if let Some(restart_at) = next_helper_process_restart_at {
            if Instant::now() >= restart_at {
                next_helper_process_restart_at = None;
                match restart_the_out_of_process_helper_that_died(
                    id,
                    processor,
                    runtime_ctx,
                    isolation_tier,
                ) {
                    Ok(()) => {
                        state.note_a_helper_process_restart();
                        current_helper_process_started_at = Instant::now();
                        helper_process_stopped_answering_at = None;
                        if pause_gate.load(Ordering::Acquire) {
                            dispatch_on_pause(id, processor, runtime_ctx);
                            was_paused = true;
                            state.transition_to_only_from(
                                ProcessorState::Restarting,
                                ProcessorState::Paused,
                            );
                        } else {
                            was_paused = false;
                            state.transition_to_only_from(
                                ProcessorState::Restarting,
                                ProcessorState::Running,
                            );
                        }
                    }
                    Err(_) if shutdown_rx.try_recv().is_ok() => {
                        tracing::info!(
                            "[{}] Received shutdown signal while its helper process restarted",
                            id
                        );
                        break;
                    }
                    Err(restart_refusal) => {
                        if let Some(reason_the_node_gave) =
                            processor.lock().failure_the_node_reported_of_itself()
                        {
                            tracing::error!(
                                "[{}] the node failed itself: {}",
                                id,
                                reason_the_node_gave
                            );
                            state.fail_with(reason_the_node_gave);
                            already_reported_failure = true;
                        } else {
                            next_helper_process_restart_at = after_a_helper_process_death(
                                id,
                                state,
                                &mut restart_backoff,
                                restart_refusal.to_string(),
                                Duration::ZERO,
                            );
                            already_reported_failure = next_helper_process_restart_at.is_none();
                        }
                    }
                }
            }
        } else if !already_reported_failure {
            let (
                failure_the_node_reported_of_itself,
                has_failed_unrecoverably,
                why_the_helper_process_died_on_its_own,
                why_process_is_raising_as_its_helper_reports,
                why_the_node_is_not_ready_as_its_helper_reports,
                its_helper_reports_whether_the_node_is_ready,
            ) = {
                let mut processor = processor.lock();
                processor.detect_and_clean_up_after_an_out_of_process_helper_that_died();
                (
                    processor.failure_the_node_reported_of_itself(),
                    processor.has_failed_unrecoverably(),
                    processor.why_the_out_of_process_helper_died_on_its_own(),
                    processor.why_process_is_raising_as_its_helper_reports(),
                    processor.why_the_node_is_not_ready_as_its_helper_reports(),
                    processor.out_of_process_link_wiring().is_some(),
                )
            };
            state.note_why_process_is_raising(why_process_is_raising_as_its_helper_reports);
            if its_helper_reports_whether_the_node_is_ready {
                state.note_why_the_node_is_not_ready(
                    why_the_node_is_not_ready_as_its_helper_reports,
                );
            }
            match (
                failure_the_node_reported_of_itself,
                has_failed_unrecoverably,
                why_the_helper_process_died_on_its_own,
            ) {
                // A node that failed itself is not started again, even when
                // its helper exits after saying so.
                (Some(reason_the_node_gave), _, _) => {
                    tracing::error!("[{}] the node failed itself: {}", id, reason_the_node_gave);
                    state.fail_with(reason_the_node_gave);
                    already_reported_failure = true;
                }
                (None, _, Some(exit_reason)) => {
                    helper_process_stopped_answering_at = None;
                    next_helper_process_restart_at = after_a_helper_process_death(
                        id,
                        state,
                        &mut restart_backoff,
                        exit_reason,
                        current_helper_process_started_at.elapsed(),
                    );
                    already_reported_failure = next_helper_process_restart_at.is_none();
                }
                (None, true, None) => {
                    // A helper that is exiting closes its escalate socket
                    // before its process can be collected; only one still
                    // there after the grace has stopped answering.
                    let stopped_answering_at =
                        *helper_process_stopped_answering_at.get_or_insert_with(Instant::now);
                    if stopped_answering_at.elapsed()
                        >= GRACE_FOR_A_HELPER_PROCESS_TO_EXIT_AFTER_ITS_ESCALATE_SOCKET_CLOSED
                    {
                        tracing::error!("[{}] Processor failed unrecoverably", id);
                        state.fail_with("its helper process stopped answering the engine");
                        already_reported_failure = true;
                    }
                }
                (None, false, None) => {}
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Call stop() - stops callbacks and waits for in-flight work. Privileged
    // ctx. Reuse the same trust-axis gate as start(): reaching here means
    // start() ran under a trusted grant, so a fresh grant is available for the
    // symmetric stop().
    match isolation_tier.grant_full_access() {
        Some(stop_grant) => {
            tracing::info!("[{}] Invoking stop()...", id);
            let full_ctx = RuntimeContextFullAccess::new(runtime_ctx, stop_grant);
            let mut guard = processor.lock();
            match guard.stop(&full_ctx) {
                Ok(()) => tracing::info!("[{}] stop() completed successfully", id),
                Err(e) => tracing::warn!("[{}] stop() failed: {}", id, e),
            }
        }
        None => {
            tracing::debug!(
                "[{}] Untrusted isolation tier ({}): skipping in-process stop()",
                id,
                isolation_tier.as_str(),
            );
        }
    }
}

/// How long a helper process whose escalate socket closed gets to exit before
/// it is taken to have stopped answering rather than to be dying.
const GRACE_FOR_A_HELPER_PROCESS_TO_EXIT_AFTER_ITS_ESCALATE_SOCKET_CLOSED: Duration =
    Duration::from_secs(5);

fn restart_the_out_of_process_helper_that_died(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    runtime_ctx: &RuntimeContext,
    isolation_tier: IsolationTier,
) -> crate::core::Result<()> {
    let restart_grant = isolation_tier.grant_full_access().ok_or_else(|| {
        crate::core::Error::Runtime(format!(
            "[{id}] untrusted isolation tier ({}): no full access to restart its helper process",
            isolation_tier.as_str()
        ))
    })?;
    let full_ctx = RuntimeContextFullAccess::new(runtime_ctx, restart_grant);
    let promotion_awaited = processor
        .lock()
        .ask_to_promote_the_function_node_that_lost_the_helper_that_died(&full_ctx);
    if let Some(promotion_awaited) = promotion_awaited {
        promotion_awaited.wait_until_it_settles();
    }
    processor
        .lock()
        .restart_the_out_of_process_helper_that_died(&full_ctx)
}

fn after_a_helper_process_death(
    id: &ProcessorUniqueId,
    state: &ObservableProcessorState,
    restart_backoff: &mut ChildProcessRestartBackoff,
    exit_reason: String,
    run_before_the_death: Duration,
) -> Option<Instant> {
    let restart_wait = restart_backoff
        .wait_before_starting_again_after_a_death_ending_a_run_of(run_before_the_death);
    state.note_a_helper_process_death(exit_reason.clone());
    if restart_backoff.has_reached_the_crash_loop_limit_of_a_child_running_user_code() {
        let deaths_in_a_row = restart_backoff.deaths_in_a_row_before_a_healthy_run();
        tracing::error!(
            "[{id}] its helper process died {deaths_in_a_row} times in a row, each within \
             {}s of starting; giving up on it: {exit_reason}",
            CHILD_PROCESS_RUN_THAT_RESETS_THE_RESTART_BACKOFF.as_secs(),
        );
        state.fail_with(format!(
            "its helper process died {deaths_in_a_row} times in a row, each within {}s of \
             starting; the last time: {exit_reason}",
            CHILD_PROCESS_RUN_THAT_RESETS_THE_RESTART_BACKOFF.as_secs(),
        ));
        return None;
    }
    tracing::warn!(
        "[{id}] its helper process died ({exit_reason}); a new one starts in {restart_wait:?}"
    );
    state.transition_to(ProcessorState::Restarting);
    Some(Instant::now() + restart_wait)
}

fn dispatch_on_pause(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    runtime_ctx: &RuntimeContext,
) {
    tracing::info!("[{}] Invoking on_pause()...", id);
    let limited_ctx = RuntimeContextLimitedAccess::new(runtime_ctx);
    let mut guard = processor.lock();
    match guard.on_pause(&limited_ctx) {
        Ok(()) => tracing::info!("[{}] on_pause() completed successfully", id),
        Err(e) => tracing::warn!("[{}] on_pause() failed: {}", id, e),
    }
}

fn dispatch_process(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    runtime_ctx: &RuntimeContext,
    state: &ObservableProcessorState,
) {
    let limited_ctx = RuntimeContextLimitedAccess::new(runtime_ctx);
    let mut guard = processor.lock();
    match guard.process(&limited_ctx) {
        Ok(()) => state.note_that_process_ran_cleanly(),
        Err(e) => {
            tracing::warn!("[{}] process() failed: {}", id, e);
            state.note_that_process_raised(|| format!("process() failed: {e}"));
        }
    }
}

fn dispatch_on_resume(
    id: &ProcessorUniqueId,
    processor: &Arc<Mutex<ProcessorInstance>>,
    runtime_ctx: &RuntimeContext,
) {
    tracing::info!("[{}] Invoking on_resume()...", id);
    let limited_ctx = RuntimeContextLimitedAccess::new(runtime_ctx);
    let mut guard = processor.lock();
    match guard.on_resume(&limited_ctx) {
        Ok(()) => tracing::info!("[{}] on_resume() completed successfully", id),
        Err(e) => tracing::warn!("[{}] on_resume() failed: {}", id, e),
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
