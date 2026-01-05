// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS OpenGL interop implementation using CGL and IOSurface.
//!
//! This module provides the macOS-specific implementation for binding
//! IOSurface-backed textures to OpenGL textures via `CGLTexImageIOSurface2D`.

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

// IOSurface lookup
#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    fn IOSurfaceLookup(csid: u32) -> *const c_void;
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

    /// Bind an IOSurface to an OpenGL texture.
    ///
    /// Returns the GL texture ID on success.
    pub fn bind_iosurface(&self, iosurface_id: u32, width: u32, height: u32) -> Result<u32> {
        unsafe {
            // Ensure we have a current context
            let current = CGLGetCurrentContext();
            if current.is_null() {
                return Err(StreamError::GpuError(
                    "No current GL context. Call make_current() first.".into(),
                ));
            }

            // Look up the IOSurface
            let iosurface = IOSurfaceLookup(iosurface_id);
            if iosurface.is_null() {
                return Err(StreamError::GpuError(format!(
                    "IOSurfaceLookup failed for ID: {}",
                    iosurface_id
                )));
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

            // Bind IOSurface to texture - this is the zero-copy magic
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
                glDeleteTextures(1, &texture_id);
                return Err(StreamError::GpuError(format!(
                    "CGLTexImageIOSurface2D failed with error: {}",
                    result
                )));
            }

            // Check for GL errors
            let gl_error = glGetError();
            if gl_error != 0 {
                tracing::warn!("GL error after IOSurface binding: 0x{:X}", gl_error);
            }

            tracing::trace!(
                "Bound IOSurface {} to GL texture {} ({}x{})",
                iosurface_id,
                texture_id,
                width,
                height
            );

            Ok(texture_id)
        }
    }

    /// Delete a GL texture.
    pub fn delete_texture(&self, texture_id: u32) {
        unsafe {
            glDeleteTextures(1, &texture_id);
        }
    }

    /// Get the raw CGL context pointer.
    pub fn context_ptr(&self) -> *mut c_void {
        self.cgl_context
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
}
