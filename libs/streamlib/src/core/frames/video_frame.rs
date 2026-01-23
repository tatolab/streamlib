// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::rhi::{PixelFormat, RhiPixelBuffer};

/// Video frame containing a GPU pixel buffer and metadata.
///
/// VideoFrame wraps an `RhiPixelBuffer` which holds a reference to
/// platform pixel data (CVPixelBuffer on macOS). To render the frame,
/// create a texture view using `RhiTextureCache::create_view()`.
///
/// Schema: `com.tatolab.videoframe@1.0.0`
#[derive(Clone)]
pub struct VideoFrame {
    /// The pixel buffer containing the frame data.
    buffer: RhiPixelBuffer,

    /// Monotonic timestamp in nanoseconds.
    pub timestamp_ns: i64,

    /// Sequential frame number.
    pub frame_number: u64,
}

impl VideoFrame {
    /// Create a VideoFrame from a pixel buffer.
    pub fn from_buffer(buffer: RhiPixelBuffer, timestamp_ns: i64, frame_number: u64) -> Self {
        Self {
            buffer,
            timestamp_ns,
            frame_number,
        }
    }

    /// Get a reference to the pixel buffer.
    pub fn buffer(&self) -> &RhiPixelBuffer {
        &self.buffer
    }

    /// Frame width in pixels.
    pub fn width(&self) -> u32 {
        self.buffer.width
    }

    /// Frame height in pixels.
    pub fn height(&self) -> u32 {
        self.buffer.height
    }

    /// Get the pixel format.
    pub fn pixel_format(&self) -> PixelFormat {
        self.buffer.format()
    }

    // ========================================================================
    // EXAMPLES (for schema generation)
    // ========================================================================

    pub fn example_720p() -> serde_json::Value {
        serde_json::json!({
            "width": 1280,
            "height": 720,
            "format": "Bgra32",
            "timestamp_ns": 33_000_000,
            "frame_number": 1
        })
    }

    pub fn example_1080p() -> serde_json::Value {
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "format": "Bgra32",
            "timestamp_ns": 33_000_000,
            "frame_number": 1
        })
    }

    pub fn example_4k() -> serde_json::Value {
        serde_json::json!({
            "width": 3840,
            "height": 2160,
            "format": "Bgra32",
            "timestamp_ns": 33_000_000,
            "frame_number": 1
        })
    }
}
