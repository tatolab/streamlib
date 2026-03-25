// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::_generated_::Encodedvideoframe;
use crate::_generated_::Encodedaudioframe;
use crate::core::{Result, StreamError};
use bytes::Bytes;
use std::time::Duration;

/// Converts encoded H.264 video frame to webrtc Sample(s).
pub fn convert_video_to_samples(
    frame: &Encodedvideoframe,
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
    frame: &Encodedaudioframe,
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

/// Auto-detect format and parse NAL units.
pub(crate) fn parse_nal_units(data: &[u8]) -> Vec<Vec<u8>> {
    if data.len() < 4 {
        return Vec::new();
    }

    let is_annex_b = (data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 1)
        || (data[0] == 0 && data[1] == 0 && data[2] == 1);

    if is_annex_b {
        parse_nal_units_annex_b(data)
    } else {
        let length = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
        if length > 0 && length < 1_000_000 && length + 4 <= data.len() {
            parse_nal_units_avcc(data)
        } else {
            Vec::new()
        }
    }
}

fn parse_nal_units_avcc(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();
    let mut i = 0;
    while i + 4 <= data.len() {
        let nal_length =
            u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        i += 4;
        if i + nal_length > data.len() {
            break;
        }
        let nal_unit = data[i..i + nal_length].to_vec();
        if !nal_unit.is_empty() {
            nal_units.push(nal_unit);
        }
        i += nal_length;
    }
    nal_units
}

fn parse_nal_units_annex_b(data: &[u8]) -> Vec<Vec<u8>> {
    let mut nal_units = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let start_code_len = if i + 3 < data.len()
            && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1
        {
            4
        } else if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            3
        } else {
            i += 1;
            continue;
        };
        let mut nal_end = i + start_code_len;
        while nal_end < data.len() {
            if (nal_end + 3 < data.len()
                && data[nal_end] == 0 && data[nal_end + 1] == 0
                && data[nal_end + 2] == 0 && data[nal_end + 3] == 1)
                || (nal_end + 2 < data.len()
                    && data[nal_end] == 0 && data[nal_end + 1] == 0
                    && data[nal_end + 2] == 1)
            {
                break;
            }
            nal_end += 1;
        }
        let nal_unit = data[i + start_code_len..nal_end].to_vec();
        if !nal_unit.is_empty() {
            nal_units.push(nal_unit);
        }
        i = nal_end;
    }
    nal_units
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
