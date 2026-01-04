// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use crate::core::context::PooledTextureHandle;

#[crate::schema(content_hint = Video)]
#[derive(Clone)]
pub struct VideoFrame {
    #[crate::field(
        internal,
        field_type = "Arc<wgpu::Texture>",
        description = "GPU texture containing the frame pixel data"
    )]
    pub texture: Arc<wgpu::Texture>,

    /// Pooled texture handle (keeps texture alive in pool until frame is dropped).
    pooled_handle: Option<Arc<PooledTextureHandle>>,

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
    /// Create a VideoFrame from a non-pooled texture.
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
            pooled_handle: None,
            format,
            timestamp_ns,
            frame_number,
            width,
            height,
        }
    }

    /// Create a VideoFrame from a pooled texture handle.
    ///
    /// The handle is wrapped in Arc so it survives cloning for fan-out.
    /// When all clones are dropped, the texture is returned to the pool.
    pub fn from_pooled(handle: PooledTextureHandle, timestamp_ns: i64, frame_number: u64) -> Self {
        let width = handle.width();
        let height = handle.height();
        let format = handle.format();
        let texture = handle.texture_arc();
        let handle = Arc::new(handle);

        Self {
            texture,
            pooled_handle: Some(handle),
            format,
            timestamp_ns,
            frame_number,
            width,
            height,
        }
    }

    /// Check if this frame uses a pooled texture.
    pub fn is_pooled(&self) -> bool {
        self.pooled_handle.is_some()
    }

    /// Get the IOSurface ID if this frame uses an IOSurface-backed texture (macOS only).
    #[cfg(target_os = "macos")]
    pub fn iosurface_id(&self) -> Option<u32> {
        self.pooled_handle.as_ref().map(|h| h.iosurface_id())
    }

    /// Create a new frame with a different non-pooled texture, preserving metadata.
    ///
    /// Use this for shader output where you render to a new texture but want to
    /// preserve timestamp and frame number from the input frame.
    pub fn with_texture(&self, texture: Arc<wgpu::Texture>, format: wgpu::TextureFormat) -> Self {
        let size = texture.size();
        Self {
            texture,
            pooled_handle: None,
            format,
            timestamp_ns: self.timestamp_ns,
            frame_number: self.frame_number,
            width: size.width,
            height: size.height,
        }
    }

    /// Create a new frame with a pooled texture, preserving metadata.
    ///
    /// Use this for shader output where you render to a pooled texture but want to
    /// preserve timestamp and frame number from the input frame.
    pub fn with_pooled_texture(&self, handle: PooledTextureHandle) -> Self {
        let width = handle.width();
        let height = handle.height();
        let format = handle.format();
        let texture = handle.texture_arc();
        let handle = Arc::new(handle);

        Self {
            texture,
            pooled_handle: Some(handle),
            format,
            timestamp_ns: self.timestamp_ns,
            frame_number: self.frame_number,
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
