// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tier A's frame generator: emits fixed-size frames at a target rate with a
//! measurement preamble, standing in for a camera on the headless arm.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ContinuousProcessor;

use crate::monotonic_clock::{
    MonotonicNanoseconds, read_monotonic_clock_nanoseconds, spin_until_monotonic_deadline,
};
use crate::synthetic_frame_measurement_preamble::{
    SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES, SyntheticFrameMeasurementPreamble,
};

/// Frame geometry and pacing for [`SyntheticFrameSourceProcessor`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyntheticFrameSourceConfiguration {
    pub frame_width_pixels: u32,
    pub frame_height_pixels: u32,
    pub channel_count: u32,
    pub target_frames_per_second: u32,
}

impl Default for SyntheticFrameSourceConfiguration {
    fn default() -> Self {
        Self {
            frame_width_pixels: 1920,
            frame_height_pixels: 1080,
            channel_count: 4,
            target_frames_per_second: 30,
        }
    }
}

impl SyntheticFrameSourceConfiguration {
    /// Pixel bytes per frame, excluding the measurement preamble.
    pub fn frame_pixel_byte_count(&self) -> usize {
        self.frame_width_pixels as usize
            * self.frame_height_pixels as usize
            * self.channel_count as usize
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
        let total_payload_bytes = SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES
            + self.configuration.frame_pixel_byte_count();
        self.frame_payload_buffer = vec![0u8; total_payload_bytes];
        // A non-uniform pattern so a stage that silently drops or zeroes the
        // payload is distinguishable from one that passes it through.
        for (index, byte) in self.frame_payload_buffer
            [SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES..]
            .iter_mut()
            .enumerate()
        {
            *byte = (index % 251) as u8;
        }
        self.next_frame_sequence_number = 0;
        self.emission_origin_monotonic_nanoseconds = read_monotonic_clock_nanoseconds();
        tracing::info!(
            frame_bytes = total_payload_bytes,
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
            source_emit_monotonic_nanoseconds: read_monotonic_clock_nanoseconds(),
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

    /// 1080p BGRA plus the preamble must stay inside the 16 MiB untrusted-session
    /// payload ceiling, because the subprocess baseline arm's links are
    /// classified UntrustedSession (`open_iceoryx2_service_op.rs:149-153`,
    /// `streamlib-ipc-types/src/lib.rs:43`). Exceeding it would make the two
    /// arms structurally incomparable rather than merely slower.
    #[test]
    fn default_frame_fits_the_untrusted_session_payload_ceiling() {
        const UNTRUSTED_SESSION_PAYLOAD_CEILING_BYTES: usize = 16 * 1024 * 1024;
        let configuration = SyntheticFrameSourceConfiguration::default();
        let total_bytes = SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES
            + configuration.frame_pixel_byte_count();
        assert_eq!(configuration.frame_pixel_byte_count(), 8_294_400);
        assert!(
            total_bytes < UNTRUSTED_SESSION_PAYLOAD_CEILING_BYTES,
            "{total_bytes} bytes exceeds the subprocess arm's ceiling"
        );
    }

    /// 4K would fit the in-process arm's 64 MiB Trusted ceiling but is refused
    /// on the subprocess arm — the boundary that caps this benchmark at 1080p.
    #[test]
    fn four_k_frame_exceeds_the_untrusted_session_ceiling() {
        const UNTRUSTED_SESSION_PAYLOAD_CEILING_BYTES: usize = 16 * 1024 * 1024;
        let configuration = SyntheticFrameSourceConfiguration {
            frame_width_pixels: 3840,
            frame_height_pixels: 2160,
            ..SyntheticFrameSourceConfiguration::default()
        };
        assert!(configuration.frame_pixel_byte_count() > UNTRUSTED_SESSION_PAYLOAD_CEILING_BYTES);
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
        };
        let encoded = serde_json::to_value(&configuration).expect("serializes");
        let decoded: SyntheticFrameSourceConfiguration =
            serde_json::from_value(encoded).expect("deserializes");
        assert_eq!(decoded, configuration);
    }
}
