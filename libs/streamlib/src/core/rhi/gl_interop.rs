// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! OpenGL interop for cross-framework GPU texture sharing.
//!
//! This module provides platform-agnostic OpenGL context and texture binding
//! for interoperability with libraries that require OpenGL (e.g., Skia).
//!
//! On each platform, native GPU textures are bound to OpenGL textures:
//! - macOS: IOSurface → GL texture via `CGLTexImageIOSurface2D`
//! - Linux: DMA-BUF → GL texture via `EGL_EXT_image_dma_buf_import` (future)
//! - Windows: DXGI → GL texture via `WGL_NV_DX_interop` (future)

use crate::core::{Result, StreamError};
use std::collections::HashMap;
use std::ffi::c_void;

/// OpenGL texture target constants.
pub mod gl_constants {
    /// GL_TEXTURE_2D - standard 2D texture.
    pub const GL_TEXTURE_2D: u32 = 0x0DE1;
    /// GL_TEXTURE_RECTANGLE - required for IOSurface textures on macOS.
    pub const GL_TEXTURE_RECTANGLE: u32 = 0x84F5;
    /// GL_RGBA8 - 8-bit RGBA internal format.
    pub const GL_RGBA8: u32 = 0x8058;
}

/// Platform-agnostic OpenGL context for GPU interop.
///
/// This context is owned by StreamLib's runtime and provides OpenGL access
/// to native GPU textures. Third-party libraries (like Skia) can use this
/// context to render into StreamLib's texture pool.
pub struct GlContext {
    #[cfg(target_os = "macos")]
    inner: crate::apple::rhi::gl_interop_macos::MacOsGlContext,

    /// Cache of bound GL textures: native_handle_id -> (gl_texture_id, gl_target)
    texture_cache: HashMap<u64, GlTextureBinding>,
}

/// A bound OpenGL texture with its target type.
#[derive(Debug, Clone, Copy)]
pub struct GlTextureBinding {
    /// OpenGL texture ID.
    pub texture_id: u32,
    /// OpenGL texture target (e.g., GL_TEXTURE_RECTANGLE).
    pub target: u32,
}

impl GlContext {
    /// Create a new OpenGL context for GPU interop.
    ///
    /// This creates a platform-appropriate OpenGL context that can share
    /// GPU memory with the native graphics API (Metal, Vulkan, DX12).
    pub fn new() -> Result<Self> {
        #[cfg(target_os = "macos")]
        {
            let inner = crate::apple::rhi::gl_interop_macos::MacOsGlContext::new()?;
            Ok(Self {
                inner,
                texture_cache: HashMap::new(),
            })
        }
        #[cfg(target_os = "linux")]
        {
            Err(StreamError::NotSupported(
                "GL interop not yet implemented for Linux".into(),
            ))
        }
        #[cfg(target_os = "windows")]
        {
            Err(StreamError::NotSupported(
                "GL interop not yet implemented for Windows".into(),
            ))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Err(StreamError::NotSupported(
                "GL interop not supported on this platform".into(),
            ))
        }
    }

    /// Make this context current on the calling thread.
    ///
    /// Must be called before any OpenGL operations, including accessing
    /// GL texture IDs or using Skia's GrDirectContext.MakeGL().
    pub fn make_current(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.inner.make_current()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(StreamError::NotSupported(
                "GL interop not supported on this platform".into(),
            ))
        }
    }

    /// Clear the current context on this thread.
    pub fn clear_current(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.inner.clear_current()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(())
        }
    }

    /// Flush all pending OpenGL commands.
    ///
    /// Call this after Skia drawing and before reading the texture from
    /// the native API (Metal, Vulkan, etc.) to ensure all GL commands
    /// have completed.
    pub fn flush(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.inner.flush()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(())
        }
    }

    /// Bind a native texture to an OpenGL texture.
    ///
    /// Returns the GL texture ID and target. The binding is cached, so
    /// subsequent calls with the same native handle return the cached texture.
    ///
    /// # Arguments
    /// * `native_handle` - Platform-native texture handle (IOSurface ID, DMA-BUF fd, etc.)
    /// * `width` - Texture width in pixels
    /// * `height` - Texture height in pixels
    pub fn bind_texture(
        &mut self,
        native_handle: &super::NativeTextureHandle,
        width: u32,
        height: u32,
    ) -> Result<GlTextureBinding> {
        let cache_key = native_handle_to_cache_key(native_handle);

        // Return cached binding if available
        if let Some(binding) = self.texture_cache.get(&cache_key) {
            return Ok(*binding);
        }

        // Create new binding
        let binding = self.create_texture_binding(native_handle, width, height)?;
        self.texture_cache.insert(cache_key, binding);
        Ok(binding)
    }

    /// Invalidate a cached texture binding.
    ///
    /// Call this when the underlying native texture is destroyed or recycled.
    pub fn invalidate_texture(&mut self, native_handle: &super::NativeTextureHandle) {
        let cache_key = native_handle_to_cache_key(native_handle);
        if let Some(binding) = self.texture_cache.remove(&cache_key) {
            #[cfg(target_os = "macos")]
            {
                self.inner.delete_texture(binding.texture_id);
            }
        }
    }

    /// Clear all cached texture bindings.
    pub fn clear_texture_cache(&mut self) {
        #[cfg(target_os = "macos")]
        {
            for binding in self.texture_cache.values() {
                self.inner.delete_texture(binding.texture_id);
            }
        }
        self.texture_cache.clear();
    }

    #[cfg(target_os = "macos")]
    fn create_texture_binding(
        &self,
        native_handle: &super::NativeTextureHandle,
        width: u32,
        height: u32,
    ) -> Result<GlTextureBinding> {
        match native_handle {
            super::NativeTextureHandle::IOSurface { id } => {
                let texture_id = self.inner.bind_iosurface(*id, width, height)?;
                Ok(GlTextureBinding {
                    texture_id,
                    target: gl_constants::GL_TEXTURE_RECTANGLE,
                })
            }
            _ => Err(StreamError::NotSupported(
                "Only IOSurface handles supported on macOS".into(),
            )),
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn create_texture_binding(
        &self,
        _native_handle: &super::NativeTextureHandle,
        _width: u32,
        _height: u32,
    ) -> Result<GlTextureBinding> {
        Err(StreamError::NotSupported(
            "GL texture binding not implemented for this platform".into(),
        ))
    }

    /// Get the raw CGL context pointer (macOS only).
    ///
    /// This is needed for advanced interop scenarios.
    #[cfg(target_os = "macos")]
    pub fn cgl_context_ptr(&self) -> *mut c_void {
        self.inner.context_ptr()
    }
}

impl Drop for GlContext {
    fn drop(&mut self) {
        self.clear_texture_cache();
    }
}

// GlContext is Send but not Sync - GL contexts are thread-bound
unsafe impl Send for GlContext {}

fn native_handle_to_cache_key(handle: &super::NativeTextureHandle) -> u64 {
    match handle {
        super::NativeTextureHandle::IOSurface { id } => *id as u64,
        super::NativeTextureHandle::DmaBuf { fd } => *fd as u64,
        super::NativeTextureHandle::DxgiSharedHandle { handle } => *handle,
    }
}
