// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Hosting the machine's streams: the engine built over the state directory,
//! the local API served at its fixed socket, every kept stream not stopped
//! re-loaded, the run until a machine shutdown is requested and every stream
//! has ended, and a teardown that drops the engine — or leaves it beneath the
//! threads a stream abandoned.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use streamlib::sdk::logging::PrettyLogMirrorStandardStream;
use streamlib::sdk::runtime::{
    ArmedEngineTeardownWatchdog, DescriptionOfTheAbandonedProcessorThreads,
    EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED, HowALoadedStreamEnded,
    KeptStreamReloadAtTheStart, LoadedStreamInThisRuntime, Runner, RunnerConstructionOptions,
    note_what_the_engine_teardown_is_waiting_on,
};
use streamlib_api_server::{LocalApiServedForAnEngine, serve_the_local_api_for_an_engine};
use streamlib_runtime_client_contract::tatolab_state_directory::TatolabStateDirectory;

use crate::refusal_on_standard_error::{EXIT_STATUS_OF_A_REFUSAL, write_refusal_to_standard_error};

/// How long the end of the run waits for a stream action the local API began —
/// a load the machine shutdown is giving up — to hand the engine back. Each
/// such action observes the shutdown within its own poll interval and kills
/// what it started.
const STREAM_ACTIONS_IN_FLIGHT_RETURN_BUDGET: Duration = Duration::from_secs(10);

/// Host the machine's streams until a machine shutdown is requested and every
/// stream has ended, tear the engine down, and return the status `tatolabd`
/// exits with.
///
/// Called on the process's first thread: on macOS the engine drives the window
/// event pump on it while it waits.
pub(crate) fn host_the_machines_streams_until_a_machine_shutdown(
    tatolab_state_directory: &TatolabStateDirectory,
    processor_interpreter_lend_directory: PathBuf,
) -> ExitCode {
    let engine = match Runner::new_with_construction_options(RunnerConstructionOptions {
        runtime_name: None,
        pretty_log_mirror_stream: PrettyLogMirrorStandardStream::StandardError,
        runtime_own_log_directory: Some(tatolab_state_directory.runtime_log_directory()),
    }) {
        Ok(engine) => engine,
        Err(construction_refusal) => {
            return write_refusal_to_standard_error(&format!(
                "the engine could not be built: {construction_refusal}"
            ));
        }
    };
    if let Err(lend_directory_refusal) =
        engine.set_processor_interpreter_lend_directory(processor_interpreter_lend_directory)
    {
        return write_refusal_to_standard_error(&format!(
            "the engine could not be handed its lend directory: {lend_directory_refusal}"
        ));
    }
    streamlib_media_builtins::register_media_builtin_processor_types();
    if let Err(kept_streams_refusal) = engine
        .keep_streams_in_the_state_directory(&tatolab_state_directory.kept_streams_directory())
    {
        return write_refusal_to_standard_error(&format!(
            "the runtime cannot keep streams: {kept_streams_refusal}"
        ));
    }

    let mut streams_loaded_when_the_machine_shutdown_was_requested = Vec::new();
    let mut local_api_served_for_the_engine = None;
    let run_outcome = engine
        .run_owning_the_machine_shutdown_signals(|| {
            local_api_served_for_the_engine = Some(serve_the_local_api_for_an_engine(&engine)?);
            log_the_kept_streams_reloaded_at_the_start(
                &engine.reload_every_kept_stream_not_stopped(),
            );
            engine.wait_until_a_machine_shutdown_is_requested();

            streams_loaded_when_the_machine_shutdown_was_requested = engine.every_loaded_stream();
            // First, so no call reaches a stream as it ends, and every stream a
            // connection attached unloads with its connection.
            drop(local_api_served_for_the_engine.take());
            // Inside the run: a load the local API began gives itself up only
            // on reading the machine's shutdown, which the run's end clears.
            if !engine.wait_until_this_reference_alone_holds_the_engine(
                STREAM_ACTIONS_IN_FLIGHT_RETURN_BUDGET,
            ) {
                tracing::warn!(
                    "a stream action the local API began still held the engine \
                     {STREAM_ACTIONS_IN_FLIGHT_RETURN_BUDGET:?} after it stopped serving"
                );
            }
            // Inside the run, so a second interrupt still forces every
            // stream's teardown and a third still ends the process.
            let every_stream_end = engine.wait_until_every_stream_has_ended();
            if streams_loaded_when_the_machine_shutdown_was_requested
                .iter()
                .any(|stream| stream_teardown_abandoned_by_its_watchdog(stream).is_some())
            {
                // The teardown names the abandonment, so it is written once.
                return Ok(());
            }
            every_stream_end
        })
        .map_err(|run_refusal| format!("the runtime stopped on a refusal: {run_refusal}"));
    if let Err(run_refusal) = &run_outcome {
        tracing::error!("{run_refusal}");
    }

    let engine_teardown_outcome = tear_the_engine_down(
        engine,
        streams_loaded_when_the_machine_shutdown_was_requested,
        local_api_served_for_the_engine,
    );
    let exit_status =
        exit_status_once_the_engine_is_torn_down(&run_outcome, &engine_teardown_outcome);
    for refusal in
        refusals_written_once_the_engine_is_torn_down(run_outcome, &engine_teardown_outcome)
    {
        write_refusal_to_standard_error(&refusal);
    }
    ExitCode::from(exit_status)
}

