// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Streaming Utilities
//
// Core streaming components for audio/video encoding/decoding and RTP/WebRTC.

pub mod h264_rtp;
pub mod opus;
pub mod opus_decoder;
pub mod rtp;

pub use h264_rtp::H264RtpDepacketizer;
pub use opus::{AudioEncoderConfig, AudioEncoderOpus, EncodedAudioFrame, OpusEncoder};
pub use opus_decoder::OpusDecoder;
pub use rtp::{convert_audio_to_sample, convert_video_to_samples, RtpTimestampCalculator};
