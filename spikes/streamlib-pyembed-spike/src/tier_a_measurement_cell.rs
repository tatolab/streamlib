// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Builds and runs one Tier A measurement cell: synthetic source → stage → sink,
//! headless, in a single process.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use streamlib::sdk::App;
use streamlib::sdk::error::{Error, Result};

use crate::latency_measurement_recorder::{
    LatencyMeasurementRecorder, LatencyPercentileSummary,
};
use crate::machine_specification_probe::{
    MACHINE_SPECIFICATION_JSON_FILE_NAME, MachineSpecification, probe_machine_specification,
};
use crate::measuring_sink_processor::{
    MeasuringSinkConfiguration, MeasuringSinkProcessor, install_measurement_collection_point,
    take_measurement_collection_point,
};
use crate::monotonic_clock::{read_monotonic_clock_nanoseconds, spin_until_monotonic_deadline};
use crate::python_callback_stage_processor::{
    PythonCallbackStageConfiguration, PythonCallbackStageProcessor,
};
use crate::rust_passthrough_floor_stage_processor::{
    RustPassthroughFloorStageConfiguration, RustPassthroughFloorStageProcessor,
};
use crate::synthetic_frame_source_processor::{
    SyntheticFrameSourceConfiguration, SyntheticFrameSourceProcessor,
};

/// Which of the two Rust-side comparison arms a cell runs. The third arm, the
/// subprocess baseline, is driven from `python/runner.py`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeasurementArm {
    /// CPython embedded in this process, callback on the processor thread.
    InProcessPython,
    /// Pure-Rust passthrough — the engine wire-hop floor, no interpreter.
    RustPassthroughFloor,
}

impl MeasurementArm {
    /// Stable token used in cell directory names and in `cell-spec.json`.
    pub fn as_artifact_token(self) -> &'static str {
        match self {
            MeasurementArm::InProcessPython => "in-process-python",
            MeasurementArm::RustPassthroughFloor => "rust-passthrough-floor",
        }
    }
}

/// Everything that defines one cell. Recorded verbatim into `cell-spec.json` so
/// the summarizer can refuse to evaluate a gate the cell cannot support.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierAMeasurementCellSpecification {
    pub arm: MeasurementArm,
    pub frame_width_pixels: u32,
    pub frame_height_pixels: u32,
    pub channel_count: u32,
    pub target_frames_per_second: u32,
    pub cell_duration_seconds: u64,
    pub warmup_exclusion_seconds: u64,
    pub python_callback_registration_token: String,
    pub anchor_processor_thread_gil: bool,
    /// The callable the stage invoked, e.g. `passthrough_stage`. Part of the
    /// cell directory name: two cells differing only by stage would otherwise
    /// collide and the second would overwrite the first.
    pub stage_callback_attribute: String,
    /// Declared on every input port. Owner decision for #1702: latency
    /// percentiles are the primary signal, so drops are reported not gated.
    pub resolved_delivery_profile: String,
    /// The quantity the percentiles describe. Deliberately not
    /// "capture-to-present" — Tier A has no capture and no present.
    pub measured_metric_name: String,
    pub repetition_index: u32,
    /// False in a `stamping-compiled-out` control build, whose only valid output
    /// is a throughput comparison. Recorded so a control cell's artifact can
    /// never be read as a measurement cell's.
    pub measurement_stamping_is_compiled_in: bool,
}

impl TierAMeasurementCellSpecification {
    /// Directory name for this cell: sortable, and unambiguous across arms,
    /// rates, and repetitions.
    pub fn artifact_directory_name(&self) -> String {
        format!(
            "arm-{}__fps-{:03}__stage-{}__gil-anchor-{}__rep-{:02}",
            self.arm.as_artifact_token(),
            self.target_frames_per_second,
            self.stage_callback_attribute,
            if self.anchor_processor_thread_gil {
                "on"
            } else {
                "off"
            },
            self.repetition_index,
        )
    }
}

/// What one cell produced, alongside the spec that produced it.
#[derive(Debug, Clone, Serialize)]
pub struct TierAMeasurementCellOutcome {
    pub specification: TierAMeasurementCellSpecification,
    pub machine_specification: MachineSpecification,
    pub source_emit_to_sink_receive: LatencyPercentileSummary,
    pub stage_callback: LatencyPercentileSummary,
    pub received_frame_count: u64,
    /// Frames that survived the warmup exclusion and therefore back the
    /// percentiles above.
    pub measured_frame_count: u64,
    pub dropped_frame_count: u64,
    /// A sink stamp earlier than its emit stamp. Nonzero means the two arms'
    /// clocks disagree, which invalidates the cell rather than degrading it.
    pub negative_latency_anomaly_count: u64,
    /// Samples that exceeded the histogram's configured range. Nonzero means the
    /// reported percentiles are clipped and the cell must be rerun with a wider
    /// range, not interpreted.
    pub histogram_range_saturation_count: u64,
    pub rolling_one_second_frame_rate_windows: Vec<f64>,
}

