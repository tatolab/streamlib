// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tier A's frame generator: emits fixed-size frames at a target rate with a
//! measurement preamble, standing in for a camera on the headless arm.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ContinuousProcessor;

use crate::monotonic_clock::{
    MonotonicNanoseconds, read_measurement_stamp_nanoseconds, read_monotonic_clock_nanoseconds,
    spin_until_monotonic_deadline,
};
use crate::synthetic_frame_measurement_preamble::{
    SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES, SyntheticFrameMeasurementPreamble,
};
use crate::synthetic_frame_wire_payload_mode::{
    SyntheticFrameWirePayloadMode, encode_surface_reference_body,
};

/// Frame geometry, pacing, and what rides the wire for
/// [`SyntheticFrameSourceProcessor`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyntheticFrameSourceConfiguration {
    pub frame_width_pixels: u32,
    pub frame_height_pixels: u32,
    pub channel_count: u32,
    pub target_frames_per_second: u32,
    /// The geometry above always describes the frame; this decides whether the
    /// pixels for it actually cross the link (owner decision 7 on #1702).
    #[serde(default)]
    pub wire_payload_mode: SyntheticFrameWirePayloadMode,
    /// Quiet period between `setup()` and the first emitted frame.
    ///
    /// Load-bearing for the subprocess baseline arm and harmless for the other
    /// two. `Runner::start()` returns once the graph is compiled, but a Python
    /// subprocess needs a few hundred milliseconds more to spawn, import, and
    /// reach its poll loop. Frames emitted into that window queue up, and under
    /// `every_sample` the backlog is never dropped — it drains only as fast as
    /// the consumer runs ahead of the source. Measured on this branch at
    /// 720p60: p50 183ms over a 20s cell, 84ms over a 40s cell, against 0.09ms
    /// for the in-process arm. Those are startup transients, not latencies, and
    /// excluding warmup *by time* does not remove them because the backlog
    /// outlives the exclusion window.
    #[serde(default = "default_startup_settle_seconds")]
    pub startup_settle_seconds: f64,
}

/// Comfortably past the ~0.66s the subprocess arm takes to reach its poll loop.
fn default_startup_settle_seconds() -> f64 {
    2.0
}

impl Default for SyntheticFrameSourceConfiguration {
    fn default() -> Self {
        Self {
            frame_width_pixels: 1920,
            frame_height_pixels: 1080,
            channel_count: 4,
            target_frames_per_second: 30,
            wire_payload_mode: SyntheticFrameWirePayloadMode::SurfaceReference,
            startup_settle_seconds: default_startup_settle_seconds(),
        }
    }
}

impl SyntheticFrameSourceConfiguration {
    /// Pixel bytes for one frame of this geometry, whether or not they cross
    /// the wire.
    pub fn frame_pixel_byte_count(&self) -> usize {
        self.frame_width_pixels as usize
            * self.frame_height_pixels as usize
            * self.channel_count as usize
    }

    /// The bytes riding behind the measurement preamble under this mode.
    pub fn wire_body_bytes(&self) -> Vec<u8> {
        match self.wire_payload_mode {
            SyntheticFrameWirePayloadMode::SurfaceReference => encode_surface_reference_body(
                self.frame_width_pixels,
                self.frame_height_pixels,
                self.target_frames_per_second,
            ),
            SyntheticFrameWirePayloadMode::FullPixelPayload => {
                // A non-uniform pattern so a stage that silently drops or zeroes
                // the payload is distinguishable from one that passes it through.
                (0..self.frame_pixel_byte_count())
                    .map(|index| (index % 251) as u8)
                    .collect()
            }
        }
    }

    /// Nanoseconds between consecutive frame emissions at the target rate.
    pub fn frame_period_nanoseconds(&self) -> Result<i64> {
        if self.target_frames_per_second == 0 {
            return Err(Error::Configuration(
                "target_frames_per_second must be greater than zero".to_string(),
            ));
        }
        Ok(1_000_000_000 / self.target_frames_per_second as i64)
    }
}

#[streamlib::sdk::processor(
    "@spike/pyembed/SyntheticFrameSource",
    execution = continuous,
    config = crate::synthetic_frame_source_processor::SyntheticFrameSourceConfiguration,
    config_field = "configuration",
    output("frame_out", any),
)]
pub struct SyntheticFrameSourceProcessor {
    next_frame_sequence_number: u64,
    frame_payload_buffer: Vec<u8>,
    /// Absolute deadlines derived from a fixed origin rather than accumulated
    /// per-frame sleeps. The engine's continuous loop runs `process()` then
    /// sleeps its own interval (`execution/thread_runner.rs:150-158`), so a
    /// relative pacer would drift by that interval every frame.
    emission_origin_monotonic_nanoseconds: MonotonicNanoseconds,
}