/// One line once the start's re-loads are over, counting them; the engine has
/// already logged each by name, and the reason for each it skipped.
fn log_the_kept_streams_reloaded_at_the_start(reloads: &[KeptStreamReloadAtTheStart]) {
    let reloaded_count = reloads
        .iter()
        .filter(|reload| matches!(reload, KeptStreamReloadAtTheStart::Reloaded { .. }))
        .count();
    tracing::info!(
        "the runtime is serving: {reloaded_count} kept streams re-loaded, {} skipped; it runs \
         until a signal stops it",
        reloads.len() - reloaded_count
    );
}

/// How `stream`'s teardown was abandoned by its watchdog, or `None` when it
/// was not.
fn stream_teardown_abandoned_by_its_watchdog(stream: &LoadedStreamInThisRuntime) -> Option<String> {
    match stream.how_this_stream_ended() {
        Some(HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog {
            what_its_teardown_was_waiting_on,
        }) => Some(format!(
            "the teardown of the stream `{}` outlived its watchdog while waiting on \
             {what_its_teardown_was_waiting_on}, and was abandoned",
            stream.stream_name()
        )),
        _ => None,
    }
}

/// The status `tatolabd` exits with once the teardown is over: the
/// watchdog's for a stream teardown it abandoned, a refusal's for anything
/// else written to standard error, and success otherwise.
fn exit_status_once_the_engine_is_torn_down(
    run_outcome: &Result<(), String>,
    engine_teardown_outcome: &EngineTeardownOutcome,
) -> u8 {
    match engine_teardown_outcome {
        EngineTeardownOutcome::LeftBeneathAStreamTeardownItsWatchdogAbandoned(_) => {
            EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED as u8
        }
        EngineTeardownOutcome::LeftBeneathAbandonedProcessorThreads(_)
        | EngineTeardownOutcome::StillReferenced(_) => EXIT_STATUS_OF_A_REFUSAL,
        EngineTeardownOutcome::Dropped if run_outcome.is_err() => EXIT_STATUS_OF_A_REFUSAL,
        EngineTeardownOutcome::Dropped => 0,
    }
}

