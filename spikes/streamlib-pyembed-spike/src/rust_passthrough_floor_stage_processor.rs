// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The floor arm: structurally identical to the Python stage but with no
//! interpreter, isolating the engine's own wire-hop cost from PyO3's.
//!
//! Without this arm a large absolute latency is unattributable. The engine's
//! public `read_raw` allocates a fresh 64 KiB `Vec`, then a fresh full-size
//! `Vec`, then memcpys, on every read
//! (`runtime/streamlib-plugin-abi/src/vtables/input_mailboxes.rs:156-157`, the
//! allocation sitting inside the retry loop) — at 1080p BGRA that is an 8.3 MB
//! allocation per hop. Measured against this floor, the PyO3 delta is what the
//! pivot actually turns on; measured against nothing, it is confounded with the
//! wire hop.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ReactiveProcessor;

use crate::synthetic_frame_measurement_preamble::{
    SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES, SyntheticFrameMeasurementPreamble,
};
use crate::synthetic_frame_wire_payload_mode::SyntheticFrameWirePayloadMode;

/// Frame geometry and wire mode for [`RustPassthroughFloorStageProcessor`].
/// Deliberately mirrors the Python stage's fields so the two arms are
/// configured identically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RustPassthroughFloorStageConfiguration {
    pub frame_width_pixels: u32,
    pub frame_height_pixels: u32,
    pub channel_count: u32,
    #[serde(default)]
    pub wire_payload_mode: SyntheticFrameWirePayloadMode,
}

impl Default for RustPassthroughFloorStageConfiguration {
    fn default() -> Self {
        Self {
            frame_width_pixels: 1920,
            frame_height_pixels: 1080,
            channel_count: 4,
            wire_payload_mode: SyntheticFrameWirePayloadMode::SurfaceReference,
        }
    }
}

#[streamlib::sdk::processor(
    "@spike/pyembed/RustPassthroughFloorStage",
    execution = reactive,
    config = crate::rust_passthrough_floor_stage_processor::RustPassthroughFloorStageConfiguration,
    config_field = "configuration",
    input("frame_in", any, delivery_profile = "every_sample"),
    output("frame_out", any),
)]
pub struct RustPassthroughFloorStageProcessor;

impl ReactiveProcessor for RustPassthroughFloorStageProcessor::Processor {
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let Some((mut frame_payload, frame_timestamp_nanoseconds)) =
            self.inputs.read_raw("frame_in")?
        else {
            return Ok(());
        };
        if frame_payload.len() <= SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES {
            return Err(Error::Link(format!(
                "frame payload of {} bytes carries nothing after the measurement preamble",
                frame_payload.len()
            )));
        }

        // Zero, not a measured span: this arm's whole purpose is that nothing
        // happens between read and write, so the sink's stage-duration column
        // is meaningfully empty rather than noise.
        SyntheticFrameMeasurementPreamble::patch_stage_callback_nanoseconds_in_payload_prefix(
            &mut frame_payload,
            0,
        );
        self.outputs
            .write_raw("frame_out", &frame_payload, frame_timestamp_nanoseconds)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The floor arm is only a valid control if it is configured with the same
    /// geometry as the Python arm — a mismatch would compare different payload
    /// sizes and therefore different allocation costs.
    #[test]
    fn floor_geometry_defaults_match_the_python_stage_geometry() {
        let floor = RustPassthroughFloorStageConfiguration::default();
        let python = crate::python_callback_stage_processor::PythonCallbackStageConfiguration::default();
        assert_eq!(floor.frame_width_pixels, python.frame_width_pixels);
        assert_eq!(floor.frame_height_pixels, python.frame_height_pixels);
        assert_eq!(floor.channel_count, python.channel_count);
        assert_eq!(floor.wire_payload_mode, python.wire_payload_mode);
    }

    /// The floor arm's whole value is attributing latency to the wire hop, so
    /// it must carry the same wire payload mode the Python arm does. A floor
    /// measured on surface references against a Python arm measured on whole
    /// pictures would report the PyO3 delta as the difference between two
    /// payload sizes.
    #[test]
    fn the_floor_arm_defaults_to_the_same_wire_payload_mode_as_the_source() {
        assert_eq!(
            RustPassthroughFloorStageConfiguration::default().wire_payload_mode,
            crate::synthetic_frame_source_processor::SyntheticFrameSourceConfiguration::default()
                .wire_payload_mode
        );
    }

    /// The config must survive the serde round-trip the engine performs when it
    /// validates and stores an instance config.
    #[test]
    fn configuration_round_trips_through_serde_json() {
        let configuration = RustPassthroughFloorStageConfiguration {
            frame_width_pixels: 1280,
            frame_height_pixels: 720,
            channel_count: 4,
            wire_payload_mode: SyntheticFrameWirePayloadMode::FullPixelPayload,
        };
        let encoded = serde_json::to_value(&configuration).expect("serializes");
        let decoded: RustPassthroughFloorStageConfiguration =
            serde_json::from_value(encoded).expect("deserializes");
        assert_eq!(decoded, configuration);
    }
}
