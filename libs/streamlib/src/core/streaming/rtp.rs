// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::streaming::opus::EncodedAudioFrame;
use crate::core::{Result, StreamError};
use bytes::Bytes;
use std::time::Duration;

#[cfg(any(target_os = "macos", target_os = "ios"))]
use crate::apple::videotoolbox::{parse_nal_units, EncodedVideoFrame};

/// Converts encoded H.264 video frame to webrtc Sample(s).
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub fn convert_video_to_samples(
    frame: &EncodedVideoFrame,
    fps: u32,
) -> Result<Vec<webrtc::media::Sample>> {
    // Parse NAL units from Annex B format
    let nal_units = parse_nal_units(&frame.data);

    if nal_units.is_empty() {
        return Err(StreamError::Runtime(
            "No NAL units found in H.264 frame".into(),
        ));
    }

    // Calculate frame duration
    let duration = Duration::from_secs_f64(1.0 / fps as f64);

    // Convert each NAL unit to a Sample
    let samples = nal_units
        .into_iter()
        .map(|nal| webrtc::media::Sample {
            data: Bytes::from(nal),
            duration,
            ..Default::default()
        })
        .collect();

    Ok(samples)
}

/// Converts encoded Opus audio frame to webrtc Sample.
pub fn convert_audio_to_sample(
    frame: &EncodedAudioFrame,
    sample_rate: u32,
) -> Result<webrtc::media::Sample> {
    // Calculate duration from sample count
    let duration = Duration::from_secs_f64(frame.sample_count as f64 / sample_rate as f64);

    Ok(webrtc::media::Sample {
        data: Bytes::from(frame.data.clone()),
        duration,
        ..Default::default()
    })
}

/// Calculates RTP timestamps from monotonic MediaClock timestamps.
pub struct RtpTimestampCalculator {
    start_time_ns: i64,
    rtp_base: u32,
    clock_rate: u32,
}

impl RtpTimestampCalculator {
    /// Creates a new RTP timestamp calculator with random base.
    pub fn new(start_time_ns: i64, clock_rate: u32) -> Self {
        // Random RTP base (RFC 3550 compliance)
        // Uses fastrand for speed (~0.5ns) - doesn't need crypto-grade PRNG
        let rtp_base = fastrand::u32(..);

        Self {
            start_time_ns,
            rtp_base,
            clock_rate,
        }
    }

    /// Converts monotonic nanosecond timestamp to RTP timestamp.
    pub fn calculate(&self, timestamp_ns: i64) -> u32 {
        let elapsed_ns = timestamp_ns - self.start_time_ns;
        let elapsed_ticks = (elapsed_ns as i128 * self.clock_rate as i128) / 1_000_000_000;
        self.rtp_base.wrapping_add(elapsed_ticks as u32)
    }

    #[cfg(test)]
    pub fn rtp_base(&self) -> u32 {
        self.rtp_base
    }
}
