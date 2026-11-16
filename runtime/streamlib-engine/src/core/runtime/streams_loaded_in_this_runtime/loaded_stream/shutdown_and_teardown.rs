// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::{HowALoadedStreamEnded, LoadedStreamInThisRuntime, what_a_panic_said};
use crate::core::context::RuntimeContext;
use crate::core::pubsub::RuntimeEvent;
use crate::core::runtime::ArmedTeardownWatchdogOfOneStream;
use crate::core::runtime::streams_loaded_in_this_runtime::stream_actions_of_this_runtime::LoadedStreamHolding;
use crate::core::{Error, Result};

impl LoadedStreamInThisRuntime {
    /// Ask for this stream's shutdown: its level moves to graceful and its own
    /// shutdown thread starts, once. A second request escalates nothing
    /// further.
    pub fn ask_for_this_streams_shutdown(&self, reason: &str) {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        if self.this_streams_shutdown_escalation().raise_to_graceful() {
            tracing::info!(
                reason,
                "the shutdown of the stream `{}` was requested",
                self.stream_id()
            );
            self.publish_on_this_streams_topic(RuntimeEvent::RuntimeShutdown);
        }
        self.start_this_streams_shutdown_thread_once();
    }

    pub fn force_this_streams_shutdown(&self, reason: &str) {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        if self.this_streams_shutdown_escalation().raise_to_forced() {
            tracing::warn!(
                reason,
                "the shutdown of the stream `{}` was forced",
                self.stream_id()
            );
        }
        self.start_this_streams_shutdown_thread_once();
    }

    /// Start the thread that arms this stream's watchdog, stops the stream,
    /// takes it out of the stream table and marks it ended — once.
    fn start_this_streams_shutdown_thread_once(&self) {
        if self.shutdown_thread_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(this_stream) = self.this_stream.upgrade() else {
            return;
        };
        let started = std::thread::Builder::new()
            .name(format!("stream-shutdown-{}", self.stream_id()))
            .spawn(move || this_stream.shut_this_stream_down_under_its_watchdog());
        if let Err(spawn_failure) = started {
            // Released so the next request, or the engine's next walk, retries.
            self.shutdown_thread_started.store(false, Ordering::SeqCst);
            tracing::error!(
                "the shutdown thread of the stream `{}` could not start; the next request \
                 retries it: {spawn_failure}",
                self.stream_id()
            );
        }
    }

