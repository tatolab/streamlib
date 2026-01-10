// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Platform-agnostic video codec types and encoder/decoder configuration.

mod audio_codec;
mod mp4_muxer;
mod mp4_muxer_config;
mod video_codec;
mod video_decoder;
mod video_decoder_config;
mod video_encoder;
mod video_encoder_config;

pub use audio_codec::AudioCodec;
pub use mp4_muxer::Mp4Muxer;
pub use mp4_muxer_config::Mp4MuxerConfig;
pub use video_codec::{H264Profile, VideoCodec, FOURCC_H264};
pub use video_decoder::VideoDecoder;
pub use video_decoder_config::VideoDecoderConfig;
pub use video_encoder::VideoEncoder;
pub use video_encoder_config::VideoEncoderConfig;
