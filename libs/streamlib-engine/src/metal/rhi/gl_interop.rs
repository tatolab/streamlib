// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS OpenGL interop implementation using CGL and CGLTexImageIOSurface2D.
//!
//! This module provides the macOS-specific implementation for binding
//! CVPixelBuffer-backed textures to OpenGL textures via IOSurface.
//!
//! Key design:
//! - Each GL context is thread-bound (only one thread can use it)
//! - GL textures are created ONCE and reused via `GlTextureBinding`
//! - `CGLTexImageIOSurface2D` rebinds existing textures to new IOSurfaces (zero-copy)
//! - Texture IDs are STABLE - safe to cache in Skia objects

use crate::apple::corevideo_ffi::CVPixelBufferRef;
use crate::core::rhi::RhiPixelBuffer;
use crate::core::{Result, StreamError};
use std::ffi::c_void;

// =============================================================================
// FFI Bindings
// =============================================================================

// CGL (Core OpenGL) bindings
#[link(name = "OpenGL", kind = "framework")]
extern "C" {
    fn CGLChoosePixelFormat(attribs: *const i32, pix: *mut *mut c_void, npix: *mut i32) -> i32;
    fn CGLCreateContext(pix: *mut c_void, share: *mut c_void, ctx: *mut *mut c_void) -> i32;
    fn CGLDestroyContext(ctx: *mut c_void) -> i32;
    fn CGLDestroyPixelFormat(pix: *mut c_void) -> i32;
    fn CGLSetCurrentContext(ctx: *mut c_void) -> i32;
    fn CGLGetCurrentContext() -> *mut c_void;
    fn CGLTexImageIOSurface2D(
        ctx: *mut c_void,
        target: u32,
        internal_format: u32,
        width: i32,
        height: i32,
        format: u32,
        typ: u32,
        iosurface: *const c_void,
        plane: u32,
    ) -> i32;
}

// IOSurface bindings (using c_void for local use - equivalent to IOSurface*)
#[allow(clashing_extern_declarations)]
#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    fn IOSurfaceGetID(surface: *const c_void) -> u32;
}

// CoreVideo binding to get IOSurface from CVPixelBuffer (using c_void for local use)
#[allow(clashing_extern_declarations)]
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferGetIOSurface(pixel_buffer: CVPixelBufferRef) -> *const c_void;
}

// OpenGL bindings
#[link(name = "OpenGL", kind = "framework")]
extern "C" {
    fn glGenTextures(n: i32, textures: *mut u32);
    fn glDeleteTextures(n: i32, textures: *const u32);
    fn glBindTexture(target: u32, texture: u32);
    fn glTexParameteri(target: u32, pname: u32, param: i32);
    fn glFinish();
    fn glGetError() -> u32;
}

// CGL pixel format attributes
const K_CGL_PFA_ACCELERATED: i32 = 73;
const K_CGL_PFA_OPENGL_PROFILE: i32 = 99;
const K_CGL_OGL_PVERSION_3_2_CORE: i32 = 0x3200;
const K_CGL_PFA_COLOR_SIZE: i32 = 8;
const K_CGL_PFA_ALPHA_SIZE: i32 = 11;
const K_CGL_PFA_DOUBLE_BUFFER: i32 = 5;
const K_CGL_PFA_ALLOW_OFFLINE_RENDERERS: i32 = 96;

// OpenGL constants
const GL_TEXTURE_RECTANGLE: u32 = 0x84F5;
const GL_RGBA8: u32 = 0x8058;
const GL_BGRA: u32 = 0x80E1;
const GL_UNSIGNED_INT_8_8_8_8_REV: u32 = 0x8367;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_TEXTURE_WRAP_S: u32 = 0x2802;
const GL_TEXTURE_WRAP_T: u32 = 0x2803;
const GL_LINEAR: i32 = 0x2601;
const GL_CLAMP_TO_EDGE: i32 = 0x812F;

// =============================================================================
// MacOS GL Context Implementation
// =============================================================================

/// macOS-specific OpenGL context using CGL.
///
/// Use `create_texture_binding()` to create reusable GL texture bindings.
/// The context is thread-bound - only use it from one thread at a time.
pub struct MacOsGlContext {
    cgl_context: *mut c_void,
    pixel_format: *mut c_void,
}

