// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor thread runner.
//!
//! Handles the main loop for processor threads based on their execution mode.

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

/// Run the processor thread main loop based on execution mode.
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

    // Teardown — privileged ctx. Gated by the isolation trust axis: an
    // untrusted tier yields no `FullAccessGrant`, so no in-process FullAccess
    // teardown runs (privileged lifecycle belongs behind the subprocess
    // sandbox). An untrusted processor never ran its setup in-process either,
    // so there is nothing to tear down here.
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

/// A processor thread's [`ProcessorSchedulingLoopHost`]: the processor
/// instance, the shutdown channel and its wake fd, the pause gate, and the
/// stream clock and step ledger entry its callbacks are counted on.
struct ProcessorSchedulingLoopHostOnAnEngineThread<'a, CallProcessor> {
    processor: &'a Arc<Mutex<ProcessorInstance>>,
    shutdown_rx: &'a crossbeam_channel::Receiver<()>,
    /// Owned for as long as the loop runs, which registers it by its raw fd.
    #[cfg(unix)]
    shutdown_wake_fd: Option<OwnedFd>,
    pause_gate: &'a AtomicBool,
    stream_clock: &'a StreamClock,
    step_progress: &'a StepProgressOfOneNode,
    /// Makes one callback on the processor; told whether to count it on the
    /// step ledger.
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

/// Make `callback` on `processor`, counted in flight on `step_progress` when
/// `counted_on_the_step_ledger` and its stream clock is not real time.
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

/// The scheduling-loop host of the processor a thread runs.
#[allow(clippy::too_many_arguments)]
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
    // Call start() - for callback-driven processors this returns immediately
    // after registering callbacks with OS (AVFoundation, CoreAudio, CVDisplayLink).
    // start() is resource-lifecycle, so it receives full-access ctx. Gated by
    // the isolation trust axis: an untrusted tier yields no `FullAccessGrant`,
    // so an in-process FullAccess start() is unrepresentable — the untrusted
    // processor's privileged lifecycle belongs behind the subprocess sandbox.
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
                // Marked here or nowhere: this thread goes straight to
                // teardown, and a processor whose `start()` failed is not
                // running — a reader that saw `Running` between `setup` and
                // here must be able to find out it was wrong.
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
            // A processor whose work happens elsewhere reports a failure here
            // or nowhere. The loop keeps running rather than breaking: the rest
            // of the pipeline is unaffected, and breaking would run teardown
            // and then overwrite the state with `Stopped`, hiding what
            // happened.
            //
            // Asked before it is read, under one lock: a helper process that
            // died on its own is noticed here, by the process rather than by
            // its escalate socket, whose EOF a surviving descendant defers
            // indefinitely.
            let (
                failure_the_node_reported_of_itself,
                has_failed_unrecoverably,
                why_the_helper_process_died_on_its_own,
                why_process_is_raising_as_its_helper_reports,
            ) = {
                let mut processor = processor.lock();
                processor.detect_and_clean_up_after_an_out_of_process_helper_that_died();
                (
                    processor.failure_the_node_reported_of_itself(),
                    processor.has_failed_unrecoverably(),
                    processor.why_the_out_of_process_helper_died_on_its_own(),
                    processor.why_process_is_raising_as_its_helper_reports(),
                )
            };
            state.note_why_process_is_raising(why_process_is_raising_as_its_helper_reports);
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

/// Start a new helper process in place of one that died on its own, under a
/// fresh full-access grant as the processor's first `setup()` and `start()`
/// had, once any promotion the death asked for has settled — waited on without
/// holding the processor, since the compile carrying it may need to.
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

