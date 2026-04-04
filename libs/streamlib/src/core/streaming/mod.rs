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
#[cfg(feature = "moq")]
pub mod moq_catalog;
#[cfg(feature = "moq")]
pub mod moq_session;

pub use h264_rtp::H264RtpDepacketizer;
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub(crate) use opus::{AudioEncoderConfig, AudioEncoderOpus, OpusEncoder};
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
pub(crate) use opus_decoder::OpusDecoder;
pub use rtp::convert_video_to_samples;
pub use rtp::convert_audio_to_sample;
pub use rtp::RtpTimestampCalculator;
pub use webrtc_session::WebRtcSession;
pub use whep_client::{RtpSample, WhepClient, WhepConfig};
pub use whip_client::{WhipClient, WhipConfig};
#[cfg(feature = "moq")]
pub use moq_catalog::{MoqBroadcastCatalog, MoqCatalogTrackEntry};
#[cfg(feature = "moq")]
pub use moq_session::{MoqRelayConfig, MoqPublishSession, MoqSubscribeSession, MoqTrackReader, MoqSubgroupReader, SharedMoqSessions, DEFAULT_MOQ_RELAY_URL};
