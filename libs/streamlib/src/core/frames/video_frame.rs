// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

#[streamlib::schema(port_type = "Video")]
#[derive(Clone)]
pub struct VideoFrame {
    #[streamlib::field(not_serializable)]
    pub texture: Arc<wgpu::Texture>,

    #[streamlib::field(skip)]
    pub format: wgpu::TextureFormat,

    pub timestamp_ns: i64,

    pub frame_number: u64,

    pub width: u32,

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
