// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for OpenGL context interop.

use crate::pixel_buffer_binding::PyRhiPixelBuffer;
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
    /// - Creating texture bindings
    /// - Updating texture bindings
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

    /// Create a reusable GL texture binding with a STABLE texture ID.
    ///
    /// The returned binding has a texture ID that NEVER changes. Call
    /// `update()` on the binding to rebind it to different pixel buffers -
    /// this is a fast operation that just updates the backing memory.
    ///
    /// # Usage Pattern
    ///
    /// ```python
    /// # In setup() - create binding ONCE
    /// gl_ctx.make_current()
    /// binding = gl_ctx.create_texture_binding()
    ///
    /// # In process() - update to new buffer (fast, zero-copy)
    /// binding.update(pixel_buffer)
    ///
    /// # Use binding.id with Skia - it's stable!
    /// skia_info = skia.GrGLTextureInfo(binding.target, binding.id, GL_RGBA8)
    /// ```
    ///
    /// The GL context must be current before calling this method.
    fn create_texture_binding(&self) -> PyResult<PyGlTextureBinding> {
        let guard = self.inner.lock();
        let binding = guard
            .create_texture_binding()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))?;
        Ok(PyGlTextureBinding::new(binding, Arc::clone(&self.inner)))
    }

    fn __repr__(&self) -> String {
        "GlContext(experimental)".to_string()
    }
}

// =============================================================================
// GL Texture Binding
// =============================================================================

/// A reusable GL texture binding with a STABLE texture ID.
///
/// Create via `gl_ctx.create_texture_binding()`. The `id` is stable and NEVER
/// changes - it's safe to cache in Skia objects.
///
/// Call `update()` each frame to rebind the texture to a new pixel buffer.
/// This is a fast operation - no new GL resources are created, just the
/// backing memory pointer is updated.
///
/// # Skia Integration
///
/// Because `id` is stable, you can create Skia backend objects ONCE and
/// reuse them:
///
/// ```python
/// # setup() - create binding and Skia objects ONCE
/// binding = gl_ctx.create_texture_binding()
/// binding.update(first_buffer)
/// skia_info = skia.GrGLTextureInfo(binding.target, binding.id, GL_RGBA8)
/// skia_backend = skia.GrBackendTexture(w, h, skia.GrMipmapped.kNo, skia_info)
/// skia_image = skia.Image.MakeFromTexture(ctx, skia_backend, ...)
///
/// # process() - just update binding, reuse Skia objects!
/// binding.update(current_buffer)
/// canvas.drawImage(skia_image, 0, 0)  # Reads from current buffer!
/// ```
///
/// Note: Marked unsendable because GL textures are thread-bound.
#[pyclass(name = "GlTextureBinding", unsendable)]
pub struct PyGlTextureBinding {
    inner: streamlib::GlTextureBinding,
    /// Reference to parent GL context for update operations
    gl_ctx: Arc<parking_lot::Mutex<GlContext>>,
}

impl PyGlTextureBinding {
    pub fn new(
        inner: streamlib::GlTextureBinding,
        gl_ctx: Arc<parking_lot::Mutex<GlContext>>,
    ) -> Self {
        Self { inner, gl_ctx }
    }
}

#[pymethods]
impl PyGlTextureBinding {
    /// The OpenGL texture name (ID). STABLE - never changes after creation.
    #[getter]
    fn id(&self) -> u32 {
        self.inner.texture_id()
    }

    /// The OpenGL texture target (GL_TEXTURE_RECTANGLE on macOS).
    #[getter]
    fn target(&self) -> u32 {
        self.inner.target()
    }

    /// Current bound buffer width (0 if not yet bound).
    #[getter]
    fn width(&self) -> u32 {
        self.inner.width()
    }

    /// Current bound buffer height (0 if not yet bound).
    #[getter]
    fn height(&self) -> u32 {
        self.inner.height()
    }

    /// Check if this binding is currently bound to a buffer.
    #[getter]
    fn is_bound(&self) -> bool {
        self.inner.is_bound()
    }

    /// Update this binding to a new pixel buffer.
    ///
    /// This is a FAST operation - it rebinds the GL texture to the new buffer's
    /// backing memory via zero-copy mechanisms. No new GL resources are created.
    ///
    /// After calling, any Skia objects using this binding's `id` will
    /// automatically see the new buffer content when rendered.
    ///
    /// # Requirements
    /// - GL context must be current
    /// - Pixel buffer must have GPU-compatible backing
    ///
    /// Example:
    ///     binding.update(pixel_buffer)
    ///     # Now skia_image backed by this binding shows new content
    fn update(&mut self, buffer: &PyRhiPixelBuffer) -> PyResult<()> {
        let guard = self.gl_ctx.lock();
        self.inner
            .update(&guard, buffer.inner())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("{}", e)))
    }

    fn __repr__(&self) -> String {
        format!(
            "GlTextureBinding(id={}, target=0x{:X}, {}x{}, bound={})",
            self.inner.texture_id(),
            self.inner.target(),
            self.inner.width(),
            self.inner.height(),
            self.inner.is_bound()
        )
    }
}