/// Build the graph, run it for the cell's duration, and return its outcome.
///
/// The caller must have registered the Python callable under
/// `python_callback_registration_token` before calling this when the arm is
/// [`MeasurementArm::InProcessPython`], and must have called
/// `Python::initialize()` before any engine thread starts.
pub fn run_tier_a_measurement_cell(
    specification: &TierAMeasurementCellSpecification,
    artifact_directory: &Path,
) -> Result<TierAMeasurementCellOutcome> {
    std::fs::create_dir_all(artifact_directory)?;

    let warmup_exclusion_nanoseconds = (specification.warmup_exclusion_seconds as i64)
        .checked_mul(1_000_000_000)
        .ok_or_else(|| {
            Error::Configuration("warmup exclusion overflows a nanosecond count".to_string())
        })?;
    let expected_frame_count = (specification.cell_duration_seconds as usize)
        .saturating_mul(specification.target_frames_per_second as usize)
        .saturating_mul(12)
        / 10;
    install_measurement_collection_point(LatencyMeasurementRecorder::new(
        warmup_exclusion_nanoseconds,
        expected_frame_count,
    ));

    let app = App::new()?;
    let source = app.add_local::<SyntheticFrameSourceProcessor::Processor>(
        SyntheticFrameSourceConfiguration {
            frame_width_pixels: specification.frame_width_pixels,
            frame_height_pixels: specification.frame_height_pixels,
            channel_count: specification.channel_count,
            target_frames_per_second: specification.target_frames_per_second,
        },
    )?;

    let stage = match specification.arm {
        MeasurementArm::InProcessPython => app
            .add_local::<PythonCallbackStageProcessor::Processor>(
                PythonCallbackStageConfiguration {
                    frame_width_pixels: specification.frame_width_pixels,
                    frame_height_pixels: specification.frame_height_pixels,
                    channel_count: specification.channel_count,
                    python_callback_registration_token: specification
                        .python_callback_registration_token
                        .clone(),
                    anchor_processor_thread_gil: specification.anchor_processor_thread_gil,
                },
            )?,
        MeasurementArm::RustPassthroughFloor => app
            .add_local::<RustPassthroughFloorStageProcessor::Processor>(
                RustPassthroughFloorStageConfiguration {
                    frame_width_pixels: specification.frame_width_pixels,
                    frame_height_pixels: specification.frame_height_pixels,
                    channel_count: specification.channel_count,
                },
            )?,
    };

    let sink = app.add_local::<MeasuringSinkProcessor::Processor>(MeasuringSinkConfiguration {})?;

    app.connect((&source, "frame_out"), (&stage, "frame_in"))?;
    app.connect((&stage, "frame_out"), (&sink, "frame_in"))?;

    let machine_specification = probe_machine_specification();

    app.runner().start()?;
    let cell_deadline = read_monotonic_clock_nanoseconds()
        + (specification.cell_duration_seconds as i64) * 1_000_000_000;
    spin_until_monotonic_deadline(cell_deadline);
    app.runner().stop()?;

    let recorder = take_measurement_collection_point().ok_or_else(|| {
        Error::Runtime(
            "the measurement collection point vanished during the cell — no numbers to report"
                .to_string(),
        )
    })?;

    // Read the frame count before any percentile: a fully-refused run must be
    // reported as zero frames, never as a fast one.
    let received_frame_count = recorder.received_frame_count();
    if received_frame_count == 0 {
        return Err(Error::Runtime(format!(
            "cell `{}` received zero frames — the graph ran but nothing arrived at the sink",
            specification.artifact_directory_name()
        )));
    }

    recorder
        .write_per_frame_measurement_jsonl(&artifact_directory.join("per-frame-measurements.jsonl"))?;
    recorder.write_mergeable_histogram_export(
        &artifact_directory.join("source-emit-to-sink-receive.histogram"),
    )?;

    let outcome = TierAMeasurementCellOutcome {
        specification: specification.clone(),
        machine_specification,
        source_emit_to_sink_receive: recorder.source_emit_to_sink_receive_summary(),
        stage_callback: recorder.stage_callback_summary(),
        received_frame_count,
        measured_frame_count: recorder.measured_frame_count(),
        dropped_frame_count: recorder.dropped_frame_count(),
        negative_latency_anomaly_count: recorder.negative_latency_anomaly_count(),
        histogram_range_saturation_count: recorder.histogram_range_saturation_count(),
        rolling_one_second_frame_rate_windows: recorder.rolling_one_second_frame_rate_windows(),
    };

    write_json_artifact(
        &artifact_directory.join("cell-spec.json"),
        &outcome.specification,
    )?;
    write_json_artifact(
        &artifact_directory.join(MACHINE_SPECIFICATION_JSON_FILE_NAME),
        &outcome.machine_specification,
    )?;
    write_json_artifact(&artifact_directory.join("summary.json"), &outcome)?;

    Ok(outcome)
}

