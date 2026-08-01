// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Runs one Tier A measurement cell and writes its artifact directory.
//!
//! Invoked per cell by `python/runner.py`, which owns A/B/A interleaving across
//! cells. One cell per process is deliberate: it keeps interpreter state, GC
//! history, and allocator state from leaking between cells, and it makes the
//! warm-restart battery measure the same startup path a user would hit.

// Same rationale as the library root: the engine's `Error` is 168 bytes, and
// boxing it would diverge these signatures from the API being measured.
#![allow(clippy::result_large_err)]

use std::path::PathBuf;

use clap::Parser;
use pyo3::prelude::*;
use streamlib::sdk::error::{Error, Result};
use streamlib_pyembed_spike::monotonic_clock::MEASUREMENT_STAMPING_IS_COMPILED_IN;
use streamlib_pyembed_spike::machine_specification_probe::{
    machine_is_in_locked_measurement_state, probe_machine_specification,
};
use streamlib_pyembed_spike::python_processor_callback_registry::register_python_callback_under_token;
use streamlib_pyembed_spike::synthetic_frame_wire_payload_mode::SyntheticFrameWirePayloadMode;
use streamlib_pyembed_spike::tier_a_measurement_cell::{
    MeasurementArm, TierAMeasurementCellSpecification, run_tier_a_measurement_cell,
};

#[derive(Parser, Debug)]
#[command(about = "Run one Tier A measurement cell for the #1702 PyO3 spike")]
struct TierAHarnessArguments {
    /// Which comparison arm to run.
    #[arg(long, value_parser = parse_measurement_arm)]
    arm: MeasurementArm,

    #[arg(long, default_value_t = 30)]
    fps: u32,

    #[arg(long, default_value_t = 1920)]
    frame_width: u32,

    #[arg(long, default_value_t = 1080)]
    frame_height: u32,

    #[arg(long, default_value_t = 4)]
    channels: u32,

    /// `surface-reference` matches what a real `VideoFrame` weighs on the wire
    /// and is what the protocol cells measure; `full-pixel-payload` reproduces
    /// the payload sweep that retracted the 27ms floor.
    #[arg(
        long,
        default_value = "surface-reference",
        value_parser = SyntheticFrameWirePayloadMode::parse_from_flag_value
    )]
    wire_payload_mode: SyntheticFrameWirePayloadMode,

    #[arg(long, default_value_t = 600)]
    duration_seconds: u64,

    #[arg(long, default_value_t = 60)]
    warmup_exclusion_seconds: u64,

    #[arg(long, default_value_t = 0)]
    repetition_index: u32,

    /// Python module exposing the stage callback, importable from PYTHONPATH.
    #[arg(long, default_value = "spike_stage_callbacks")]
    stage_callback_module: String,

    /// Callable within `--stage-callback-module` to invoke per frame.
    #[arg(long, default_value = "passthrough_stage")]
    stage_callback_attribute: String,

    /// Control condition for what the GIL attachment anchor is worth.
    #[arg(long)]
    disable_gil_anchor: bool,

    /// `default` leaves CPython's collector alone; `tuned` applies
    /// `gc.freeze()` plus raised thresholds inside the embedded interpreter.
    #[arg(long, default_value = "default")]
    garbage_collection_mode: String,

    /// Refuse to run unless every latency-relevant knob is locked. Off by
    /// default so exploratory runs work; the protocol's gated cells set it.
    #[arg(long)]
    require_locked_measurement_state: bool,

    #[arg(long)]
    output_directory: PathBuf,
}

fn parse_measurement_arm(value: &str) -> std::result::Result<MeasurementArm, String> {
    match value {
        "in-process-python" => Ok(MeasurementArm::InProcessPython),
        "rust-passthrough-floor" => Ok(MeasurementArm::RustPassthroughFloor),
        other => Err(format!(
            "unknown arm `{other}` — expected `in-process-python` or `rust-passthrough-floor`"
        )),
    }
}