impl MacOsGlContext {
    /// Create a new CGL context for IOSurface interop.
    pub fn new() -> Result<Self> {
        unsafe {
            // Build pixel format attributes
            let attributes: [i32; 11] = [
                K_CGL_PFA_ACCELERATED,
                K_CGL_PFA_ALLOW_OFFLINE_RENDERERS,
                K_CGL_PFA_OPENGL_PROFILE,
                K_CGL_OGL_PVERSION_3_2_CORE,
                K_CGL_PFA_COLOR_SIZE,
                24,
                K_CGL_PFA_ALPHA_SIZE,
                8,
                K_CGL_PFA_DOUBLE_BUFFER,
                0, // terminator
                0, // extra terminator for safety
            ];

            let mut pixel_format: *mut c_void = std::ptr::null_mut();
            let mut num_formats: i32 = 0;

            let err =
                CGLChoosePixelFormat(attributes.as_ptr(), &mut pixel_format, &mut num_formats);

            if err != 0 || pixel_format.is_null() {
                return Err(StreamError::GpuError(format!(
                    "CGLChoosePixelFormat failed with error: {}",
                    err
                )));
            }

            // Create context
            let mut context: *mut c_void = std::ptr::null_mut();
            let err = CGLCreateContext(pixel_format, std::ptr::null_mut(), &mut context);

            if err != 0 || context.is_null() {
                CGLDestroyPixelFormat(pixel_format);
                return Err(StreamError::GpuError(format!(
                    "CGLCreateContext failed with error: {}",
                    err
                )));
            }

            tracing::debug!("Created CGL context for GL interop");

            Ok(Self {
                cgl_context: context,
                pixel_format,
            })
        }
    }

    /// Make this context current on the calling thread.
    pub fn make_current(&self) -> Result<()> {
        unsafe {
            let err = CGLSetCurrentContext(self.cgl_context);
            if err != 0 {
                return Err(StreamError::GpuError(format!(
                    "CGLSetCurrentContext failed with error: {}",
                    err
                )));
            }
        }
        Ok(())
    }

    /// Clear the current context on this thread.
    pub fn clear_current(&self) -> Result<()> {
        unsafe {
            CGLSetCurrentContext(std::ptr::null_mut());
        }
        Ok(())
    }

    /// Flush all pending GL commands.
    pub fn flush(&self) -> Result<()> {
        unsafe {
            glFinish();
        }
        Ok(())
    }

    /// Get the raw CGL context pointer.
    pub fn context_ptr(&self) -> *mut c_void {
        self.cgl_context
    }