/// What `tatolabd` writes to standard error once the teardown is over, in
/// order: the run's own refusal, then what the teardown left behind. Empty
/// for a clean exit.
///
/// The run's refusal is written again even though it was logged, because
/// under `STREAMLIB_QUIET` no log mirror carried it to standard error.
fn refusals_written_once_the_engine_is_torn_down(
    run_outcome: Result<(), String>,
    engine_teardown_outcome: &EngineTeardownOutcome,
) -> Vec<String> {
    let mut refusals = Vec::new();
    if let Err(run_refusal) = run_outcome {
        refusals.push(run_refusal);
    }
    match engine_teardown_outcome {
        EngineTeardownOutcome::Dropped => {}
        EngineTeardownOutcome::LeftBeneathAbandonedProcessorThreads(description)
        | EngineTeardownOutcome::LeftBeneathAStreamTeardownItsWatchdogAbandoned(description)
        | EngineTeardownOutcome::StillReferenced(description) => refusals.push(description.clone()),
    }
    refusals
}

/// How a teardown ended.
enum EngineTeardownOutcome {
    /// Every engine thread joined, and the engine dropped.
    Dropped,
    /// Processor threads ignored shutdown past their budget, so the engine is
    /// left alive beneath them until the process exits: a thread returning late
    /// would otherwise run the engine's drop on its own thread.
    LeftBeneathAbandonedProcessorThreads(String),
    /// A stream's teardown outlived its watchdog and its thread was
    /// abandoned, so the engine is left alive beneath it and the process ends
    /// with the watchdog's status.
    LeftBeneathAStreamTeardownItsWatchdogAbandoned(String),
    /// Something else still held the engine, so its threads were not joined.
    StillReferenced(String),
}

