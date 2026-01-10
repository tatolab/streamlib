// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! FFmpeg-based video encoding/decoding for Linux.

mod decoder;
mod encoder;
mod muxer;

pub use decoder::FFmpegDecoder;
pub use encoder::FFmpegEncoder;
pub use muxer::FFmpegMuxer;