    /// Create a reusable GL texture binding with a STABLE texture ID.
    ///
    /// The returned binding has a texture ID that NEVER changes. Call `update()`
    /// on the binding to rebind it to different pixel buffers - this is a fast
    /// operation that just updates the IOSurface backing, not the GL texture.
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
    /// // Use binding.texture_id with Skia - it's stable!
    /// let skia_info = GrGLTextureInfo(binding.target, binding.texture_id, GL_RGBA8);
    /// ```
    pub fn create_texture_binding(&self) -> Result<GlTextureBinding> {
        unsafe {
            // Ensure we have a current context
            let current = CGLGetCurrentContext();
            if current.is_null() {
                return Err(StreamError::GpuError(
                    "No current GL context. Call make_current() first.".into(),
                ));
            }

            // Generate texture
            let mut texture_id: u32 = 0;
            glGenTextures(1, &mut texture_id);

            if texture_id == 0 {
                return Err(StreamError::GpuError(
                    "glGenTextures failed to create texture".into(),
                ));
            }

            // Bind and configure texture
            glBindTexture(GL_TEXTURE_RECTANGLE, texture_id);
            glTexParameteri(GL_TEXTURE_RECTANGLE, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
            glTexParameteri(GL_TEXTURE_RECTANGLE, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
            glTexParameteri(GL_TEXTURE_RECTANGLE, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
            glTexParameteri(GL_TEXTURE_RECTANGLE, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
            glBindTexture(GL_TEXTURE_RECTANGLE, 0);

            // Check for GL errors
            let gl_error = glGetError();
            if gl_error != 0 {
                glDeleteTextures(1, &texture_id);
                return Err(StreamError::GpuError(format!(
                    "GL error during texture creation: 0x{:X}",
                    gl_error
                )));
            }

            tracing::debug!(
                "Created reusable GL texture binding: texture_id={}, target=GL_TEXTURE_RECTANGLE",
                texture_id
            );

            Ok(GlTextureBinding {
                texture_id,
                target: GL_TEXTURE_RECTANGLE,
                width: 0,
                height: 0,
                bound_iosurface_id: None,
            })
        }
    }

    /// Internal: Rebind a GL texture to an IOSurface via CGLTexImageIOSurface2D.
    ///
    /// This is a fast operation - it just updates the texture's backing memory
    /// to point to a different IOSurface. No new GL resources are created.
    fn rebind_texture_to_iosurface(
        &self,
        texture_id: u32,
        iosurface: *const c_void,
        width: u32,
        height: u32,
    ) -> Result<()> {
        unsafe {
            // Ensure we have a current context
            let current = CGLGetCurrentContext();
            if current != self.cgl_context {
                return Err(StreamError::GpuError(
                    "GL context must be current to rebind texture".into(),
                ));
            }

            // Bind texture
            glBindTexture(GL_TEXTURE_RECTANGLE, texture_id);

            // Rebind IOSurface to texture - this is the zero-copy magic
            let result = CGLTexImageIOSurface2D(
                self.cgl_context,
                GL_TEXTURE_RECTANGLE,
                GL_RGBA8,
                width as i32,
                height as i32,
                GL_BGRA,
                GL_UNSIGNED_INT_8_8_8_8_REV,
                iosurface,
                0, // plane
            );

            glBindTexture(GL_TEXTURE_RECTANGLE, 0);

            if result != 0 {
                return Err(StreamError::GpuError(format!(
                    "CGLTexImageIOSurface2D failed with error: {}",
                    result
                )));
            }

            // Check for GL errors
            let gl_error = glGetError();
            if gl_error != 0 {
                tracing::warn!("GL error after IOSurface rebind: 0x{:X}", gl_error);
            }

            Ok(())
        }
    }

    /// Delete a GL texture.
    pub fn delete_texture(&self, texture_id: u32) {
        unsafe {
            glDeleteTextures(1, &texture_id);
        }
    }
}

impl Drop for MacOsGlContext {
    fn drop(&mut self) {
        unsafe {
            if !self.cgl_context.is_null() {
                // Clear context if it's current
                let current = CGLGetCurrentContext();
                if current == self.cgl_context {
                    CGLSetCurrentContext(std::ptr::null_mut());
                }
                CGLDestroyContext(self.cgl_context);
            }
            if !self.pixel_format.is_null() {
                CGLDestroyPixelFormat(self.pixel_format);
            }
        }
        tracing::debug!("Destroyed CGL context");
    }
}

// MacOsGlContext is Send but not Sync (GL contexts are thread-bound)
unsafe impl Send for MacOsGlContext {}

// =============================================================================
// GL Texture Binding
// =============================================================================

/// A reusable GL texture binding with a STABLE texture ID.
///
/// Create via `MacOsGlContext::create_texture_binding()`. The `texture_id` is
/// stable and NEVER changes - it's safe to cache in Skia objects.
///
/// Call `update()` each frame to rebind the texture to a new pixel buffer.
/// This is a fast operation using `CGLTexImageIOSurface2D` - no new GL
/// resources are created, just the backing memory pointer is updated.
///
/// # Example
///
/// ```ignore
/// // setup() - create binding and Skia objects ONCE
/// let binding = gl_ctx.create_texture_binding()?;
/// binding.update(&gl_ctx, &first_buffer)?;
/// let skia_backend = GrBackendTexture::new(
///     w, h, GrMipmapped::No,
///     GrGLTextureInfo(binding.target, binding.texture_id, GL_RGBA8)
/// );
/// let skia_image = Image::MakeFromTexture(ctx, skia_backend, ...);
///
/// // process() - just update binding, reuse Skia objects!
/// binding.update(&gl_ctx, &current_buffer)?;
/// canvas.drawImage(skia_image, 0, 0);  // Reads from current buffer!
/// ```
pub struct GlTextureBinding {
    /// The OpenGL texture name (ID). STABLE - never changes after creation.
    pub texture_id: u32,
    /// The OpenGL texture target (GL_TEXTURE_RECTANGLE on macOS).
    pub target: u32,
    /// Current bound buffer width (0 if not yet bound).
    pub width: u32,
    /// Current bound buffer height (0 if not yet bound).
    pub height: u32,
    /// IOSurface ID of currently bound buffer (for debugging).
    bound_iosurface_id: Option<u32>,
}

impl GlTextureBinding {
    /// Update this binding to a new pixel buffer.
    ///
    /// This is a FAST operation - it rebinds the GL texture to the new buffer's
    /// IOSurface via `CGLTexImageIOSurface2D`. No new GL resources are created.
    ///
    /// After calling, any Skia objects using this binding's `texture_id` will
    /// automatically see the new buffer content when rendered.
    ///
    /// # Requirements
    /// - GL context must be current
    /// - Pixel buffer must have IOSurface backing (created with proper attributes)
    pub fn update(&mut self, gl_ctx: &MacOsGlContext, buffer: &RhiPixelBuffer) -> Result<()> {
        unsafe {
            let cv_buffer: CVPixelBufferRef = buffer.as_ptr();

            // Get IOSurface from CVPixelBuffer
            let iosurface = CVPixelBufferGetIOSurface(cv_buffer);
            if iosurface.is_null() {
                return Err(StreamError::GpuError(
                    "CVPixelBuffer has no IOSurface backing. \
                     Ensure buffer was created with kCVPixelBufferIOSurfacePropertiesKey."
                        .into(),
                ));
            }

            let iosurface_id = IOSurfaceGetID(iosurface);
            let width = buffer.width;
            let height = buffer.height;

            // Rebind texture to new IOSurface
            gl_ctx.rebind_texture_to_iosurface(self.texture_id, iosurface, width, height)?;

            // Update state
            self.width = width;
            self.height = height;
            self.bound_iosurface_id = Some(iosurface_id);

            tracing::trace!(
                "Updated GL texture {} to IOSurface {} ({}x{})",
                self.texture_id,
                iosurface_id,
                width,
                height
            );

            Ok(())
        }
    }

    /// Check if this binding is currently bound to a buffer.
    pub fn is_bound(&self) -> bool {
        self.bound_iosurface_id.is_some()
    }
}

impl Drop for GlTextureBinding {
    fn drop(&mut self) {
        // Note: We can't delete the texture here because we don't have access
        // to the GL context. The texture will be cleaned up when the GL context
        // is destroyed. For explicit cleanup, use gl_ctx.delete_texture().
        tracing::trace!(
            "GlTextureBinding dropped (texture_id={}, cleanup deferred to context)",
            self.texture_id
        );
    }
}

// GlTextureBinding is Send but not Sync (GL textures are context-bound)
unsafe impl Send for GlTextureBinding {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gl_context_creation() {
        let ctx = MacOsGlContext::new();
        assert!(ctx.is_ok(), "Failed to create GL context: {:?}", ctx.err());

        let ctx = ctx.unwrap();
        assert!(ctx.make_current().is_ok());
        assert!(ctx.flush().is_ok());
        assert!(ctx.clear_current().is_ok());
    }

    #[test]
    fn test_gl_context_ptr() {
        let ctx = MacOsGlContext::new().unwrap();
        assert!(!ctx.context_ptr().is_null());
    }

    #[test]
    fn test_create_texture_binding() {
        let ctx = MacOsGlContext::new().unwrap();
        ctx.make_current().unwrap();

        let binding = ctx.create_texture_binding();
        assert!(
            binding.is_ok(),
            "Failed to create texture binding: {:?}",
            binding.err()
        );

        let binding = binding.unwrap();
        assert!(binding.texture_id > 0);
        assert_eq!(binding.target, GL_TEXTURE_RECTANGLE);
        assert_eq!(binding.width, 0);
        assert_eq!(binding.height, 0);
        assert!(!binding.is_bound());

        ctx.clear_current().unwrap();
    }
}
