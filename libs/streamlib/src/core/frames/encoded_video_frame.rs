// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Encoded video frame for H.264/H.265 compressed video data.

/// Encoded video frame containing compressed video data (H.264/H.265 NAL units).
///
/// This is the output of video encoders and input to muxers/network transmitters.
/// Platform-agnostic representation that works with VideoToolbox, FFmpeg, etc.
#[crate::schema(content_hint = Video)]
#[derive(Clone)]
pub struct EncodedVideoFrame {
    /// The encoded NAL units (H.264/H.265 bitstream data).
    pub data: Vec<u8>,

    /// Monotonic timestamp in nanoseconds.
    #[crate::field(description = "Monotonic timestamp in nanoseconds")]
    pub timestamp_ns: i64,

    /// Whether this is a keyframe (I-frame).
    #[crate::field(description = "Whether this is a keyframe (I-frame)")]
    pub is_keyframe: bool,

    /// Sequential frame number.
    #[crate::field(description = "Sequential frame number")]
    pub frame_number: u64,
}

impl EncodedVideoFrame {
    /// Create a new encoded video frame.
    pub fn new(data: Vec<u8>, timestamp_ns: i64, is_keyframe: bool, frame_number: u64) -> Self {
        Self {
            data,
            timestamp_ns,
            is_keyframe,
            frame_number,
        }
    }

    /// Returns the size of the encoded data in bytes.
    pub fn data_len(&self) -> usize {
        self.data.len()
    }

    /// Returns true if the encoded data is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}
