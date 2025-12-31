// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

#[crate::schema(content_hint = Video)]
#[derive(Clone)]
pub struct VideoFrame {
    #[crate::field(
        internal,
        field_type = "Arc<wgpu::Texture>",
        description = "GPU texture containing the frame pixel data"
    )]
    pub texture: Arc<wgpu::Texture>,

    #[crate::field(
        internal,
        field_type = "wgpu::TextureFormat",
        description = "Pixel format of the texture (e.g., Rgba8Unorm)"
    )]
    pub format: wgpu::TextureFormat,

    #[crate::field(description = "Monotonic timestamp in nanoseconds")]
    pub timestamp_ns: i64,

    #[crate::field(description = "Sequential frame number")]
    pub frame_number: u64,

    #[crate::field(description = "Frame width in pixels")]
    pub width: u32,

    #[crate::field(description = "Frame height in pixels")]
    pub height: u32,
}

impl VideoFrame {
    pub fn new(
        texture: Arc<wgpu::Texture>,
        format: wgpu::TextureFormat,
        timestamp_ns: i64,
        frame_number: u64,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            texture,
            format,
            timestamp_ns,
            frame_number,
            width,
            height,
        }
    }

    pub fn example_720p() -> serde_json::Value {
        serde_json::json!({
            "width": 1280,
            "height": 720,
            "format": "Rgba8Unorm",
            "timestamp_ns": 33_000_000,
            "frame_number": 1
        })
    }

    pub fn example_1080p() -> serde_json::Value {
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "format": "Rgba8Unorm",
            "timestamp_ns": 33_000_000,
            "frame_number": 1
        })
    }

    pub fn example_4k() -> serde_json::Value {
        serde_json::json!({
            "width": 3840,
            "height": 2160,
            "format": "Rgba8Unorm",
            "timestamp_ns": 33_000_000,
            "frame_number": 1
        })
    }
}