    fn shut_this_stream_down_under_its_watchdog(self: Arc<Self>) {
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        let watchdog = ArmedTeardownWatchdogOfOneStream::arm(
            self.stream_id(),
            self.teardown_watchdog_budget,
            self.teardown_progress_note().clone(),
            {
                // Weak, so a disarmed watchdog thread that has not yet seen
                // its disarm holds nothing of the stream or the engine.
                let this_stream = Arc::downgrade(&self);
                move |what_its_teardown_was_waiting_on| {
                    if let Some(this_stream) = this_stream.upgrade() {
                        this_stream.abandon_this_stream_its_teardown_watchdog_fired_on(
                            what_its_teardown_was_waiting_on,
                        );
                    }
                }
            },
        );
        // Caught, so a panicking teardown still leaves the table and is marked
        // ended: its waiters would otherwise wait with nothing to bound them.
        let stop_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.stop()))
            .unwrap_or_else(|teardown_panic| {
                if let Some(this_streams_runtime_context) = self.runtime_context.lock().take() {
                    self.free_the_shared_pool_entries_of_a_stream_whose_teardown_panicked(
                        &this_streams_runtime_context,
                    );
                }
                self.release_every_surface_this_stream_registered();
                Err(Error::Runtime(format!(
                    "its teardown panicked: {}",
                    what_a_panic_said(teardown_panic.as_ref())
                )))
            });
        drop(watchdog);
        if self.end_claimed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.engine_resources_shared_by_every_stream
            .remove_from_the_stream_table(&self);
        let how_it_ended = match stop_outcome {
            Ok(()) => HowALoadedStreamEnded::Stopped,
            Err(stop_failure) => {
                tracing::error!(
                    "the stream `{}` failed to stop: {stop_failure}",
                    self.stream_id()
                );
                HowALoadedStreamEnded::StoppedReportingAFailure(stop_failure.to_string())
            }
        };
        self.let_go_of_this_stream_then_mark_it_ended(how_it_ended);
    }

    /// What this stream's watchdog does on expiry: kill the stream's helper
    /// process groups, count its hung teardown thread as abandoned, and unload
    /// it. Every other stream is left running.
    fn abandon_this_stream_its_teardown_watchdog_fired_on(
        self: Arc<Self>,
        what_its_teardown_was_waiting_on: String,
    ) {
        if self.end_claimed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _entered_this_streams_log_route = self.log_route().enter_on_this_thread();
        let killed_helper_process_groups =
            crate::core::runtime::kill_every_registered_helper_process_group_of_one_stream(
                self.stream_tag(),
            );
        self.engine_resources_shared_by_every_stream
            .remove_from_the_stream_table(&self);
        tracing::error!(
            "the teardown of the stream `{}` did not finish within {}s; it was still waiting on \
             {what_its_teardown_was_waiting_on}. Its {killed_helper_process_groups} helper \
             process group(s) were killed, its teardown thread was abandoned, and it was \
             unloaded; every other stream keeps running.",
            self.stream_id(),
            self.teardown_watchdog_budget.as_secs_f64(),
        );
        crate::core::runtime::count_threads_abandoned_in_this_process(
            1,
            &format!("the teardown of the stream `{}`", self.stream_id()),
            self.stream_id(),
        );
        self.let_go_of_this_stream_then_mark_it_ended(
            HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog {
                what_its_teardown_was_waiting_on,
            },
        );
    }

    /// Write this stream's records to its project's log and its function-node
    /// states to its project's store, let go of the stream, then mark it ended — so a
    /// waiter that wakes finds them whole. A stream a restart loads again keeps every
    /// state to resume from; one that lives with its host process forgets them, since
    /// nothing loads it again.
    fn let_go_of_this_stream_then_mark_it_ended(
        self: Arc<Self>,
        how_it_ended: HowALoadedStreamEnded,
    ) {
        self.log_route().write_every_record_it_emitted_and_sync();
        let function_node_states_carried_for_every_stream = &self
            .engine_resources_shared_by_every_stream
            .function_node_states_carried_for_every_stream;
        match self.holding() {
            LoadedStreamHolding::LivesWithTheHostProcess => {
                function_node_states_carried_for_every_stream
                    .forget_every_state_of_the_stream(self.stream_id())
            }
            LoadedStreamHolding::RecordedByTheRuntime => {
                function_node_states_carried_for_every_stream
                    .keep_the_states_of_a_stream_that_ended(self.stream_id())
            }
        }
        let the_end_of_this_stream = Arc::clone(&self.the_end_of_this_stream);
        drop(self);
        the_end_of_this_stream.mark(how_it_ended);
    }

    /// How this stream ended, `None` while it has not.
    pub fn how_this_stream_ended(&self) -> Option<HowALoadedStreamEnded> {
        self.the_end_of_this_stream.how_it_ended.lock().clone()
    }

    /// Whether this stream has ended: shut down and out of the stream table.
    pub fn has_ended(&self) -> bool {
        self.the_end_of_this_stream.is_recorded()
    }

    /// Block for at most `budget` until this stream has ended, saying whether
    /// it has.
    pub(crate) fn wait_for_this_streams_end_within(&self, budget: Duration) -> bool {
        let the_end = &self.the_end_of_this_stream;
        let mut how_it_ended = the_end.how_it_ended.lock();
        if how_it_ended.is_none() {
            the_end.has_ended.wait_for(&mut how_it_ended, budget);
        }
        how_it_ended.is_some()
    }

    /// Free the shared pool entries a stream whose teardown panicked last
    /// acquired, as its stop would have.
    fn free_the_shared_pool_entries_of_a_stream_whose_teardown_panicked(
        &self,
        this_streams_runtime_context: &RuntimeContext,
    ) {
        // The panic may have come from this freeing; a second one must not
        // escape the shutdown thread before the stream is marked ended.
        let freeing_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            this_streams_runtime_context
                .gpu
                .free_every_unheld_shared_pool_entry_the_stream_last_acquired(self.stream_tag())
        }));
        if let Err(freeing_panic) = freeing_outcome {
            tracing::error!(
                "freeing the shared pool entries of the stream `{}` panicked; they are freed \
                 once idle past the window instead: {}",
                self.stream_id(),
                what_a_panic_said(freeing_panic.as_ref())
            );
        }
    }

    /// Release every surface this stream registered with the engine's
    /// surface-sharing service, which keeps a registration from this process
    /// past its connection's close.
    pub(super) fn release_every_surface_this_stream_registered(&self) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.engine_resources_shared_by_every_stream
            .release_every_surface_registered_by(&self.surface_owner_key());
    }

    /// How this stream ended as its waiters report it: `Ok` for a clean stop,
    /// else the failure, and `Ok` while it has not ended.
    pub(crate) fn how_this_stream_ended_as_a_waiter_reports_it(&self) -> Result<()> {
        match self.how_this_stream_ended() {
            None | Some(HowALoadedStreamEnded::Stopped) => Ok(()),
            Some(HowALoadedStreamEnded::StoppedReportingAFailure(stop_failure)) => {
                Err(Error::Runtime(format!(
                    "the stream `{}` failed to stop: {stop_failure}",
                    self.stream_id()
                )))
            }
            Some(HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog {
                what_its_teardown_was_waiting_on,
            }) => Err(Error::Runtime(format!(
                "the teardown of the stream `{}` outlived its watchdog while waiting on \
                 {what_its_teardown_was_waiting_on}; the stream was abandoned and unloaded",
                self.stream_id()
            ))),
        }
    }

    pub fn processor_threads_abandoned_and_still_running(
        &self,
    ) -> Vec<crate::core::runtime::ProcessorDisplayNameAndId> {
        self.compiler
            .processor_threads_abandoned_and_still_running()
    }

    /// Kill any describe this stream is running and refuse every later one —
    /// its host's user interrupted the load that started it.
    pub fn interrupt_every_processor_interpreter_describe(&self) {
        self.processor_interpreter_launch_record
            .interrupt_every_describe();
    }
}