impl ContinuousProcessor for SyntheticFrameSourceProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let wire_body = self.configuration.wire_body_bytes();
        self.frame_payload_buffer =
            Vec::with_capacity(SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES + wire_body.len());
        self.frame_payload_buffer
            .resize(SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES, 0u8);
        self.frame_payload_buffer.extend_from_slice(&wire_body);
        self.next_frame_sequence_number = 0;
        self.emission_origin_monotonic_nanoseconds = read_monotonic_clock_nanoseconds()
            + (self.configuration.startup_settle_seconds * 1_000_000_000.0) as i64;
        tracing::info!(
            frame_bytes = self.frame_payload_buffer.len(),
            startup_settle_seconds = self.configuration.startup_settle_seconds,
            wire_payload_mode = self.configuration.wire_payload_mode.as_artifact_token(),
            target_fps = self.configuration.target_frames_per_second,
            "synthetic frame source ready"
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let frame_period_nanoseconds = self.configuration.frame_period_nanoseconds()?;
        let emission_deadline = self.emission_origin_monotonic_nanoseconds
            + (self.next_frame_sequence_number as i64) * frame_period_nanoseconds;
        spin_until_monotonic_deadline(emission_deadline);

        // The emit stamp is taken after pacing and immediately before the
        // write, so pacing wait never lands inside the measured latency.
        let preamble = SyntheticFrameMeasurementPreamble {
            frame_sequence_number: self.next_frame_sequence_number,
            source_emit_monotonic_nanoseconds: read_measurement_stamp_nanoseconds(),
            // The stage patches this in place; the source leaves it zero so a
            // frame that never reached a stage is distinguishable.
            stage_callback_nanoseconds: 0,
        };
        if !preamble.write_into_payload_prefix(&mut self.frame_payload_buffer) {
            return Err(Error::Configuration(
                "frame payload buffer is smaller than the measurement preamble".to_string(),
            ));
        }

        self.outputs.write_raw(
            "frame_out",
            &self.frame_payload_buffer,
            preamble.source_emit_monotonic_nanoseconds,
        )?;
        self.next_frame_sequence_number += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full-pixel 1080p frame plus the preamble must stay inside the 16 MiB
    /// untrusted-session payload ceiling, because the subprocess baseline arm's
    /// links are classified UntrustedSession (`open_iceoryx2_service_op.rs:149-153`,
    /// `streamlib-ipc-types/src/lib.rs:43`). Exceeding it would make the two
    /// arms structurally incomparable rather than merely slower.
    #[test]
    fn a_full_pixel_1080p_frame_fits_the_untrusted_session_payload_ceiling() {
        const UNTRUSTED_SESSION_PAYLOAD_CEILING_BYTES: usize = 16 * 1024 * 1024;
        let configuration = SyntheticFrameSourceConfiguration {
            wire_payload_mode: SyntheticFrameWirePayloadMode::FullPixelPayload,
            ..SyntheticFrameSourceConfiguration::default()
        };
        let total_bytes =
            SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES + configuration.wire_body_bytes().len();
        assert_eq!(configuration.frame_pixel_byte_count(), 8_294_400);
        assert!(
            total_bytes < UNTRUSTED_SESSION_PAYLOAD_CEILING_BYTES,
            "{total_bytes} bytes exceeds the subprocess arm's ceiling"
        );
    }

    /// 4K pixels would fit the in-process arm's 64 MiB Trusted ceiling but are
    /// refused on the subprocess arm — the boundary that caps the full-pixel
    /// sweep at 1080p. The reference mode is not bound by it at any geometry.
    #[test]
    fn four_k_pixels_exceed_the_untrusted_session_ceiling_but_a_reference_does_not() {
        const UNTRUSTED_SESSION_PAYLOAD_CEILING_BYTES: usize = 16 * 1024 * 1024;
        let four_k = SyntheticFrameSourceConfiguration {
            frame_width_pixels: 3840,
            frame_height_pixels: 2160,
            wire_payload_mode: SyntheticFrameWirePayloadMode::FullPixelPayload,
            ..SyntheticFrameSourceConfiguration::default()
        };
        assert!(four_k.wire_body_bytes().len() > UNTRUSTED_SESSION_PAYLOAD_CEILING_BYTES);

        let four_k_by_reference = SyntheticFrameSourceConfiguration {
            wire_payload_mode: SyntheticFrameWirePayloadMode::SurfaceReference,
            ..four_k
        };
        assert!(
            four_k_by_reference.wire_body_bytes().len()
                < UNTRUSTED_SESSION_PAYLOAD_CEILING_BYTES
        );
    }

    /// Owner decision 7: the protocol cells carry surface references, not
    /// pictures. A default that regressed to full pixels would silently
    /// reproduce the retracted 27ms floor and report it as a gated number.
    #[test]
    fn the_source_defaults_to_putting_a_surface_reference_on_the_wire() {
        let configuration = SyntheticFrameSourceConfiguration::default();
        assert_eq!(
            configuration.wire_payload_mode,
            SyntheticFrameWirePayloadMode::SurfaceReference
        );
        assert!(configuration.wire_body_bytes().len() < 1024);
    }

    /// The 1080p60 cell is only measurable because the wire body no longer
    /// tracks geometry: a full-pixel two-hop service time exceeds the 16.6ms
    /// frame period and the cell runs permanently saturated.
    #[test]
    fn a_surface_reference_wire_body_does_not_grow_with_frame_geometry() {
        let seven_twenty = SyntheticFrameSourceConfiguration {
            frame_width_pixels: 1280,
            frame_height_pixels: 720,
            ..SyntheticFrameSourceConfiguration::default()
        };
        let ten_eighty = SyntheticFrameSourceConfiguration::default();
        assert_eq!(
            seven_twenty.wire_body_bytes().len(),
            ten_eighty.wire_body_bytes().len(),
            "the resolution leg must vary pixel work, never transport width"
        );
        assert_ne!(
            seven_twenty.frame_pixel_byte_count(),
            ten_eighty.frame_pixel_byte_count()
        );
    }

    /// The full-pixel body must keep the non-uniform pattern, which is what
    /// distinguishes a stage that passed the payload through from one that
    /// zeroed it.
    #[test]
    fn the_full_pixel_wire_body_carries_a_non_uniform_pattern() {
        let configuration = SyntheticFrameSourceConfiguration {
            frame_width_pixels: 4,
            frame_height_pixels: 2,
            channel_count: 4,
            wire_payload_mode: SyntheticFrameWirePayloadMode::FullPixelPayload,
            ..SyntheticFrameSourceConfiguration::default()
        };
        let body = configuration.wire_body_bytes();
        assert_eq!(body.len(), 32);
        assert_eq!(body[0], 0);
        assert_eq!(body[31], 31);
    }

    /// Both protocol rates must produce exact periods; a rate that divided
    /// unevenly would accumulate pacing error across a 10-minute cell.
    #[test]
    fn protocol_frame_rates_yield_expected_periods() {
        for (frames_per_second, expected_period_nanoseconds) in [(30, 33_333_333), (60, 16_666_666)]
        {
            let configuration = SyntheticFrameSourceConfiguration {
                target_frames_per_second: frames_per_second,
                ..SyntheticFrameSourceConfiguration::default()
            };
            assert_eq!(
                configuration.frame_period_nanoseconds().expect("valid rate"),
                expected_period_nanoseconds
            );
        }
    }

    /// A zero rate must surface a configuration error rather than dividing by
    /// zero inside the processor loop.
    #[test]
    fn zero_frame_rate_is_a_configuration_error() {
        let configuration = SyntheticFrameSourceConfiguration {
            target_frames_per_second: 0,
            ..SyntheticFrameSourceConfiguration::default()
        };
        assert!(configuration.frame_period_nanoseconds().is_err());
    }

    /// The config must survive the serde round-trip the engine performs when it
    /// validates and stores an instance config.
    #[test]
    fn configuration_round_trips_through_serde_json() {
        let configuration = SyntheticFrameSourceConfiguration {
            frame_width_pixels: 1280,
            frame_height_pixels: 720,
            channel_count: 4,
            target_frames_per_second: 60,
            wire_payload_mode: SyntheticFrameWirePayloadMode::FullPixelPayload,
            startup_settle_seconds: 2.5,
        };
        let encoded = serde_json::to_value(&configuration).expect("serializes");
        let decoded: SyntheticFrameSourceConfiguration =
            serde_json::from_value(encoded).expect("deserializes");
        assert_eq!(decoded, configuration);
    }

    /// The settle exists because the subprocess arm needs ~0.66s to reach its
    /// poll loop; frames emitted before then become a backlog that the warmup
    /// exclusion cannot remove. A default of zero would silently restore the
    /// 183ms-vs-0.09ms comparison that measured startup transients.
    #[test]
    fn the_source_settles_before_its_first_frame_by_default() {
        assert!(SyntheticFrameSourceConfiguration::default().startup_settle_seconds >= 1.0);
    }

    /// An older `cell-spec.json` predating the settle must not silently replay
    /// with no settle at all — the field defaults to the protocol value.
    #[test]
    fn a_configuration_without_a_startup_settle_decodes_to_the_protocol_default() {
        let decoded: SyntheticFrameSourceConfiguration = serde_json::from_value(
            serde_json::json!({
                "frame_width_pixels": 1280,
                "frame_height_pixels": 720,
                "channel_count": 4,
                "target_frames_per_second": 60,
            }),
        )
        .expect("deserializes without the settle");
        assert_eq!(
            decoded.startup_settle_seconds,
            SyntheticFrameSourceConfiguration::default().startup_settle_seconds
        );
    }

    /// A config written before the mode existed must decode to the protocol
    /// default rather than failing, so a cell can be replayed from an older
    /// `cell-spec.json` without hand-editing it.
    #[test]
    fn a_configuration_without_a_wire_payload_mode_decodes_to_surface_reference() {
        let decoded: SyntheticFrameSourceConfiguration = serde_json::from_value(
            serde_json::json!({
                "frame_width_pixels": 1280,
                "frame_height_pixels": 720,
                "channel_count": 4,
                "target_frames_per_second": 60,
            }),
        )
        .expect("deserializes without the mode");
        assert_eq!(
            decoded.wire_payload_mode,
            SyntheticFrameWirePayloadMode::SurfaceReference
        );
    }
}
