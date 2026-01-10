// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Streaming Utilities
//
// Core streaming components for audio/video encoding/decoding and RTP/WebRTC.

pub mod h264_rtp;
pub mod opus;
pub mod opus_decoder;
pub mod rtp;
pub mod webrtc_session;
pub mod whep_client;
pub mod whip_client;

pub use h264_rtp::H264RtpDepacketizer;
pub use opus::{AudioEncoderConfig, AudioEncoderOpus, EncodedAudioFrame, OpusEncoder};
pub use opus_decoder::OpusDecoder;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use rtp::convert_video_to_samples;
pub use rtp::{convert_audio_to_sample, RtpTimestampCalculator};
pub use webrtc_session::WebRtcSession;
pub use whep_client::{RtpSample, WhepClient, WhepConfig};
pub use whip_client::{WhipClient, WhipConfig};