fn write_json_artifact<T: Serialize>(path: &PathBuf, value: &T) -> Result<()> {
    let encoded = serde_json::to_vec_pretty(value)
        .map_err(|error| Error::Runtime(format!("failed to encode {}: {error}", path.display())))?;
    std::fs::write(path, encoded)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_specification() -> TierAMeasurementCellSpecification {
        TierAMeasurementCellSpecification {
            arm: MeasurementArm::InProcessPython,
            frame_width_pixels: 1920,
            frame_height_pixels: 1080,
            channel_count: 4,
            target_frames_per_second: 60,
            cell_duration_seconds: 600,
            warmup_exclusion_seconds: 60,
            python_callback_registration_token: "cell-token".to_string(),
            anchor_processor_thread_gil: true,
            stage_callback_attribute: "passthrough_stage".to_string(),
            resolved_delivery_profile: "every_sample".to_string(),
            measured_metric_name: "source_emit_to_sink_receive".to_string(),
            repetition_index: 3,
            measurement_stamping_is_compiled_in: true,
        }
    }

    /// Cell directories must be unambiguous and sort sensibly, because A/B/A
    /// interleaving writes many sibling cells that differ only in arm and
    /// repetition — a collision would silently overwrite a cell's raw data.
    #[test]
    fn cell_directory_names_encode_every_distinguishing_dimension() {
        let name = example_specification().artifact_directory_name();
        assert_eq!(
            name,
            "arm-in-process-python__fps-060__stage-passthrough_stage__gil-anchor-on__rep-03"
        );

        let floor_arm = TierAMeasurementCellSpecification {
            arm: MeasurementArm::RustPassthroughFloor,
            ..example_specification()
        };
        assert_ne!(floor_arm.artifact_directory_name(), name);
    }

    /// Zero-padding keeps 30fps sorting before 60fps and rep-02 before rep-10 in
    /// a plain lexicographic directory listing.
    #[test]
    fn cell_directory_names_sort_lexicographically_by_rate_and_repetition() {
        let thirty = TierAMeasurementCellSpecification {
            target_frames_per_second: 30,
            ..example_specification()
        };
        let sixty = example_specification();
        assert!(thirty.artifact_directory_name() < sixty.artifact_directory_name());

        let second = TierAMeasurementCellSpecification {
            repetition_index: 2,
            ..example_specification()
        };
        let tenth = TierAMeasurementCellSpecification {
            repetition_index: 10,
            ..example_specification()
        };
        assert!(second.artifact_directory_name() < tenth.artifact_directory_name());
    }

    /// The metric name is load-bearing: "capture-to-present" is not observable
    /// in Tier A, and a spec that claimed it would mislabel the public
    /// benchmark artifact.
    #[test]
    fn the_recorded_metric_name_is_not_capture_to_present() {
        let specification = example_specification();
        assert_eq!(
            specification.measured_metric_name,
            "source_emit_to_sink_receive"
        );
        assert!(!specification.measured_metric_name.contains("present"));
    }

    /// The owner's delivery-profile decision must survive into the artifact —
    /// the summarizer keys gate eligibility off this field.
    #[test]
    fn the_recorded_delivery_profile_is_every_sample() {
        assert_eq!(example_specification().resolved_delivery_profile, "every_sample");
    }

    /// The spec round-trips so a cell can be replayed exactly from its own
    /// artifact directory.
    #[test]
    fn cell_specification_round_trips_through_serde_json() {
        let specification = example_specification();
        let encoded = serde_json::to_value(&specification).expect("serializes");
        let decoded: TierAMeasurementCellSpecification =
            serde_json::from_value(encoded).expect("deserializes");
        assert_eq!(decoded, specification);
    }
}

#[cfg(test)]
mod cell_directory_collision_tests {
    use super::*;

    /// Two cells differing only by stage must not share a directory. They did
    /// before the stage token was added, and the second silently overwrote the
    /// first — losing half a matrix with no error.
    #[test]
    fn cells_differing_only_by_stage_get_distinct_directories() {
        let passthrough = TierAMeasurementCellSpecification {
            arm: MeasurementArm::InProcessPython,
            frame_width_pixels: 1280,
            frame_height_pixels: 720,
            channel_count: 4,
            target_frames_per_second: 30,
            cell_duration_seconds: 600,
            warmup_exclusion_seconds: 60,
            python_callback_registration_token: "token".to_string(),
            anchor_processor_thread_gil: true,
            stage_callback_attribute: "passthrough_stage".to_string(),
            resolved_delivery_profile: "every_sample".to_string(),
            measured_metric_name: "source_emit_to_sink_receive".to_string(),
            repetition_index: 0,
            measurement_stamping_is_compiled_in: true,
        };
        let realistic = TierAMeasurementCellSpecification {
            stage_callback_attribute: "realistic_stage".to_string(),
            ..passthrough.clone()
        };
        assert_ne!(
            passthrough.artifact_directory_name(),
            realistic.artifact_directory_name()
        );
    }
}
