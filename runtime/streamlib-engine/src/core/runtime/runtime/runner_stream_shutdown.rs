// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Unloading streams, waiting on them to end, and shutting the engine down.

use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::core::Result;
use crate::core::runtime::RuntimeShutdownEscalation;
use crate::core::runtime::loaded_stream::LoadedStreamInThisRuntime;

#[cfg(target_os = "linux")]
use super::runner_linux_surface_share::stop_the_surface_share_service;
#[cfg(target_os = "macos")]
use super::runner_macos_surface_share::stop_the_surface_share_service;
use super::{
    EveryStreamEndedDuringTheWait, Runner, TheMachineShutdownSignalsOwnedByOneEngine,
    request_a_streams_shutdown_and_wait_until_it_has_ended, take_shutdown_signal_ownership,
};

impl Runner {
    /// Shut the stream `stream_id` names down and wait until it has ended —
    /// stopped and out of the table, or abandoned by its watchdog — reporting
    /// how it ended.
    pub fn unload_stream(&self, stream_id: &str) -> Result<()> {
        let stream = self.loaded_stream_of_the_id(stream_id)?;
        request_a_streams_shutdown_and_wait_until_it_has_ended(
            &stream,
            "the stream is being unloaded",
        );
        stream.how_this_stream_ended_as_a_waiter_reports_it()
    }

    /// Raise the machine's shutdown level to graceful, exactly as a first
    /// signal does: whoever owns the machine's shutdown signals walks every
    /// loaded stream to it.
    pub fn request_the_shutdown_of_every_loaded_stream(&self, reason: &str) -> Result<()> {
        crate::core::runtime::request_the_shutdown_of_every_loaded_stream(reason)
    }

    /// Run `run` while owning the machine's shutdown signals (SIGINT, SIGTERM,
    /// SIGHUP) — the waits inside it walk every loaded stream to the machine's
    /// shutdown level — then clear what this run's interrupts escalated. Fails
    /// if another owner in this process already holds them.
    pub fn run_owning_the_machine_shutdown_signals<R>(
        &self,
        run: impl FnOnce() -> Result<R>,
    ) -> Result<R> {
        let run_outcome = {
            let _shutdown_signals = take_shutdown_signal_ownership()?;
            let _owned_by_this_engine = TheMachineShutdownSignalsOwnedByOneEngine::mark(
                &self.engine_resources_shared_by_every_stream,
            );
            run()
        };
        crate::core::runtime::take_the_machines_shutdown_escalation();
        run_outcome
    }

    /// Block until `stream` has ended — its own shutdown request, a machine
    /// shutdown, an unload or its watchdog — reporting how it ended.
    pub fn wait_until_the_stream_ends(
        &self,
        stream: &Arc<LoadedStreamInThisRuntime>,
    ) -> Result<()> {
        self.block_until(&|| stream.has_ended());
        stream.how_this_stream_ended_as_a_waiter_reports_it()
    }

    /// Block until every loaded stream has ended, including each loaded while
    /// the wait runs and each already out of the table whose end is not yet
    /// recorded, returning every stream it saw and the first that ended with a
    /// failure.
    pub fn wait_until_every_stream_has_ended(&self) -> EveryStreamEndedDuringTheWait {
        let engine = &self.engine_resources_shared_by_every_stream;
        let streams_seen_during_the_wait: Mutex<Vec<Arc<LoadedStreamInThisRuntime>>> =
            Mutex::new(Vec::new());
        self.block_until(&|| {
            let mut streams_seen = streams_seen_during_the_wait.lock();
            for stream in engine.every_stream_still_held_whose_end_is_not_yet_recorded() {
                if !streams_seen.iter().any(|seen| Arc::ptr_eq(seen, &stream)) {
                    streams_seen.push(stream);
                }
            }
            streams_seen.iter().all(|stream| stream.has_ended())
        });
        let streams_seen_during_the_wait = streams_seen_during_the_wait.into_inner();
        let how_the_first_failed_stream_ended = streams_seen_during_the_wait
            .iter()
            .map(|stream| stream.how_this_stream_ended_as_a_waiter_reports_it())
            .find(Result::is_err)
            .unwrap_or(Ok(()));
        EveryStreamEndedDuringTheWait {
            streams_seen_during_the_wait,
            how_the_first_failed_stream_ended,
        }
    }

