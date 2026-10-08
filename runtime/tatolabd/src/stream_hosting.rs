// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Hosting one stream: the engine built, the graph loaded with its
//! environment, the local API added, the run until a shutdown, and a teardown
//! that drops the engine — or leaves it beneath the processor threads it
//! abandoned.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use streamlib::engine_internal::core::app_directory::record_the_app_entry_directory_the_language_host_captured;
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
    record_the_app_entry_directory_the_language_host_captured(
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

    match (tear_the_engine_down(engine), run_outcome) {
        (EngineTeardownOutcome::Dropped, Ok(())) => ExitCode::SUCCESS,
        // Written again once the engine is gone, because under
        // `STREAMLIB_QUIET` no log mirror carried it to standard error.
        (EngineTeardownOutcome::Dropped, Err(run_refusal)) => {
            write_refusal_to_standard_error(&run_refusal)
        }
        (
            EngineTeardownOutcome::LeftBeneathAbandonedProcessorThreads(description)
            | EngineTeardownOutcome::StillReferenced(description),
            _,
        ) => write_refusal_to_standard_error(&description),
    }
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
