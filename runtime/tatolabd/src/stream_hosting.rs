// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Hosting one stream: the engine built, the graph loaded as its one stream
//! with its environment, the engine's local API served, the run until the
//! stream ends, and a teardown that drops the engine — or leaves it beneath
//! the threads its stream abandoned.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use streamlib::engine_internal::core::app_directory::record_the_app_directory_the_runtime_host_was_given;
use streamlib::sdk::logging::PrettyLogMirrorStandardStream;
use streamlib::sdk::runtime::{
    ArmedEngineTeardownWatchdog, DescriptionOfTheAbandonedProcessorThreads,
    EXIT_STATUS_OF_A_TEARDOWN_THE_WATCHDOG_ENDED, HowALoadedStreamEnded, LoadedStreamInThisRuntime,
    OptionsForLoadingOneStream, Runner, RunnerConstructionOptions,
    StreamLoadObservingMachineShutdownRequests, note_what_the_engine_teardown_is_waiting_on,
};
use streamlib_api_server::{LocalApiServedForAnEngine, serve_the_local_api_for_an_engine};

use crate::refusal_on_standard_error::{EXIT_STATUS_OF_A_REFUSAL, write_refusal_to_standard_error};
use crate::stream_launch_inputs::StreamLaunchInputs;

/// Host the stream until it ends, tear the engine down, and return the status
/// `tatolabd` exits with.
///
/// Called on the process's first thread: on macOS the engine drives the window
/// event pump on it while it waits for the stream to end.
pub(crate) fn host_the_stream_until_shutdown(
    StreamLaunchInputs {
        stream_graph,
        stream_environment,
    }: StreamLaunchInputs,
    processor_interpreter_lend_directory: PathBuf,
) -> ExitCode {
    // So the runtime is named after the project rather than the directory
    // `tatolabd` was started from; `STREAMLIB_APP_DIRECTORY` still outranks it.
    record_the_app_directory_the_runtime_host_was_given(
        stream_environment.project_directory.clone(),
    );

    let engine = match Runner::new_with_construction_options(RunnerConstructionOptions {
        runtime_name: None,
        pretty_log_mirror_stream: PrettyLogMirrorStandardStream::StandardError,
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

    let mut hosted_stream = None;
    let mut local_api_served_for_the_engine = None;
    let run_outcome = engine
        .run_owning_the_machine_shutdown_signals(|| {
            let stream = match engine
                .load_stream_from_graph_snapshot_unless_a_machine_shutdown_is_requested(
                    &stream_graph,
                    OptionsForLoadingOneStream::in_stream_environment(stream_environment),
                )? {
                StreamLoadObservingMachineShutdownRequests::Loaded(stream) => stream,
                StreamLoadObservingMachineShutdownRequests::AbandonedForAMachineShutdownRequest => {
                    return Ok(());
                }
            };
            hosted_stream = Some(Arc::clone(&stream));
            log_that_the_stream_loaded(&stream)?;
            local_api_served_for_the_engine = Some(serve_the_local_api_for_an_engine(&engine)?);
            stream.start()?;
            let stream_end = engine.wait_until_the_stream_ends(&stream);
            if let Some(HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog { .. }) =
                stream.how_this_stream_ended()
            {
                // The teardown names the abandonment, so it is written once.
                return Ok(());
            }
            stream_end
        })
        .map_err(|run_refusal| format!("the stream did not run: {run_refusal}"));
    if let Err(run_refusal) = &run_outcome {
        tracing::error!("{run_refusal}");
    }

    let engine_teardown_outcome =
        tear_the_engine_down(engine, hosted_stream, local_api_served_for_the_engine);
    let exit_status =
        exit_status_once_the_engine_is_torn_down(&run_outcome, &engine_teardown_outcome);
    for refusal in
        refusals_written_once_the_engine_is_torn_down(run_outcome, &engine_teardown_outcome)
    {
        write_refusal_to_standard_error(&refusal);
    }
    ExitCode::from(exit_status)
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

/// Log the load's success under the stream name and node count the stream's
/// live graph holds, so a reader of the log can tell a stream that loaded but
/// failed to start from one the load refused.
fn log_that_the_stream_loaded(
    stream: &LoadedStreamInThisRuntime,
) -> streamlib::sdk::error::Result<()> {
    let loaded_node_count = stream
        .to_json()?
        .get("nodes")
        .and_then(|nodes| nodes.as_array())
        .map_or(0, Vec::len);
    tracing::info!(
        "the stream `{}` loaded with {loaded_node_count} nodes",
        stream.stream_name()
    );
    Ok(())
}

/// How a teardown ended.
enum EngineTeardownOutcome {
    /// Every engine thread joined, and the engine dropped.
    Dropped,
    /// Processor threads ignored shutdown past their budget, so the engine is
    /// left alive beneath them until the process exits: a thread returning late
    /// would otherwise run the engine's drop on its own thread.
    LeftBeneathAbandonedProcessorThreads(String),
    /// The stream's teardown outlived its watchdog and its thread was
    /// abandoned, so the engine is left alive beneath it and the process ends
    /// with the watchdog's status.
    LeftBeneathAStreamTeardownItsWatchdogAbandoned(String),
    /// Something else still held the engine, so its threads were not joined.
    StillReferenced(String),
}

/// Stop serving the local API, shut the engine down and drop it under the
/// engine's teardown watchdog, whether or not the stream started.
fn tear_the_engine_down(
    engine: Arc<Runner>,
    hosted_stream: Option<Arc<LoadedStreamInThisRuntime>>,
    local_api_served_for_the_engine: Option<LocalApiServedForAnEngine>,
) -> EngineTeardownOutcome {
    let _watchdog = ArmedEngineTeardownWatchdog::arm("the engine teardown tatolabd began");
    // First, because its router holds the engine.
    note_what_the_engine_teardown_is_waiting_on("the local API to stop serving");
    drop(local_api_served_for_the_engine);
    if let Err(shut_down_failure) = engine.shut_down() {
        tracing::warn!(%shut_down_failure, "the engine shut down reporting a failure");
    }

    if let Some(hosted_stream) = hosted_stream {
        let teardown_abandoned_by_its_watchdog = match hosted_stream.how_this_stream_ended() {
            Some(HowALoadedStreamEnded::AbandonedByItsTeardownWatchdog {
                what_its_teardown_was_waiting_on,
            }) => Some(format!(
                "the teardown of the stream `{}` outlived its watchdog while waiting on \
                 {what_its_teardown_was_waiting_on}, and was abandoned",
                hosted_stream.stream_name()
            )),
            _ => None,
        };
        let abandoned_processor_threads =
            hosted_stream.processor_threads_abandoned_and_still_running();
        if teardown_abandoned_by_its_watchdog.is_some() || !abandoned_processor_threads.is_empty() {
            // The report goes straight to the real standard error, because the
            // forgotten engine never drops its hold on the process logging
            // pathway, which is what gives the standard streams back.
            engine.stop_intercepting_the_standard_streams();
            std::mem::forget(hosted_stream);
            std::mem::forget(engine);
            return match teardown_abandoned_by_its_watchdog {
                Some(description) => {
                    EngineTeardownOutcome::LeftBeneathAStreamTeardownItsWatchdogAbandoned(
                        description,
                    )
                }
                None => EngineTeardownOutcome::LeftBeneathAbandonedProcessorThreads(
                    DescriptionOfTheAbandonedProcessorThreads(&abandoned_processor_threads)
                        .to_string(),
                ),
            };
        }
    }

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
                Err("the stream did not run: refused".to_owned()),
                &EngineTeardownOutcome::Dropped,
            ),
            ["the stream did not run: refused"]
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
                    Err("the stream did not run: refused".to_owned()),
                    &engine_teardown_outcome,
                ),
                ["the stream did not run: refused", teardown_description]
            );
        }
    }

    #[test]
    fn a_stream_teardown_the_watchdog_abandoned_exits_with_the_watchdogs_status() {
        for run_outcome in [Ok(()), Err("the stream did not run: refused".to_owned())] {
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
        let refused_run: Result<(), String> = Err("the stream did not run: refused".to_owned());
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