    /// Block until the machine's shutdown is requested — a signal, or the
    /// request the Rust SDK makes — with zero, one or many streams loaded.
    pub fn wait_until_a_machine_shutdown_is_requested(&self) {
        self.block_until(&crate::core::runtime::is_the_machines_shutdown_requested);
    }

    /// Block, as the waits above do, until `self` is the one reference left to
    /// this engine or `budget` has passed; whether it is.
    pub fn wait_until_this_reference_alone_holds_the_engine(
        self: &Arc<Self>,
        budget: Duration,
    ) -> bool {
        let deadline = Instant::now() + budget;
        self.block_until(&|| Arc::strong_count(self) == 1 || Instant::now() >= deadline);
        Arc::strong_count(self) == 1
    }

    /// Poll until `has_ended` holds — while this engine owns the machine's
    /// shutdown signals, walking every loaded stream to the machine's shutdown
    /// level on the way.
    ///
    /// `has_ended` is read before the walk: the machine's level only rises
    /// until the owner takes it, so the walk reaches every level the read saw,
    /// and a request landing between the two is never seen by the wait and
    /// left unwalked.
    pub(super) fn block_until(&self, has_ended: &dyn Fn() -> bool) {
        let observe = || {
            let ended = has_ended();
            if self
                .engine_resources_shared_by_every_stream
                .owns_the_machine_shutdown_signals
                .load(Ordering::SeqCst)
            {
                self.walk_every_loaded_stream_to_the_machines_shutdown_level();
            }
            if ended {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };

        while observe().is_continue() {
            std::thread::sleep(
                crate::core::runtime::RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL,
            );
        }
    }

    /// Move every loaded stream to the machine's shutdown level at once:
    /// graceful asks each for its shutdown, forced forces each.
    pub(super) fn walk_every_loaded_stream_to_the_machines_shutdown_level(&self) {
        let machines_level = crate::core::runtime::the_machines_shutdown_escalation();
        if machines_level < RuntimeShutdownEscalation::Graceful {
            return;
        }
        for stream in self
            .engine_resources_shared_by_every_stream
            .every_loaded_stream()
        {
            stream.ask_for_this_streams_shutdown("the machine is shutting down every stream");
            if machines_level >= RuntimeShutdownEscalation::Forced {
                stream.force_this_streams_shutdown("the machine's shutdown was forced");
            }
        }
    }

    /// Ask every loaded stream for its shutdown at once — each on its own
    /// thread, under its own watchdog — and wait until each has ended,
    /// including each already out of the table whose end is not yet recorded,
    /// reporting the first that ended with a failure.
    pub(super) fn shut_every_loaded_stream_down_and_wait_until_each_has_ended(
        &self,
        reason: &str,
    ) -> Result<()> {
        let engine = &self.engine_resources_shared_by_every_stream;
        let every_stream = engine.every_stream_still_held_whose_end_is_not_yet_recorded();
        for stream in &every_stream {
            stream.ask_for_this_streams_shutdown(reason);
        }
        let mut first_failed_end = Ok(());
        for stream in &every_stream {
            request_a_streams_shutdown_and_wait_until_it_has_ended(stream, reason);
            if let Err(end_failure) = stream.how_this_stream_ended_as_a_waiter_reports_it()
                && first_failed_end.is_ok()
            {
                first_failed_end = Err(end_failure);
            }
        }
        first_failed_end
    }

    /// Shut every loaded stream down at once, each under its own watchdog,
    /// then stop the surface-sharing service. Idempotent; dropping the
    /// `Runner` runs it. A load after it is refused.
    pub fn shut_down(&self) -> Result<()> {
        let engine = &self.engine_resources_shared_by_every_stream;
        if engine.shut_down.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let first_failed_end = self.shut_every_loaded_stream_down_and_wait_until_each_has_ended(
            "the engine is shutting down",
        );
        crate::core::runtime::note_what_the_engine_teardown_is_waiting_on(
            "the shared native services",
        );
        engine.shared_native_services.stop_every_service();
        engine
            .project_stores_open_in_this_runtime
            .commit_every_waiting_write_of_every_store_now();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        stop_the_surface_share_service(engine);
        tracing::info!("[shut_down] The engine is shut down");
        first_failed_end
    }
}