/// Record a helper process death for `exit_reason` after a run of
/// `run_before_the_death`, and return when to start the next helper — or
/// `None` once the crash-loop limit is reached and the processor has failed.
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
    // block_on is internal to ProcessorInstance::on_pause's dispatch.
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
    // block_on is internal to ProcessorInstance::on_resume's dispatch.
    match guard.on_resume(&limited_ctx) {
        Ok(()) => tracing::info!("[{}] on_resume() completed successfully", id),
        Err(e) => tracing::warn!("[{}] on_resume() failed: {}", id, e),
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use crate::core::execution::processor_scheduling_loop::{
        ContinuousLoopWaitOutcome, PAUSE_CHECK_INTERVAL, ReactiveLoopFdWaiter,
        ReactiveLoopWakeOutcome, next_continuous_deadline_ns, wait_for_a_deadline_or_a_control,
    };
    use crate::core::graph::ShutdownChannelComponent;
    use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
    use iceoryx2::prelude::*;
    use std::os::fd::AsRawFd;
    use std::os::fd::OwnedFd;
    use std::time::Instant;

    fn unique_suffix(tag: &str) -> String {
        format!(
            "test/runner/{tag}/{}",
            mint_machine_global_unique_name_suffix()
        )
    }

    /// A production shutdown channel and a duplicate of its wake fd, as the
    /// spawn op hands one to a reactive runner.
    fn shutdown_channel_and_its_wake_fd() -> (ShutdownChannelComponent, OwnedFd) {
        let shutdown_channel = ShutdownChannelComponent::new();
        let shutdown_wake_fd = shutdown_channel
            .try_clone_shutdown_wake_fd()
            .expect("the shutdown wake fd duplicates");
        (shutdown_channel, shutdown_wake_fd)
    }

    /// Whether any of `processor`'s input ports would hand a reader
    /// something; `None` when it holds no mailboxes to ask.
    fn ports_would_return_something(processor: &Arc<Mutex<ProcessorInstance>>) -> Option<bool> {
        processor
            .lock()
            .iceoryx2_input_mailboxes_inner()
            .map(|mailboxes| mailboxes.any_port_has_data())
    }

    fn drain_input_listener(processor: &Arc<Mutex<ProcessorInstance>>) {
        if let Some(mailboxes) = processor.lock().iceoryx2_input_mailboxes_inner() {
            mailboxes.drain_listener();
        }
    }

    /// Run the reactive scheduling loop over `processor` on an engine thread's
    /// host, recording each callback through `make_callback` rather than
    /// dispatching it.
    fn run_reactive_scheduling_loop_recording_callbacks(
        processor: &Arc<Mutex<ProcessorInstance>>,
        shutdown_rx: &crossbeam_channel::Receiver<()>,
        shutdown_wake_fd: Option<OwnedFd>,
        pause_gate: &AtomicBool,
        mut make_callback: impl FnMut(ProcessorSchedulingCallback),
    ) {
        let stream_clock = StreamClock::starting_now(crate::core::context::StreamClockSpec::Real);
        let step_progress = StepProgressOfOneNode::default();
        let mut host = ProcessorSchedulingLoopHostOnAnEngineThread {
            processor,
            shutdown_rx,
            shutdown_wake_fd,
            pause_gate,
            stream_clock: &stream_clock,
            step_progress: &step_progress,
            call_processor: |callback, _counted_on_the_step_ledger| make_callback(callback),
        };
        run_reactive_scheduling_loop("Preactive-scheduling", &mut host);
    }

    /// Wait on `stream_clock` for `deadline_ns` the way a continuous loop does,
    /// on an engine thread's host over `shutdown_rx`.
    fn wait_for_stream_time_or_shutdown(
        stream_clock: &StreamClock,
        deadline_ns: i64,
        shutdown_rx: &crossbeam_channel::Receiver<()>,
        stops_waiting_early: impl Fn() -> bool,
    ) -> ContinuousLoopWaitOutcome {
        let (processor, _mailboxes) = input_only_processor_with_plain_port(None);
        let step_progress = StepProgressOfOneNode::default();
        let pause_gate = AtomicBool::new(false);
        let mut host = ProcessorSchedulingLoopHostOnAnEngineThread {
            processor: &processor,
            shutdown_rx,
            shutdown_wake_fd: None,
            pause_gate: &pause_gate,
            stream_clock,
            step_progress: &step_progress,
            call_processor: |_, _| {},
        };
        wait_for_a_deadline_or_a_control(&mut host, stream_clock, deadline_ns, stops_waiting_early)
    }

    fn listener_fd_of(
        listener: &iceoryx2::port::listener::Listener<ipc_threadsafe::Service>,
    ) -> i32 {
        // SAFETY: every caller uses the fd only while `listener` is alive.
        unsafe { listener.file_descriptor().native_handle() }
    }

    /// The rule the window contract turns on: a reactive processor is never
    /// dispatched with nothing to read. A bag that does not complete a window
    /// wakes the runner and must not make it call `process()`.
    ///
    /// Fail-without-fix: drop the `if !ports_would_return_something(...)` guard
    /// above the drain loop and this goes green while the contract is broken —
    /// which is why the gate is asserted here rather than only through
    /// `has_data`.
    #[test]
    fn a_bag_that_does_not_complete_a_window_does_not_dispatch_the_reactive_runner() {
        use crate::core::test_support::MockWindowedAudioConsumerProcessor;

        let mut instance = ProcessorInstance::new(Box::new(
            <MockWindowedAudioConsumerProcessor::Processor as crate::core::GeneratedProcessor>
                ::from_config(Default::default())
                .expect("the mock constructs from its default config"),
        ));
        instance
            .install_iceoryx2_resources()
            .expect("the mock accepts its iceoryx2 resources");
        let mailboxes = instance
            .iceoryx2_input_mailboxes_inner()
            .expect("a windowed mock holds input mailboxes");
        mailboxes.add_windowed_port(
            "audio",
            crate::iceoryx2::ReadMode::ReadNextInOrder,
            windowed_mock_contract(),
        );
        let processor = Arc::new(Mutex::new(instance));

        assert_eq!(
            ports_would_return_something(&processor),
            Some(false),
            "an empty windowed port must not dispatch"
        );

        // A third of the declared 512-sample window.
        let link_into_audio = a_test_link_into(&mailboxes, "audio");
        link_into_audio.send(&one_mono_audio_frame_for(160, 0));
        assert_eq!(
            ports_would_return_something(&processor),
            Some(false),
            "160 of 512 samples is not a window, and dispatching here would hand \
             `process()` an empty read"
        );

        for block in 1..4i64 {
            link_into_audio.send(&one_mono_audio_frame_for(
                160,
                block * 160 * 1_000_000_000 / 16_000,
            ));
        }
        assert_eq!(
            ports_would_return_something(&processor),
            Some(true),
            "640 samples completes a window, and a ready window must not sit latent"
        );
    }

    /// A processor the runner cannot ask is dispatched rather than silenced —
    /// the pre-dispatch gate's default, which is the opposite of the drain
    /// loop's and is why the helper hands back an `Option` instead of picking.
    #[test]
    fn a_processor_with_no_input_mailboxes_is_not_gated_at_all() {
        use crate::core::test_support::MockOutputOnlyProcessor;

        let instance = ProcessorInstance::new(Box::new(
            <MockOutputOnlyProcessor::Processor as crate::core::GeneratedProcessor>::from_config(
                Default::default(),
            )
            .expect("the mock constructs from its default config"),
        ));
        let processor = Arc::new(Mutex::new(instance));

        assert_eq!(
            ports_would_return_something(&processor),
            None,
            "there is nothing to gate on, and each caller says what that means"
        );
    }

    fn windowed_mock_contract() -> crate::iceoryx2::ResolvedAudioWindowContract {
        crate::iceoryx2::ResolvedAudioWindowContract::from_declared_values(
            &crate::core::descriptors::AudioWindowContractDeclaredValues {
                sample_rate: 16_000,
                channels: Some(1),
                dtype: "f32".to_string(),
                window_size: 512,
                hop: 512,
            },
        )
        .expect("the mock's own declaration resolves")
    }

    /// One sample carrying `frames` mono 16 kHz samples as an audio block bag.
    fn one_mono_audio_frame_for(
        frames: usize,
        first_sample_timestamp_ns: i64,
    ) -> crate::iceoryx2::channel_sample_test_support::TestChannelSample {
        crate::iceoryx2::channel_sample_test_support::TestChannelSample::of_a_mono_channels_block(
            frames,
            16_000,
            first_sample_timestamp_ns,
        )
    }

    /// A reactive processor's scheduling loop running on its own thread, as the
    /// runner runs it, with its processor callbacks recorded and each dispatch
    /// reading one bag off `in1`.
    struct ReactiveSchedulingLoopOnItsOwnThread {
        processor_callbacks: Arc<Mutex<Vec<ProcessorSchedulingCallback>>>,
        shutdown_channel: ShutdownChannelComponent,
        pause_gate: Arc<AtomicBool>,
        loop_thread: std::thread::JoinHandle<()>,
    }

    /// Whether the pause gate is already closed when the loop's thread starts.
    #[derive(Clone, Copy)]
    enum PauseGateAtLoopStart {
        Open,
        Closed,
    }

    impl ReactiveSchedulingLoopOnItsOwnThread {
        fn start(
            processor: Arc<Mutex<ProcessorInstance>>,
            mailboxes: Arc<crate::iceoryx2::InputMailboxesInner>,
            each_dispatch_takes: std::time::Duration,
            pause_gate_at_loop_start: PauseGateAtLoopStart,
        ) -> Self {
            let processor_callbacks: Arc<Mutex<Vec<ProcessorSchedulingCallback>>> = Arc::default();
            let (mut shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
            let shutdown_receiver = shutdown_channel
                .take_receiver()
                .expect("a fresh shutdown channel holds its receiver");
            let pause_gate = Arc::new(AtomicBool::new(matches!(
                pause_gate_at_loop_start,
                PauseGateAtLoopStart::Closed
            )));
            let loop_thread = std::thread::spawn({
                let processor_callbacks = Arc::clone(&processor_callbacks);
                let pause_gate = Arc::clone(&pause_gate);
                move || {
                    run_reactive_scheduling_loop_recording_callbacks(
                        &processor,
                        &shutdown_receiver,
                        Some(shutdown_wake_fd),
                        &pause_gate,
                        |callback| {
                            if callback == ProcessorSchedulingCallback::Process {
                                let _ = mailboxes.read_raw("in1");
                                std::thread::sleep(each_dispatch_takes);
                            }
                            processor_callbacks.lock().push(callback);
                        },
                    )
                }
            });
            Self {
                processor_callbacks,
                shutdown_channel,
                pause_gate,
                loop_thread,
            }
        }

        fn count_of(&self, callback: ProcessorSchedulingCallback) -> usize {
            self.processor_callbacks
                .lock()
                .iter()
                .filter(|made| **made == callback)
                .count()
        }

        /// Whether `callback` has been made `times` times within `deadline`.
        fn made_within(
            &self,
            callback: ProcessorSchedulingCallback,
            times: usize,
            deadline: std::time::Duration,
        ) -> bool {
            let started = std::time::Instant::now();
            while started.elapsed() < deadline {
                if self.count_of(callback) >= times {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            self.count_of(callback) >= times
        }

        fn stop(self) {
            self.shutdown_channel.signal_shutdown();
            self.loop_thread
                .join()
                .expect("the scheduling loop must not panic");
        }
    }

    /// An input-only processor with a plain `in1` mailbox and, when given one,
    /// the listener its runner waits on.
    fn input_only_processor_with_plain_port(
        listener: Option<iceoryx2::port::listener::Listener<ipc_threadsafe::Service>>,
    ) -> (
        Arc<Mutex<ProcessorInstance>>,
        Arc<crate::iceoryx2::InputMailboxesInner>,
    ) {
        use crate::core::test_support::MockInputOnlyProcessor;

        let mut instance = ProcessorInstance::new(Box::new(
            <MockInputOnlyProcessor::Processor as crate::core::GeneratedProcessor>::from_config(
                Default::default(),
            )
            .expect("the mock constructs from its default config"),
        ));
        instance
            .install_iceoryx2_resources()
            .expect("the mock accepts its iceoryx2 resources");
        let mailboxes = instance
            .iceoryx2_input_mailboxes_inner()
            .expect("an input-only mock holds input mailboxes");
        mailboxes.add_port("in1", 16, crate::iceoryx2::ReadMode::ReadNextInOrder);
        if let Some(listener) = listener {
            mailboxes.set_listener(listener);
        }
        (Arc::new(Mutex::new(instance)), mailboxes)
    }

    /// One wire frame stamped for `in1`, carrying four opaque bytes.
    fn one_frame_for_in1() -> crate::iceoryx2::channel_sample_test_support::TestChannelSample {
        crate::iceoryx2::channel_sample_test_support::TestChannelSample::carrying(0, &[0; 4])
    }

    /// A test link bound into `port` of `mailboxes`.
    fn a_test_link_into(
        mailboxes: &crate::iceoryx2::InputMailboxesInner,
        port: &str,
    ) -> crate::iceoryx2::channel_sample_test_support::TestLinkIntoAnInputPort {
        crate::iceoryx2::channel_sample_test_support::TestLinkIntoAnInputPort::bound_into(
            mailboxes, port, 16,
        )
    }

    fn open_one_listener_event_service(
        node: &iceoryx2::node::Node<ipc_threadsafe::Service>,
        tag: &str,
    ) -> iceoryx2::service::port_factory::event::PortFactory<ipc_threadsafe::Service> {
        node.service_builder(&ServiceName::new(&unique_suffix(tag)).unwrap())
            .event()
            .max_notifiers(1)
            .max_listeners(1)
            .open_or_create()
            .unwrap()
    }

    /// A reactive processor slower than its upstream never leaves its dispatch
    /// loop while bags keep arriving, and every bag notifies — and every notify
    /// is still delivered, the guarantee that lets every destination be
    /// notified whatever mode it runs in.
    ///
    /// A 2 ms `process()` against a 1 kHz source.
    /// Fail-without-fix: an event transport queuing a wakeup per send comes
    /// back undelivered about 280 frames in once the dispatch loop stops
    /// draining.
    #[test]
    fn a_processor_slower_than_its_upstream_keeps_every_notify_deliverable() {
        const FRAMES_AT_ONE_KILOHERTZ: usize = 1000;

        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = open_one_listener_event_service(&node, "slow-consumer");
        let notifier = service.notifier_builder().create().unwrap();
        let (processor, mailboxes) = input_only_processor_with_plain_port(Some(
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
                .unwrap(),
        ));
        let link_into_in1 = a_test_link_into(&mailboxes, "in1");
        let running = ReactiveSchedulingLoopOnItsOwnThread::start(
            processor,
            Arc::clone(&mailboxes),
            std::time::Duration::from_millis(2),
            PauseGateAtLoopStart::Open,
        );

        let mut undelivered_notifies = 0;
        for _ in 0..FRAMES_AT_ONE_KILOHERTZ {
            link_into_in1.send(&one_frame_for_in1());
            if notifier.notify().unwrap() == 0 {
                undelivered_notifies += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let dispatches = running.count_of(ProcessorSchedulingCallback::Process);
        running.stop();

        assert!(
            dispatches >= 100,
            "the consumer must have been kept busy for the burst; it dispatched {dispatches} times"
        );
        assert_eq!(
            undelivered_notifies, 0,
            "{undelivered_notifies} of {FRAMES_AT_ONE_KILOHERTZ} notifies reached nobody"
        );
    }

    /// Bags that arrive while a reactive processor is paused are notified, and
    /// the paused ticks drain those notifications — so after the resume there
    /// is nothing left to wake on, and the runner has to ask before it waits.
    ///
    /// The ticket's repro: pause, publish three bags, resume, publish nothing
    /// more. Fail-without-fix: drop the check before the wait and `process()`
    /// never runs.
    #[test]
    fn bags_that_arrived_during_a_pause_are_dispatched_after_the_resume_with_nothing_more_published()
     {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = open_one_listener_event_service(&node, "paused-consumer");
        let notifier = service.notifier_builder().create().unwrap();
        let (processor, mailboxes) = input_only_processor_with_plain_port(Some(
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
                .unwrap(),
        ));
        let link_into_in1 = a_test_link_into(&mailboxes, "in1");
        let running = ReactiveSchedulingLoopOnItsOwnThread::start(
            processor,
            Arc::clone(&mailboxes),
            std::time::Duration::ZERO,
            PauseGateAtLoopStart::Closed,
        );
        assert!(
            running.made_within(
                ProcessorSchedulingCallback::OnPause,
                1,
                std::time::Duration::from_secs(5)
            ),
            "the loop must see the pause"
        );

        for _ in 0..3 {
            link_into_in1.send(&one_frame_for_in1());
            notifier.notify().unwrap();
        }
        // Several paused ticks, each of which drains the listener.
        std::thread::sleep(PAUSE_CHECK_INTERVAL * 5);
        running.pause_gate.store(false, Ordering::Release);

        let dispatched = running.made_within(
            ProcessorSchedulingCallback::Process,
            3,
            std::time::Duration::from_secs(2),
        );
        let dispatches = running.count_of(ProcessorSchedulingCallback::Process);
        running.stop();
        assert!(
            dispatched,
            "three bags were waiting at the resume and {dispatches} were dispatched"
        );
    }

    /// A first live link creates its subscriber before its listener, so a bag
    /// published in between notifies nobody and waits in the subscriber. The
    /// runner reaches its next pass with that bag queued and a listener holding
    /// nothing, which is the state built here before the loop starts.
    ///
    /// Fail-without-fix: drop the check before the wait and the runner sleeps
    /// on the empty listener until the upstream publishes again.
    #[test]
    fn a_bag_queued_before_the_listener_existed_is_dispatched_with_no_notify() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = open_one_listener_event_service(&node, "first-link-gap");
        let (processor, mailboxes) = input_only_processor_with_plain_port(None);
        let link_into_in1 = a_test_link_into(&mailboxes, "in1");
        link_into_in1.send(&one_frame_for_in1());
        mailboxes.set_listener(
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
                .unwrap(),
        );

        let running = ReactiveSchedulingLoopOnItsOwnThread::start(
            processor,
            mailboxes,
            std::time::Duration::ZERO,
            PauseGateAtLoopStart::Open,
        );
        let dispatched = running.made_within(
            ProcessorSchedulingCallback::Process,
            1,
            std::time::Duration::from_secs(2),
        );
        running.stop();
        assert!(
            dispatched,
            "the queued bag must be dispatched without waiting for a notify"
        );
    }

    /// The drain every reactive tick runs leaves the listener fd not readable
    /// however many notifications piled up behind it, and the next send makes
    /// it readable again — so a wait after a drain wakes only for something new.
    ///
    /// Fail-without-fix: make `drain_input_listener` a no-op and the fd still
    /// reads readable after the drain.
    #[test]
    fn a_drained_listener_waits_for_the_next_send_however_many_came_before() {
        use crate::core::test_support::MockInputOnlyProcessor;

        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = open_one_listener_event_service(&node, "drain-then-wait");
        let notifier = service.notifier_builder().create().unwrap();

        let mut instance = ProcessorInstance::new(Box::new(
            <MockInputOnlyProcessor::Processor as crate::core::GeneratedProcessor>::from_config(
                Default::default(),
            )
            .expect("the mock constructs from its default config"),
        ));
        instance
            .install_iceoryx2_resources()
            .expect("the mock accepts its iceoryx2 resources");
        let input_mailboxes = instance
            .iceoryx2_input_mailboxes_inner()
            .expect("an input-only mock holds input mailboxes");
        input_mailboxes.set_listener(
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
                .unwrap(),
        );
        let waiter = ReactiveLoopFdWaiter::new(
            input_mailboxes
                .listener_fd()
                .expect("the listener was just installed"),
            1,
            None,
        )
        .expect("the readiness queue registers the listener fd");
        let processor = Arc::new(Mutex::new(instance));

        for _ in 0..8192 {
            notifier.notify().unwrap();
        }
        drain_input_listener(&processor);
        let outcome_after_the_drain = waiter.wait(std::time::Duration::ZERO);
        assert!(
            matches!(outcome_after_the_drain, ReactiveLoopWakeOutcome::TimedOut),
            "a drained listener must not wake the next wait; got {outcome_after_the_drain:?}"
        );

        notifier.notify().unwrap();
        let outcome_after_the_next_send = waiter.wait(std::time::Duration::ZERO);
        assert!(
            matches!(
                outcome_after_the_next_send,
                ReactiveLoopWakeOutcome::Notified
            ),
            "the next send must wake the wait; got {outcome_after_the_next_send:?}"
        );
    }

    /// A waiter on a fresh listener, and the notifier that feeds it.
    struct ListenerWaiterFixture {
        _node: iceoryx2::node::Node<ipc_threadsafe::Service>,
        notifier: iceoryx2::port::notifier::Notifier<ipc_threadsafe::Service>,
        listener: iceoryx2::port::listener::Listener<ipc_threadsafe::Service>,
        shutdown_channel: ShutdownChannelComponent,
        waiter: ReactiveLoopFdWaiter,
        /// Registered by the waiter without being owned by it.
        _shutdown_wake_fd: OwnedFd,
    }

    impl ListenerWaiterFixture {
        fn new(tag: &str) -> Self {
            let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
            let service = open_one_listener_event_service(&node, tag);
            let notifier = service.notifier_builder().create().unwrap();
            let listener =
                crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(
                    &service,
                )
                .unwrap();
            let (shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
            let waiter = ReactiveLoopFdWaiter::new(
                listener_fd_of(&listener),
                1,
                Some(shutdown_wake_fd.as_raw_fd()),
            )
            .expect("the readiness queue registers both fds");
            Self {
                _node: node,
                notifier,
                listener,
                shutdown_channel,
                waiter,
                _shutdown_wake_fd: shutdown_wake_fd,
            }
        }
    }

    /// The waiter reports the listener's readiness. Level-triggered, so the
    /// notify lands first and the wait is a zero-bound poll.
    #[test]
    fn a_delivered_notify_makes_the_wait_report_notified() {
        let fixture = ListenerWaiterFixture::new("wait-notified");
        fixture.notifier.notify().unwrap();

        let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::Notified),
            "expected Notified, got {outcome:?}"
        );
        fixture.listener.try_wait(|_| {}).unwrap();
    }

    /// The production shutdown signal makes the waiter report shutdown, with
    /// no listener activity at all.
    #[test]
    fn a_signalled_shutdown_makes_the_wait_report_shutdown() {
        let fixture = ListenerWaiterFixture::new("wait-shutdown");
        fixture.shutdown_channel.signal_shutdown();

        let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::ControlArrived),
            "expected Shutdown, got {outcome:?}"
        );
    }

    /// A runner woken by both exits rather than draining one more frame.
    #[test]
    fn shutdown_outranks_a_notify_reported_by_the_same_wait() {
        let fixture = ListenerWaiterFixture::new("wait-both");
        fixture.notifier.notify().unwrap();
        fixture.shutdown_channel.signal_shutdown();

        let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::ControlArrived),
            "expected Shutdown, got {outcome:?}"
        );
        fixture.listener.try_wait(|_| {}).unwrap();
    }

    /// A wait with nothing to report returns on its own, which is what lets a
    /// runner whose listener was replaced while it slept come round to rebuild
    /// rather than sleeping on a dead fd until shutdown.
    #[test]
    fn a_wait_with_nothing_to_report_times_out() {
        let fixture = ListenerWaiterFixture::new("wait-idle");

        let outcome = fixture.waiter.wait(std::time::Duration::ZERO);

        assert!(
            matches!(outcome, ReactiveLoopWakeOutcome::TimedOut),
            "an idle wait must time out rather than block; got {outcome:?}"
        );
    }

    /// The one cross-thread check: a shutdown made visible only on the wake
    /// fd ends a reactive loop blocked in its wait. The crossbeam half of the
    /// shutdown is withheld, so the channel-poll fallback never exits here.
    /// The deadline is generous and nothing is asserted about latency.
    #[test]
    fn a_shutdown_seen_only_on_the_wake_fd_ends_a_loop_blocked_in_its_wait() {
        let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
        let service = open_one_listener_event_service(&node, "loop-wake-fd-shutdown");
        let (processor, _mailboxes) = input_only_processor_with_plain_port(Some(
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&service)
                .unwrap(),
        ));
        let (shutdown_channel, shutdown_wake_fd) = shutdown_channel_and_its_wake_fd();
        let (withheld_shutdown_sender, withheld_shutdown_receiver) = crossbeam_channel::bounded(1);
        let (loop_exited_sender, loop_exited_receiver) = crossbeam_channel::bounded(1);
        let loop_thread = std::thread::spawn(move || {
            run_reactive_scheduling_loop_recording_callbacks(
                &processor,
                &withheld_shutdown_receiver,
                Some(shutdown_wake_fd),
                &AtomicBool::new(false),
                |_| {},
            );
            let _ = loop_exited_sender.send(());
        });

        shutdown_channel.signal_shutdown();
        let exited_on_the_wake_fd = loop_exited_receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok();
        let _ = withheld_shutdown_sender.send(());
        loop_thread
            .join()
            .expect("the scheduling loop must not panic");

        assert!(
            exited_on_the_wake_fd,
            "the loop never saw the wake fd's shutdown, so it is not waiting on it"
        );
    }

    /// A helper that keeps dying soon after it starts reads `Restarting`, with
    /// the last death's reason, until its fifth death in a row fails the
    /// processor with that reason; a helper that ran healthily starts again.
    ///
    /// Fail-without-fix: skip the crash-loop check and a processor whose
    /// `setup()` always raises reads `Restarting` forever.
    #[test]
    fn a_helper_dying_soon_after_each_start_is_restarted_four_times_then_fails() {
        let id = ProcessorUniqueId::from("Pblur");
        let state = ObservableProcessorState::new(ProcessorState::Running);
        let mut restart_backoff = ChildProcessRestartBackoff::new();

        for death in 1..=4 {
            let next_start = after_a_helper_process_death(
                &id,
                &state,
                &mut restart_backoff,
                format!("its helper process ended with exit status: {death}"),
                Duration::from_millis(200),
            );
            assert!(next_start.is_some(), "death {death} gave up");
            assert_eq!(state.current(), ProcessorState::Restarting);
        }
        let fifth = after_a_helper_process_death(
            &id,
            &state,
            &mut restart_backoff,
            "its helper process ended with exit status: 5".to_string(),
            Duration::ZERO,
        );

        assert!(fifth.is_none());
        assert_eq!(state.current(), ProcessorState::Error);
        let failure_reason = state.failure_reason().expect("the failure is explained");
        assert!(
            failure_reason.contains("died 5 times in a row")
                && failure_reason.ends_with("exit status: 5"),
            "{failure_reason}"
        );
        assert_eq!(
            state
                .helper_process_restart_record()
                .last_exit_reason
                .as_deref(),
            Some("its helper process ended with exit status: 5")
        );
    }

    #[test]
    fn a_helper_that_ran_healthily_before_dying_starts_again_after_the_first_wait() {
        let id = ProcessorUniqueId::from("Pblur");
        let state = ObservableProcessorState::new(ProcessorState::Running);
        let mut restart_backoff = ChildProcessRestartBackoff::new();
        let before = Instant::now();

        let next_start = after_a_helper_process_death(
            &id,
            &state,
            &mut restart_backoff,
            "its helper process ended with signal: 11 (SIGSEGV)".to_string(),
            Duration::from_secs(3600),
        )
        .expect("a healthy helper is never given up on");

        assert_eq!(state.current(), ProcessorState::Restarting);
        assert!(next_start >= before + Duration::from_secs(1));
        assert!(next_start < before + Duration::from_secs(2));
    }

    #[test]
    fn a_continuous_deadline_steps_one_interval_on_its_grid() {
        assert_eq!(next_continuous_deadline_ns(1_000, 100, 1_050), 1_100);
        assert_eq!(next_continuous_deadline_ns(1_000, 100, 900), 1_100);
    }

    #[test]
    fn an_overrun_skips_to_the_next_deadline_after_now_on_the_same_grid() {
        assert_eq!(next_continuous_deadline_ns(1_000, 100, 1_350), 1_400);
        assert_eq!(next_continuous_deadline_ns(1_000, 100, 1_400), 1_500);
    }

    fn a_scaled_stream_clock(scale: f64) -> StreamClock {
        StreamClock::starting_now(crate::core::context::StreamClockSpec::Scaled {
            scale,
            step_ns: crate::core::context::DEFAULT_SCALED_STREAM_CLOCK_STEP_NS,
        })
    }

    #[test]
    fn a_wait_on_a_half_speed_stream_takes_twice_the_stream_time_on_the_machine_clock() {
        let stream_clock = a_scaled_stream_clock(0.5);
        let (_shutdown_tx, shutdown_rx) = crossbeam_channel::unbounded::<()>();
        let machine_started = Instant::now();
        let deadline_stream_ns = stream_clock.now_ns() + 20_000_000;

        let outcome = wait_for_stream_time_or_shutdown(
            &stream_clock,
            deadline_stream_ns,
            &shutdown_rx,
            || false,
        );

        assert_eq!(outcome, ContinuousLoopWaitOutcome::DeadlineReached);
        assert!(machine_started.elapsed() >= Duration::from_millis(40));
        assert!(stream_clock.now_ns() >= deadline_stream_ns);
    }

    #[test]
    fn a_rescale_reaches_a_wait_already_under_way() {
        let stream_clock = Arc::new(a_scaled_stream_clock(0.001));
        let (_shutdown_tx, shutdown_rx) = crossbeam_channel::unbounded::<()>();
        let deadline_stream_ns = stream_clock.now_ns() + 50_000_000;
        let rescaling_clock = Arc::clone(&stream_clock);
        let rescaler = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            rescaling_clock
                .rescale("warped", 1.0)
                .expect("a scaled clock rescales");
        });
        let machine_started = Instant::now();

        let outcome = wait_for_stream_time_or_shutdown(
            &stream_clock,
            deadline_stream_ns,
            &shutdown_rx,
            || false,
        );

        rescaler.join().expect("the rescaling thread finishes");
        assert_eq!(outcome, ContinuousLoopWaitOutcome::DeadlineReached);
        assert!(machine_started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_shutdown_ends_a_continuous_wait_before_its_deadline() {
        let stream_clock = a_scaled_stream_clock(0.001);
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::unbounded::<()>();
        shutdown_tx.send(()).expect("the shutdown channel is open");

        let outcome = wait_for_stream_time_or_shutdown(
            &stream_clock,
            stream_clock.now_ns() + 1_000_000_000,
            &shutdown_rx,
            || false,
        );

        assert_eq!(outcome, ContinuousLoopWaitOutcome::Stopped);
    }
}
