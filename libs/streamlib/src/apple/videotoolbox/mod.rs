// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// VideoToolbox Module
//
// Hardware-accelerated video encoding using Apple's VideoToolbox framework.
// Provides a reusable encoder abstraction that can be used by various processors
// (WebRTC, file writers, network streamers, etc.).
//
// ## Current Status
// - **H.264/AVC**: Fully implemented and tested
// - **H.265/HEVC**: Codec enum prepared, not yet implemented
// - **AV1**: Codec enum prepared, not yet implemented
//
// ## Architecture
// ```
// VideoFrame (wgpu texture)
//     ↓
// GPU-accelerated RGBA → NV12 conversion (Metal/VideoToolbox)
//     ↓
// VideoToolboxEncoder::encode()
//     ↓
// EncodedVideoFrame (Annex B format for H.264)
// ```
//
// ## Usage Example
// ```rust
// use streamlib::apple::videotoolbox::{VideoToolboxEncoder, VideoEncoderConfig, VideoCodec, H264Profile};
//
// let config = VideoEncoderConfig {
//     width: 1920,
//     height: 1080,
//     fps: 60,
//     bitrate_bps: 5_000_000,
//     codec: VideoCodec::H264(H264Profile::High),
//     ..Default::default()
// };
//
// let mut encoder = VideoToolboxEncoder::new(config, gpu_context, &runtime_context)?;
//
// // Encode a frame
// let encoded = encoder.encode(&video_frame)?;
// println!("Encoded {} bytes, keyframe={}", encoded.data.len(), encoded.is_keyframe);
// ```

mod codec;
mod decoder;
mod encoder;
mod ffi;
pub mod format; // Public for SPS parsing utilities

// Public API exports
pub use codec::{H264Profile, VideoCodec};
pub use decoder::VideoToolboxDecoder;
pub use encoder::{EncodedVideoFrame, VideoEncoderConfig, VideoToolboxEncoder};
pub use format::parse_nal_units;
