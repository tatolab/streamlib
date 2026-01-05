// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for OpenGL context interop.

use pyo3::prelude::*;
use std::sync::Arc;
use streamlib::GlContext;

/// Python-accessible OpenGL context for GPU interop.
///
/// This context enables third-party libraries like Skia to render into
/// StreamLib's GPU textures. The context is owned by StreamLib's runtime
/// and shared with Python code.
///
/// Access via `ctx.gpu._experimental_gl_context()`.
#[pyclass(name = "GlContext")]
pub struct PyGlContext {
    /// Shared GL context (wrapped in Arc for Python's reference counting)
    inner: Arc<parking_lot::Mutex<GlContext>>,
}

impl PyGlContext {
    pub fn new(ctx: GlContext) -> Self {
        Self {
            inner: Arc::new(parking_lot::Mutex::new(ctx)),
        }
    }

    /// Get a locked reference to the inner context for texture binding.
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, GlContext> {
        self.inner.lock()
    }
}

impl Clone for PyGlContext {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[pymethods]
impl PyGlContext {
    /// Make this OpenGL context current on the calling thread.
    ///
    /// Must be called before any OpenGL operations, including:
    /// - Accessing `_experimental_gl_texture_id()` on textures
    /// - Creating Skia's `GrDirectContext.MakeGL()`
    /// - Any Skia drawing operations
    ///
    /// Example:
    ///     gl_ctx = ctx.gpu._experimental_gl_context()
    ///     gl_ctx.make_current()
    ///     skia_ctx = skia.GrDirectContext.MakeGL()
    fn make_current(&self) -> PyResult<()> {
        self.inner
            .lock()
            .make_current()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))
    }

    /// Clear the current OpenGL context on this thread.
    fn clear_current(&self) -> PyResult<()> {
        self.inner
            .lock()
            .clear_current()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))
    }

    /// Flush all pending OpenGL commands.
    ///
    /// Call this after Skia drawing operations and before the frame is
    /// consumed by Metal/Vulkan to ensure all GL rendering is complete.
    ///
    /// Example:
    ///     canvas.drawRect(...)
    ///     skia_ctx.flush()
    ///     skia_ctx.submit(True)
    ///     gl_ctx.flush()  # Ensure GL commands complete before Metal reads
    fn flush(&self) -> PyResult<()> {
        self.inner
            .lock()
            .flush()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))
    }

    /// Get the GL texture target constant for IOSurface textures.
    ///
    /// On macOS, IOSurface textures require GL_TEXTURE_RECTANGLE (0x84F5).
    /// Use this when constructing Skia's GrGLTextureInfo.
    #[getter]
    fn texture_target(&self) -> u32 {
        streamlib::gl_constants::GL_TEXTURE_RECTANGLE
    }

    /// Get the GL internal format constant.
    ///
    /// Returns GL_RGBA8 (0x8058) for use with Skia's GrGLTextureInfo.
    #[getter]
    fn internal_format(&self) -> u32 {
        streamlib::gl_constants::GL_RGBA8
    }

    fn __repr__(&self) -> String {
        "GlContext(experimental)".to_string()
    }
}
