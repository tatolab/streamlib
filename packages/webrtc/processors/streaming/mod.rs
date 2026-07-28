// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! WebRTC session + WHIP/WHEP client primitives backing the
//! `WebRtcWhipProcessor` and `WebRtcWhepProcessor`.

pub mod h264_rtp;
pub mod rtp;
pub mod session;
pub mod whep_client;
pub mod whip_client;

pub use h264_rtp::H264RtpDepacketizer;
pub use rtp::{convert_audio_to_sample, convert_video_to_samples, RtpTimestampCalculator};
pub use session::WebRtcSession;
pub use whep_client::{RtpSample, WhepClient, WhepConfig};
pub use whip_client::{WhipClient, WhipConfig};