fn main() -> Result<()> {
    // No subscriber is installed here on purpose: `Runner::new()` installs the
    // engine's global tracing dispatcher, and a second `set_global_default`
    // aborts the run before a single frame is measured.
    let arguments = TierAHarnessArguments::parse();

    let machine_specification = probe_machine_specification();
    if arguments.require_locked_measurement_state
        && !machine_is_in_locked_measurement_state(&machine_specification)
    {
        return Err(Error::Configuration(
            "machine is not in a locked measurement state and \
             --require-locked-measurement-state was set; see machine-spec.json for which knob \
             is unlocked and the owner checklist for the commands that lock it"
                .to_string(),
        ));
    }

    // CPython comes up before `App::new` so interpreter initialization is
    // ordered ahead of GpuContext init and iceoryx2 node creation rather than
    // racing them from a processor thread.
    Python::initialize();

    let python_callback_registration_token = format!(
        "{}::{}::rep-{}",
        arguments.stage_callback_module, arguments.stage_callback_attribute, arguments.repetition_index
    );

    if arguments.arm == MeasurementArm::InProcessPython {
        Python::attach(|python| -> Result<()> {
            let module = python
                .import(arguments.stage_callback_module.as_str())
                .map_err(|error| {
                    Error::Configuration(format!(
                        "cannot import stage callback module `{}` — is PYTHONPATH set to the \
                         spike's python/ directory? {error}",
                        arguments.stage_callback_module
                    ))
                })?;
            let callable = module
                .getattr(arguments.stage_callback_attribute.as_str())
                .map_err(|error| {
                    Error::Configuration(format!(
                        "module `{}` exposes no `{}`: {error}",
                        arguments.stage_callback_module, arguments.stage_callback_attribute
                    ))
                })?;
            register_python_callback_under_token(
                python_callback_registration_token.clone(),
                callable.unbind(),
            );
            Ok(())
        })?;
    }

    let specification = TierAMeasurementCellSpecification {
        arm: arguments.arm,
        frame_width_pixels: arguments.frame_width,
        frame_height_pixels: arguments.frame_height,
        channel_count: arguments.channels,
        target_frames_per_second: arguments.fps,
        wire_payload_mode: arguments.wire_payload_mode,
        cell_duration_seconds: arguments.duration_seconds,
        warmup_exclusion_seconds: arguments.warmup_exclusion_seconds,
        python_callback_registration_token,
        anchor_processor_thread_gil: !arguments.disable_gil_anchor,
        stage_callback_attribute: arguments.stage_callback_attribute.clone(),
        resolved_delivery_profile: "every_sample".to_string(),
        measured_metric_name: "source_emit_to_sink_receive".to_string(),
        repetition_index: arguments.repetition_index,
        measurement_stamping_is_compiled_in: MEASUREMENT_STAMPING_IS_COMPILED_IN,
    };

    let cell_directory = arguments
        .output_directory
        .join(specification.artifact_directory_name());

    // The recorder has to live in the interpreter that runs the callback. In the
    // runner's interpreter it would time collections that cannot have caused any
    // frame's latency — which is what it did before, reporting 0 events.
    let garbage_collection_recorder = if arguments.arm == MeasurementArm::InProcessPython {
        Some(install_embedded_interpreter_garbage_collection_recorder(
            &arguments.garbage_collection_mode,
        )?)
    } else {
        None
    };

    let outcome = run_tier_a_measurement_cell(&specification, &cell_directory)?;

    if let Some(recorder) = garbage_collection_recorder {
        export_embedded_interpreter_garbage_collection_records(&recorder, &cell_directory)?;
    }

    // `Runner::stop()` has torn the engine's global dispatcher down by now, so
    // every record below would be dropped on the floor without a subscriber of
    // our own. An unattended matrix run would then show no sign that a cell was
    // invalid — the counters would reach summary.json and nothing else.
    let post_stop_subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .finish();
    tracing::subscriber::with_default(post_stop_subscriber, || {
    tracing::info!(
        cell = %specification.artifact_directory_name(),
        received_frames = outcome.received_frame_count,
        measured_frames = outcome.measured_frame_count,
        dropped_frames = outcome.dropped_frame_count,
        p50_ns = outcome.source_emit_to_sink_receive.p50_nanoseconds,
        p99_ns = outcome.source_emit_to_sink_receive.p99_nanoseconds,
        p99_9_ns = outcome.source_emit_to_sink_receive.p99_9_nanoseconds,
        max_ns = outcome.source_emit_to_sink_receive.max_nanoseconds,
        "cell complete"
    );

    if outcome.negative_latency_anomaly_count > 0 {
        tracing::error!(
            count = outcome.negative_latency_anomaly_count,
            "cell recorded sink stamps earlier than their emit stamps — the clocks disagree \
             and this cell's percentiles must not be used"
        );
    }
    if outcome.histogram_range_saturation_count > 0 {
        tracing::error!(
            count = outcome.histogram_range_saturation_count,
            "samples exceeded the histogram range — reported percentiles are clipped"
        );
    }
    });

    Ok(())
}

/// Install the GC phase recorder inside the embedded interpreter and apply the
/// requested collector mode, returning the recorder object.
fn install_embedded_interpreter_garbage_collection_recorder(
    garbage_collection_mode: &str,
) -> Result<Py<PyAny>> {
    Python::attach(|python| {
        let module = python
            .import("gc_collection_attribution")
            .map_err(|error| {
                Error::Configuration(format!(
                    "cannot import gc_collection_attribution — is PYTHONPATH set to the spike's \
                     python/ directory? {error}"
                ))
            })?;
        let recorder = module
            .getattr("GarbageCollectionPauseAttributionRecorder")
            .and_then(|class| class.call0())
            .map_err(|error| {
                Error::Runtime(format!("constructing the GC recorder failed: {error}"))
            })?;
        recorder
            .call_method1("apply_garbage_collection_mode", (garbage_collection_mode,))
            .map_err(|error| {
                Error::Configuration(format!(
                    "unsupported --garbage-collection-mode `{garbage_collection_mode}`: {error}"
                ))
            })?;
        recorder
            .call_method0("install_collection_phase_callback")
            .map_err(|error| {
                Error::Runtime(format!("installing the GC callback failed: {error}"))
            })?;
        Ok(recorder.unbind())
    })
}

/// Uninstall the recorder and write its records beside the cell's other artifacts.
fn export_embedded_interpreter_garbage_collection_records(
    recorder: &Py<PyAny>,
    cell_directory: &std::path::Path,
) -> Result<()> {
    let record_path = cell_directory.join("gc-collections-embedded-interpreter.jsonl");
    Python::attach(|python| {
        let recorder = recorder.bind(python);
        recorder
            .call_method0("uninstall_collection_phase_callback")
            .map_err(|error| {
                Error::Runtime(format!("uninstalling the GC callback failed: {error}"))
            })?;
        let recorded_event_count = recorder
            .call_method0("recorded_collection_phase_event_count")
            .and_then(|count| count.extract::<u64>())
            .map_err(|error| {
                Error::Runtime(format!("reading the GC event count failed: {error}"))
            })?;
        recorder
            .call_method1(
                "export_collection_records_as_jsonl",
                (record_path.to_string_lossy().as_ref(),),
            )
            .map_err(|error| {
                Error::Runtime(format!("exporting the GC records failed: {error}"))
            })?;
        tracing::info!(
            recorded_event_count,
            path = %record_path.display(),
            "exported embedded-interpreter GC records"
        );
        Ok(())
    })
}
