// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Streaming Utilities
//
// Core streaming components for audio/video encoding/decoding and RTP/WebRTC.

pub mod h264_rtp;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub mod opus;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub mod opus_decoder;
pub mod rtp;
pub mod webrtc_session;
pub mod whep_client;
pub mod whip_client;

pub use h264_rtp::H264RtpDepacketizer;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub use opus::{AudioEncoderConfig, AudioEncoderOpus, EncodedAudioFrame, OpusEncoder};

/// Encoded audio frame (cross-platform definition for muxer support).
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "linux")))]
#[derive(Clone, Debug)]
pub struct EncodedAudioFrame {
    pub data: Vec<u8>,
    pub timestamp_ns: i64,
    pub sample_count: usize,
}
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub use opus_decoder::OpusDecoder;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use rtp::convert_video_to_samples;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use rtp::convert_audio_to_sample;
pub use rtp::RtpTimestampCalculator;
pub use webrtc_session::WebRtcSession;
pub use whep_client::{RtpSample, WhepClient, WhepConfig};
pub use whip_client::{WhipClient, WhipConfig};
