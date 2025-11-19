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

mod ffi;
mod codec;
mod format;
mod encoder;

// Public API exports
pub use codec::{VideoCodec, H264Profile, CodecInfo};
pub use encoder::{VideoToolboxEncoder, VideoEncoderConfig, EncodedVideoFrame};
pub use format::{parse_nal_units, parse_nal_units_avcc, parse_nal_units_annex_b};
