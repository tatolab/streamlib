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

use crate::core::rhi::RhiPixelBuffer;
use crate::core::Result;
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
///
/// # Usage
///
/// 1. Create texture bindings in `setup()` (they have stable IDs)
/// 2. Update bindings to new buffers in `process()` (fast, zero-copy)
/// 3. Use binding's `texture_id` with Skia or other GL-based libraries
///
/// ```ignore
/// // setup()
/// let binding = gl_ctx.create_texture_binding()?;
///
/// // process() - each frame
/// binding.update(&gl_ctx, &buffer)?;  // Fast rebind
/// // Use binding.texture_id with Skia - it never changes!
/// ```
pub struct GlContext {
    #[cfg(target_os = "macos")]
    inner: crate::apple::rhi::gl_interop_macos::MacOsGlContext,
}

/// A reusable GL texture binding with a STABLE texture ID.
///
/// Create via [`GlContext::create_texture_binding()`]. The `texture_id` is
/// stable and NEVER changes - it's safe to cache in Skia objects.
///
/// Call [`update()`](GlTextureBinding::update) each frame to rebind the texture
/// to a new pixel buffer. This is a fast operation - no new GL resources are
/// created, just the backing memory pointer is updated.
///
/// # Skia Integration
///
/// Because `texture_id` is stable, you can create Skia backend objects ONCE
/// and reuse them:
///
/// ```ignore
/// // setup() - create binding and Skia objects ONCE
/// let binding = gl_ctx.create_texture_binding()?;
/// binding.update(&gl_ctx, &first_buffer)?;
/// let skia_info = GrGLTextureInfo(binding.target, binding.texture_id, GL_RGBA8);
/// let skia_backend = GrBackendTexture::new(w, h, GrMipmapped::No, skia_info);
/// let skia_image = Image::MakeFromTexture(ctx, skia_backend, ...);
///
/// // process() - just update binding, reuse Skia objects!
/// binding.update(&gl_ctx, &current_buffer)?;
/// canvas.drawImage(skia_image, 0, 0);  // Reads from current buffer!
/// ```
pub struct GlTextureBinding {
    #[cfg(target_os = "macos")]
    inner: crate::apple::rhi::gl_interop_macos::GlTextureBinding,
}

impl GlTextureBinding {
    /// The OpenGL texture name (ID). STABLE - never changes after creation.
    pub fn texture_id(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            self.inner.texture_id
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// The OpenGL texture target (GL_TEXTURE_RECTANGLE on macOS).
    pub fn target(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            self.inner.target
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// Current bound buffer width (0 if not yet bound).
    pub fn width(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            self.inner.width
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// Current bound buffer height (0 if not yet bound).
    pub fn height(&self) -> u32 {
        #[cfg(target_os = "macos")]
        {
            self.inner.height
        }
        #[cfg(not(target_os = "macos"))]
        {
            0
        }
    }

    /// Update this binding to a new pixel buffer.
    ///
    /// This is a FAST operation - it rebinds the GL texture to the new buffer's
    /// backing memory via platform-specific zero-copy mechanisms. No new GL
    /// resources are created.
    ///
    /// After calling, any Skia objects using this binding's `texture_id` will
    /// automatically see the new buffer content when rendered.
    ///
    /// # Requirements
    /// - GL context must be current
    /// - Pixel buffer must have GPU-compatible backing
    pub fn update(&mut self, gl_ctx: &GlContext, buffer: &RhiPixelBuffer) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            self.inner.update(&gl_ctx.inner, buffer)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (gl_ctx, buffer);
            Err(StreamError::NotSupported(
                "GL texture binding not supported on this platform".into(),
            ))
        }
    }

    /// Check if this binding is currently bound to a buffer.
    pub fn is_bound(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.inner.is_bound()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }
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
            Ok(Self { inner })
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
    /// Must be called before any OpenGL operations, including:
    /// - Creating texture bindings
    /// - Updating texture bindings
    /// - Using Skia's GrDirectContext.MakeGL()
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

    /// Create a reusable GL texture binding with a STABLE texture ID.
    ///
    /// The returned binding has a texture ID that NEVER changes. Call
    /// [`GlTextureBinding::update()`] on the binding to rebind it to different
    /// pixel buffers - this is a fast operation that just updates the backing
    /// memory, not the GL texture itself.
    ///
    /// # Usage Pattern
    ///
    /// ```ignore
    /// // In setup() - create binding ONCE
    /// let binding = gl_ctx.create_texture_binding()?;
    ///
    /// // In process() - update to new buffer (fast, zero-copy)
    /// binding.update(&gl_ctx, &pixel_buffer)?;
    ///
    /// // Use binding.texture_id() with Skia - it's stable!
    /// let skia_info = GrGLTextureInfo(binding.target(), binding.texture_id(), GL_RGBA8);
    /// ```
    ///
    /// # Requirements
    /// - GL context must be current (call `make_current()` first)
    pub fn create_texture_binding(&self) -> Result<GlTextureBinding> {
        #[cfg(target_os = "macos")]
        {
            let inner = self.inner.create_texture_binding()?;
            Ok(GlTextureBinding { inner })
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(StreamError::NotSupported(
                "GL texture binding not supported on this platform".into(),
            ))
        }
    }

    /// Delete a GL texture explicitly.
    ///
    /// Normally textures are cleaned up when bindings are dropped, but this
    /// allows explicit cleanup when needed.
    pub fn delete_texture(&self, texture_id: u32) {
        #[cfg(target_os = "macos")]
        {
            self.inner.delete_texture(texture_id);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = texture_id;
        }
    }

    /// Get the raw CGL context pointer (macOS only).
    ///
    /// This is needed for advanced interop scenarios.
    #[cfg(target_os = "macos")]
    pub fn cgl_context_ptr(&self) -> *mut c_void {
        self.inner.context_ptr()
    }
}

// GlContext is Send but not Sync - GL contexts are thread-bound
unsafe impl Send for GlContext {}
