// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Terminal processor of every spike graph: stamps arrival, reconstructs the
//! per-frame measurement, and parks it where the harness can collect it after
//! the run.

use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ReactiveProcessor;

use crate::latency_measurement_recorder::{
    LatencyMeasurementRecorder, PerFrameLatencyMeasurement,
};
use crate::monotonic_clock::read_measurement_stamp_nanoseconds;
use crate::synthetic_frame_measurement_preamble::{
    SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES, SyntheticFrameMeasurementPreamble,
};

/// The recorder the sink feeds. Process-global because the sink is instantiated
/// by the engine, which offers no channel for handing an owned collaborator to a
/// processor instance — a spike artifact, not a proposed pattern.
fn measurement_collection_point() -> &'static Mutex<Option<LatencyMeasurementRecorder>> {
    static MEASUREMENT_COLLECTION_POINT: OnceLock<Mutex<Option<LatencyMeasurementRecorder>>> =
        OnceLock::new();
    MEASUREMENT_COLLECTION_POINT.get_or_init(|| Mutex::new(None))
}

/// Install the recorder for the cell about to run, discarding any recorder left
/// by a previous cell in the same process.
pub fn install_measurement_collection_point(recorder: LatencyMeasurementRecorder) {
    *measurement_collection_point()
        .lock()
        .expect("measurement collection point mutex is never held across a panic") =
        Some(recorder);
}

/// Take the recorder back once the graph has stopped.
pub fn take_measurement_collection_point() -> Option<LatencyMeasurementRecorder> {
    measurement_collection_point()
        .lock()
        .expect("measurement collection point mutex is never held across a panic")
        .take()
}

/// Configuration for [`MeasuringSinkProcessor`]. Empty by design — the sink
/// derives everything it reports from the preamble, so it cannot be
/// misconfigured into disagreeing with the source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MeasuringSinkConfiguration {}

#[streamlib::sdk::processor(
    "@spike/pyembed/MeasuringSink",
    execution = reactive,
    config = crate::measuring_sink_processor::MeasuringSinkConfiguration,
    config_field = "configuration",
    input("frame_in", any, delivery_profile = "every_sample"),
)]
pub struct MeasuringSinkProcessor;

impl ReactiveProcessor for MeasuringSinkProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if measurement_collection_point()
            .lock()
            .expect("measurement collection point mutex is never held across a panic")
            .is_none()
        {
            return Err(Error::Configuration(
                "no measurement collection point installed — the harness must install one \
                 before starting the graph, or the cell would run and record nothing"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let Some((frame_payload, _frame_timestamp_nanoseconds)) =
            self.inputs.read_raw("frame_in")?
        else {
            return Ok(());
        };
        // Stamped immediately after the read returns, so the engine's own
        // read-side allocation and memcpy land inside the measured interval —
        // they are part of the latency a user would experience.
        let sink_receive_monotonic_nanoseconds = read_measurement_stamp_nanoseconds();

        let preamble = SyntheticFrameMeasurementPreamble::read_from_payload_prefix(&frame_payload)
            .ok_or_else(|| {
                Error::Link(format!(
                    "frame payload of {} bytes is shorter than the {SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES}-byte measurement preamble",
                    frame_payload.len()
                ))
            })?;

        measurement_collection_point()
            .lock()
            .expect("measurement collection point mutex is never held across a panic")
            .as_mut()
            .ok_or_else(|| {
                Error::Configuration(
                    "measurement collection point was taken while the graph was still running"
                        .to_string(),
                )
            })?
            .record_frame_measurement(PerFrameLatencyMeasurement {
                frame_sequence_number: preamble.frame_sequence_number,
                source_emit_monotonic_nanoseconds: preamble.source_emit_monotonic_nanoseconds,
                sink_receive_monotonic_nanoseconds,
                stage_callback_nanoseconds: preamble.stage_callback_nanoseconds,
            });
        Ok(())
    }
}

// The second test asserts recorder contents, which a control build does not
// populate — see the note on the recorder's own test module.
#[cfg(all(test, not(feature = "stamping-compiled-out")))]
mod tests {
    use super::*;

    /// Installing then taking must hand back the same recorder — the harness
    /// reads its numbers exclusively through this handoff, so a lost recorder
    /// means a cell that ran for ten minutes and reported nothing.
    #[test]
    fn the_collection_point_round_trips_a_recorder() {
        install_measurement_collection_point(LatencyMeasurementRecorder::new(0, 0, 0));
        let recovered = take_measurement_collection_point();
        assert!(recovered.is_some());
        assert!(
            take_measurement_collection_point().is_none(),
            "taking twice must not resurrect a recorder"
        );
    }

    /// Installing a second recorder must not silently accumulate into the
    /// first — two cells in one process have to stay separate.
    #[test]
    fn installing_replaces_rather_than_merges() {
        let mut first = LatencyMeasurementRecorder::new(0, 0, 0);
        first.record_frame_measurement(PerFrameLatencyMeasurement {
            frame_sequence_number: 0,
            source_emit_monotonic_nanoseconds: 0,
            sink_receive_monotonic_nanoseconds: 1_000,
            stage_callback_nanoseconds: 0,
        });
        install_measurement_collection_point(first);
        install_measurement_collection_point(LatencyMeasurementRecorder::new(0, 0, 0));
        let recovered = take_measurement_collection_point().expect("second recorder is installed");
        assert_eq!(
            recovered.received_frame_count(),
            0,
            "the replacement recorder must not inherit the first cell's frames"
        );
    }
}