/// Stop serving the local API, shut the engine down and drop it under the
/// engine's teardown watchdog, whether or not the run reached a machine
/// shutdown.
fn tear_the_engine_down(
    engine: Arc<Runner>,
    streams_loaded_when_the_machine_shutdown_was_requested: Vec<Arc<LoadedStreamInThisRuntime>>,
    local_api_served_for_the_engine: Option<LocalApiServedForAnEngine>,
) -> EngineTeardownOutcome {
    let _watchdog = ArmedEngineTeardownWatchdog::arm("the engine teardown tatolabd began");
    // First, because its router holds the engine.
    note_what_the_engine_teardown_is_waiting_on("the local API to stop serving");
    drop(local_api_served_for_the_engine);
    if let Err(shut_down_failure) = engine.shut_down() {
        tracing::warn!(%shut_down_failure, "the engine shut down reporting a failure");
    }

    let teardowns_abandoned_by_their_watchdogs: Vec<String> =
        streams_loaded_when_the_machine_shutdown_was_requested
            .iter()
            .filter_map(|stream| stream_teardown_abandoned_by_its_watchdog(stream))
            .collect();
    let abandoned_processor_threads: Vec<_> =
        streams_loaded_when_the_machine_shutdown_was_requested
            .iter()
            .flat_map(|stream| stream.processor_threads_abandoned_and_still_running())
            .collect();
    if !teardowns_abandoned_by_their_watchdogs.is_empty() || !abandoned_processor_threads.is_empty()
    {
        // The report goes straight to the real standard error, because the
        // forgotten engine never drops its hold on the process logging
        // pathway, which is what gives the standard streams back.
        engine.stop_intercepting_the_standard_streams();
        std::mem::forget(streams_loaded_when_the_machine_shutdown_was_requested);
        std::mem::forget(engine);
        return if teardowns_abandoned_by_their_watchdogs.is_empty() {
            EngineTeardownOutcome::LeftBeneathAbandonedProcessorThreads(
                DescriptionOfTheAbandonedProcessorThreads(&abandoned_processor_threads).to_string(),
            )
        } else {
            EngineTeardownOutcome::LeftBeneathAStreamTeardownItsWatchdogAbandoned(
                teardowns_abandoned_by_their_watchdogs.join("; "),
            )
        };
    }
    drop(streams_loaded_when_the_machine_shutdown_was_requested);

    note_what_the_engine_teardown_is_waiting_on("the engine's own drop");
    match Arc::try_unwrap(engine) {
        Ok(owned_engine) => {
            drop(owned_engine);
            EngineTeardownOutcome::Dropped
        }
        Err(still_referenced_engine) => {
            // Its stdio interceptor is still installed, so the report would
            // otherwise land in the intercept pipe and die with the process.
            still_referenced_engine.stop_intercepting_the_standard_streams();
            std::mem::forget(still_referenced_engine);
            EngineTeardownOutcome::StillReferenced(
                "engine teardown left a live reference behind, so its threads were not joined"
                    .to_owned(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_run_and_a_dropped_engine_write_nothing() {
        assert!(
            refusals_written_once_the_engine_is_torn_down(Ok(()), &EngineTeardownOutcome::Dropped)
                .is_empty()
        );
    }

    #[test]
    fn a_refused_run_whose_engine_dropped_writes_the_run_refusal() {
        assert_eq!(
            refusals_written_once_the_engine_is_torn_down(
                Err("the runtime stopped on a refusal: refused".to_owned()),
                &EngineTeardownOutcome::Dropped,
            ),
            ["the runtime stopped on a refusal: refused"]
        );
    }

    #[test]
    fn an_engine_left_alive_after_a_refused_run_writes_the_run_refusal_then_the_teardown() {
        for (engine_teardown_outcome, teardown_description) in [
            (
                EngineTeardownOutcome::LeftBeneathAbandonedProcessorThreads(
                    "a processor thread was abandoned".to_owned(),
                ),
                "a processor thread was abandoned",
            ),
            (
                EngineTeardownOutcome::LeftBeneathAStreamTeardownItsWatchdogAbandoned(
                    "a stream teardown was abandoned".to_owned(),
                ),
                "a stream teardown was abandoned",
            ),
            (
                EngineTeardownOutcome::StillReferenced("a live reference was left".to_owned()),
                "a live reference was left",
            ),
        ] {
            assert_eq!(
                refusals_written_once_the_engine_is_torn_down(
                    Err("the runtime stopped on a refusal: refused".to_owned()),
                    &engine_teardown_outcome,
                ),
                [
                    "the runtime stopped on a refusal: refused",
                    teardown_description
                ]
            );
        }
    }

    #[test]
    fn a_stream_teardown_the_watchdog_abandoned_exits_with_the_watchdogs_status() {
        for run_outcome in [
            Ok(()),
            Err("the runtime stopped on a refusal: refused".to_owned()),
        ] {
            assert_eq!(
                exit_status_once_the_engine_is_torn_down(
                    &run_outcome,
                    &EngineTeardownOutcome::LeftBeneathAStreamTeardownItsWatchdogAbandoned(
                        "a stream teardown was abandoned".to_owned(),
                    ),
                ),
                124
            );
        }
    }

    #[test]
    fn every_other_outcome_exits_as_a_refusal_or_succeeds() {
        let refused_run: Result<(), String> =
            Err("the runtime stopped on a refusal: refused".to_owned());
        assert_eq!(
            exit_status_once_the_engine_is_torn_down(&Ok(()), &EngineTeardownOutcome::Dropped),
            0
        );
        assert_eq!(
            exit_status_once_the_engine_is_torn_down(&refused_run, &EngineTeardownOutcome::Dropped),
            EXIT_STATUS_OF_A_REFUSAL
        );
        for engine_teardown_outcome in [
            EngineTeardownOutcome::LeftBeneathAbandonedProcessorThreads(
                "a processor thread was abandoned".to_owned(),
            ),
            EngineTeardownOutcome::StillReferenced("a live reference was left".to_owned()),
        ] {
            assert_eq!(
                exit_status_once_the_engine_is_torn_down(&Ok(()), &engine_teardown_outcome),
                EXIT_STATUS_OF_A_REFUSAL
            );
        }
    }

    #[test]
    fn an_engine_left_alive_after_a_clean_run_writes_the_teardown() {
        assert_eq!(
            refusals_written_once_the_engine_is_torn_down(
                Ok(()),
                &EngineTeardownOutcome::StillReferenced("a live reference was left".to_owned()),
            ),
            ["a live reference was left"]
        );
    }
}
