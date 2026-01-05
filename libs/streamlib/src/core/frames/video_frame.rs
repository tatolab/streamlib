// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use crate::core::context::PooledTextureHandle;
use crate::core::rhi::{NativeTextureHandle, StreamTexture, TextureFormat};

#[crate::schema(content_hint = Video)]
#[derive(Clone)]
pub struct VideoFrame {
    #[crate::field(
        internal,
        field_type = "StreamTexture",
        description = "GPU texture containing the frame pixel data"
    )]
    pub texture: StreamTexture,

    /// Pooled texture handle (keeps texture alive in pool until frame is dropped).
    pooled_handle: Option<Arc<PooledTextureHandle>>,

    #[crate::field(
        internal,
        field_type = "TextureFormat",
        description = "Pixel format of the texture (e.g., Rgba8Unorm)"
    )]
    pub format: TextureFormat,

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
        texture: StreamTexture,
        format: TextureFormat,
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
        let texture = handle.texture_clone();
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

    /// Get the IOSurface ID for cross-framework sharing.
    ///
    /// Returns `Some(id)` on macOS/iOS if the texture is backed by an IOSurface.
    /// Returns `None` on other platforms or if no IOSurface is available.
    pub fn iosurface_id(&self) -> Option<u32> {
        self.texture.iosurface_id()
    }

    /// Get the platform-native sharing handle for this texture.
    ///
    /// Returns the appropriate handle type for the current platform:
    /// - macOS/iOS: `IOSurface { id }`
    /// - Linux: `DmaBuf { fd }` (when implemented)
    /// - Windows: `DxgiSharedHandle { handle }` (when implemented)
    ///
    /// Returns `None` if no sharing handle is available.
    pub fn native_handle(&self) -> Option<NativeTextureHandle> {
        self.texture.native_handle()
    }

    /// Get the underlying Metal texture (macOS only).
    #[cfg(target_os = "macos")]
    pub fn metal_texture(&self) -> &metal::TextureRef {
        self.texture.as_metal_texture()
    }

    /// Bind this frame's texture to an OpenGL texture and return the binding info.
    ///
    /// This enables interop with OpenGL-based libraries like Skia.
    /// See [`StreamTexture::gl_texture_binding`] for details.
    pub fn gl_texture_binding(
        &self,
        gl_ctx: &mut crate::core::rhi::GlContext,
    ) -> crate::core::Result<crate::core::rhi::GlTextureBinding> {
        self.texture.gl_texture_binding(gl_ctx)
    }

    /// Create a new frame with a different non-pooled texture, preserving metadata.
    ///
    /// Use this for shader output where you render to a new texture but want to
    /// preserve timestamp and frame number from the input frame.
    pub fn with_texture(&self, texture: StreamTexture, format: TextureFormat) -> Self {
        let width = texture.width();
        let height = texture.height();
        Self {
            texture,
            pooled_handle: None,
            format,
            timestamp_ns: self.timestamp_ns,
            frame_number: self.frame_number,
            width,
            height,
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
        let texture = handle.texture_clone();
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
