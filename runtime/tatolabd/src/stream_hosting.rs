// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Hosting one stream: the engine built, the graph loaded with its
//! environment, the local API added, the run until a shutdown, and a teardown
//! that drops the engine — or leaves it beneath the processor threads it
//! abandoned.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use streamlib::engine_internal::core::app_directory::record_the_app_directory_the_runtime_host_was_given;
use streamlib::sdk::logging::PrettyLogMirrorStandardStream;
use streamlib::sdk::runtime::{
    ArmedEngineTeardownWatchdog, DescriptionOfTheAbandonedProcessorThreads, Runner,
    RunnerConstructionOptions, note_what_the_engine_teardown_is_waiting_on,
};
use streamlib_api_server::control_plane_host::{
    ApiServerControlPlaneHostConfig, register_api_server_control_plane_processor_on_runtime,
};

use crate::refusal_on_standard_error::write_refusal_to_standard_error;
use crate::stream_launch_inputs::StreamLaunchInputs;

/// Host the stream until a shutdown is requested, tear the engine down, and
/// return the status `tatolabd` exits with.
///
/// Called on the process's first thread: on macOS the engine drives the window
/// event pump on it while it waits for the shutdown.
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
    engine.set_processor_interpreter_lend_directory(processor_interpreter_lend_directory);
    streamlib_media_builtins::register_media_builtin_processor_types();

    let run_outcome = engine
        .load_graph_snapshot_start_and_wait_for_shutdown(
            &stream_graph,
            Some(stream_environment),
            |loaded_engine| {
                log_that_the_stream_loaded(loaded_engine)?;
                register_api_server_control_plane_processor_on_runtime(
                    loaded_engine,
                    ApiServerControlPlaneHostConfig::default(),
                )
            },
        )
        .map_err(|run_refusal| format!("the stream did not run: {run_refusal}"));
    if let Err(run_refusal) = &run_outcome {
        tracing::error!("{run_refusal}");
    }

    let engine_teardown_outcome = tear_the_engine_down(engine);
    let mut exit_code = ExitCode::SUCCESS;
    for refusal in
        refusals_written_once_the_engine_is_torn_down(run_outcome, engine_teardown_outcome)
    {
        exit_code = write_refusal_to_standard_error(&refusal);
    }
    exit_code
}

/// What `tatolabd` writes to standard error once the teardown is over, in
/// order: the run's own refusal, then what the teardown left behind. Empty
/// for a clean exit.
///
/// The run's refusal is written again even though it was logged, because
/// under `STREAMLIB_QUIET` no log mirror carried it to standard error.
fn refusals_written_once_the_engine_is_torn_down(
    run_outcome: Result<(), String>,
    engine_teardown_outcome: EngineTeardownOutcome,
) -> Vec<String> {
    let mut refusals = Vec::new();
    if let Err(run_refusal) = run_outcome {
        refusals.push(run_refusal);
    }
    match engine_teardown_outcome {
        EngineTeardownOutcome::Dropped => {}
        EngineTeardownOutcome::LeftBeneathAbandonedProcessorThreads(description)
        | EngineTeardownOutcome::StillReferenced(description) => refusals.push(description),
    }
    refusals
}

/// Log the load's success under the stream name and node count the engine's
/// live graph holds, so a reader of the log can tell a stream that loaded but
/// failed to start from one the load refused.
///
/// Called before the local API's processor is added, so the count is the
/// stream's own nodes.
fn log_that_the_stream_loaded(loaded_engine: &Runner) -> streamlib::sdk::error::Result<()> {
    let live_graph = loaded_engine.to_json()?;
    let loaded_node_count = live_graph
        .get("nodes")
        .and_then(|nodes| nodes.as_array())
        .map_or(0, Vec::len);
    match live_graph.get("stream").and_then(|stream| stream.as_str()) {
        Some(loaded_stream_name) => tracing::info!(
            "the stream `{loaded_stream_name}` loaded with {loaded_node_count} nodes"
        ),
        None => tracing::info!("the stream loaded with {loaded_node_count} nodes"),
    }
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
    /// Something else still held the engine, so its threads were not joined.
    StillReferenced(String),
}

/// Stop the engine and drop it under the engine's teardown watchdog, whether or
/// not it started.
///
/// `stop()` runs even after a run loop that stopped the engine itself, because
/// `start()` parks a reference to the engine inside the context only `stop()`
/// clears; after a failed start, that cycle would otherwise outlive the drop.
fn tear_the_engine_down(engine: Arc<Runner>) -> EngineTeardownOutcome {
    let _watchdog = ArmedEngineTeardownWatchdog::arm("the engine teardown tatolabd began");
    if let Err(stop_failure) = engine.stop() {
        tracing::warn!(%stop_failure, "engine stop reported a failure during teardown");
    }

    let abandoned_processor_threads = engine.processor_threads_abandoned_and_still_running();
    if !abandoned_processor_threads.is_empty() {
        // The report goes straight to the real standard error, because the
        // engine that owns the log worker is never dropped to flush it.
        engine.stop_intercepting_the_standard_streams();
        std::mem::forget(engine);
        return EngineTeardownOutcome::LeftBeneathAbandonedProcessorThreads(
            DescriptionOfTheAbandonedProcessorThreads(&abandoned_processor_threads).to_string(),
        );
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
            refusals_written_once_the_engine_is_torn_down(Ok(()), EngineTeardownOutcome::Dropped)
                .is_empty()
        );
    }

    #[test]
    fn a_refused_run_whose_engine_dropped_writes_the_run_refusal() {
        assert_eq!(
            refusals_written_once_the_engine_is_torn_down(
                Err("the stream did not run: refused".to_owned()),
                EngineTeardownOutcome::Dropped,
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
                EngineTeardownOutcome::StillReferenced("a live reference was left".to_owned()),
                "a live reference was left",
            ),
        ] {
            assert_eq!(
                refusals_written_once_the_engine_is_torn_down(
                    Err("the stream did not run: refused".to_owned()),
                    engine_teardown_outcome,
                ),
                ["the stream did not run: refused", teardown_description]
            );
        }
    }

    #[test]
    fn an_engine_left_alive_after_a_clean_run_writes_the_teardown() {
        assert_eq!(
            refusals_written_once_the_engine_is_torn_down(
                Ok(()),
                EngineTeardownOutcome::StillReferenced("a live reference was left".to_owned()),
            ),
            ["a live reference was left"]
        );
    }
}
