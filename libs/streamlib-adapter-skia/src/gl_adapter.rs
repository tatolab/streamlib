// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `SkiaGlSurfaceAdapter` — Skia-typed `SurfaceAdapter` composing on
//! [`OpenGlSurfaceAdapter`].
//!
//! Customers writing Skia code on top of an existing GL stack
//! (skia-python, gst-plugin-skia, custom GL applications) acquire
//! through this adapter and get a `skia_safe::Surface` (write) or
//! `skia_safe::Image` (read) directly. The `gl_texture_id`,
//! EGLImage, and DMA-BUF FD never reach them.
//!
//! Mirror-shape of [`crate::SkiaSurfaceAdapter`] but composed on
//! [`OpenGlSurfaceAdapter`] via the `GlWritable` capability marker
//! from `streamlib-adapter-abi`. Both backends share the same
//! [`crate::skia_internal::SyncDirectContext`] mutex pattern and
//! drop-order template — see `skia_internal.rs`.

use std::sync::{Arc, Mutex};

use skia_safe::gpu::{
    self, backend_textures, direct_contexts, gl as gpu_gl, surfaces, Mipmapped, Protected,
    SurfaceOrigin,
};
use skia_safe::{AlphaType, ColorSpace, ColorType};
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, SurfaceFormat, SurfaceId,
    WriteGuard,
};
use streamlib_adapter_opengl::{EglRuntime, OpenGlSurfaceAdapter, GL_TEXTURE_2D};

use crate::error::SkiaAdapterError;
use crate::gl_view::{SkiaGlReadView, SkiaGlWriteView};
use crate::skia_internal::SyncDirectContext;

/// Skia surface adapter composed on the OpenGL/EGL adapter.
///
/// Constructs a single Skia GL `DirectContext` shared by all
/// `acquire_*` calls; the EGL context must be current at construction
/// time (handled internally) and at every Skia operation (handled by
/// the views' drop hooks via [`EglRuntime::lock_make_current`]).
pub struct SkiaGlSurfaceAdapter {
    inner: Arc<OpenGlSurfaceAdapter>,
    egl: Arc<EglRuntime>,
    direct_context: Arc<Mutex<SyncDirectContext>>,
}

impl SkiaGlSurfaceAdapter {
    /// Build a Skia GL adapter on top of an existing OpenGL adapter.
    ///
    /// Acquires `lock_make_current`, builds a Skia GL interface via
    /// `interfaces::make_egl()` (which resolves GL function pointers
    /// via `eglGetProcAddress`), constructs the `GrDirectContext`,
    /// then releases the EGL lock. Returns
    /// [`SkiaAdapterError::DirectContextBuildFailed`] when either the
    /// interface or the DirectContext can't be created.
    pub fn new(inner: Arc<OpenGlSurfaceAdapter>) -> Result<Self, SkiaAdapterError> {
        let egl = Arc::clone(inner.runtime());
        let _current = egl.lock_make_current().map_err(|e| {
            SkiaAdapterError::DirectContextBuildFailed {
                reason: format!("lock_make_current: {e}"),
            }
        })?;
        // Build a Skia GL `Interface` with proc resolution routed
        // through `EglRuntime::get_proc_address` (i.e.
        // `eglGetProcAddress` under the hood). The closure is borrowed
        // for the duration of `new_load_with`; Skia copies any
        // resolved fn pointers into its own internal proc table during
        // interface construction, so the closure (and its captured
        // `&EglRuntime`) does not need to outlive this call.
        let interface = skia_safe::gpu::gl::Interface::new_load_with(|sym| {
            egl.get_proc_address(sym)
        })
        .ok_or_else(|| SkiaAdapterError::DirectContextBuildFailed {
            reason: "skia_safe::gpu::gl::Interface::new_load_with returned None".into(),
        })?;
        let direct_context = direct_contexts::make_gl(interface, None).ok_or_else(|| {
            SkiaAdapterError::DirectContextBuildFailed {
                reason: "skia_safe::gpu::direct_contexts::make_gl returned None".into(),
            }
        })?;
        drop(_current);
        Ok(Self {
            inner,
            egl,
            direct_context: Arc::new(Mutex::new(SyncDirectContext(direct_context))),
        })
    }

    /// Inner OpenGL adapter — power-user accessor for callers that
    /// need to issue raw GL alongside Skia (debug tooling, custom
    /// rendering passes).
    pub fn inner(&self) -> &Arc<OpenGlSurfaceAdapter> {
        &self.inner
    }

    fn wrap_write<'g>(
        &'g self,
        inner_guard: WriteGuard<'g, OpenGlSurfaceAdapter>,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let surface_id = inner_guard.surface_id();
        let texture_id = inner_guard.view().gl_texture_id();
        let color_type = surface_format_to_color_type(surface.format).ok_or_else(|| {
            AdapterError::UnsupportedFormat {
                surface_id,
                reason: format!(
                    "SurfaceFormat {:?} not supported by Skia GL backend",
                    surface.format
                ),
            }
        })?;

        let _current = self.egl.lock_make_current().map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("lock_make_current (acquire_write): {e}"),
            }
        })?;

        let backend_texture = build_backend_texture(
            texture_id,
            surface.width as i32,
            surface.height as i32,
            surface.format,
        );

        let mut ctx_guard = self.direct_context.lock().map_err(|_| {
            AdapterError::BackendRejected {
                reason: "Skia DirectContext mutex poisoned".into(),
            }
        })?;
        let skia_surface = surfaces::wrap_backend_texture(
            &mut ctx_guard.0,
            &backend_texture,
            SurfaceOrigin::TopLeft,
            None,
            color_type,
            None::<ColorSpace>,
            None,
        )
        .ok_or_else(|| AdapterError::BackendRejected {
            reason: "skia_safe::gpu::surfaces::wrap_backend_texture returned None".into(),
        })?;
        drop(ctx_guard);
        drop(_current);

        let view = SkiaGlWriteView::new(
            skia_surface,
            backend_texture,
            inner_guard,
            Arc::clone(&self.direct_context),
            Arc::clone(&self.egl),
        );
        Ok(WriteGuard::new(self, surface_id, view))
    }

    fn wrap_read<'g>(
        &'g self,
        inner_guard: ReadGuard<'g, OpenGlSurfaceAdapter>,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let surface_id = inner_guard.surface_id();
        let texture_id = inner_guard.view().gl_texture_id();
        let color_type = surface_format_to_color_type(surface.format).ok_or_else(|| {
            AdapterError::UnsupportedFormat {
                surface_id,
                reason: format!(
                    "SurfaceFormat {:?} not supported by Skia GL backend",
                    surface.format
                ),
            }
        })?;

        let _current = self.egl.lock_make_current().map_err(|e| {
            AdapterError::BackendRejected {
                reason: format!("lock_make_current (acquire_read): {e}"),
            }
        })?;

        let backend_texture = build_backend_texture(
            texture_id,
            surface.width as i32,
            surface.height as i32,
            surface.format,
        );

        let mut ctx_guard = self.direct_context.lock().map_err(|_| {
            AdapterError::BackendRejected {
                reason: "Skia DirectContext mutex poisoned".into(),
            }
        })?;
        let image = skia_safe::gpu::images::borrow_texture_from(
            &mut ctx_guard.0,
            &backend_texture,
            SurfaceOrigin::TopLeft,
            color_type,
            AlphaType::Opaque,
            None::<ColorSpace>,
        )
        .ok_or_else(|| AdapterError::BackendRejected {
            reason: "skia_safe::gpu::images::borrow_texture_from returned None".into(),
        })?;
        drop(ctx_guard);
        drop(_current);

        let view = SkiaGlReadView::new(
            image,
            inner_guard,
            Arc::clone(&self.direct_context),
            Arc::clone(&self.egl),
        );
        Ok(ReadGuard::new(self, surface_id, view))
    }
}

/// Build a Skia [`gpu::BackendTexture`] from the GL texture id imported
/// by [`OpenGlSurfaceAdapter`].
///
/// `format` (the Skia GL `Format` enum) is the GL *internal* format —
/// what the driver thinks the texture's storage looks like. The
/// EGLImage import binds the underlying DMA-BUF as `GL_RGBA8` for both
/// `Bgra8` and `Rgba8` `SurfaceFormat`s; we tell Skia `RGBA8` and let
/// the [`ColorType`] (chosen by [`surface_format_to_color_type`])
/// disambiguate the in-memory channel order.
fn build_backend_texture(
    texture_id: u32,
    width: i32,
    height: i32,
    _format: SurfaceFormat,
) -> gpu::BackendTexture {
    let info = gpu_gl::TextureInfo {
        target: GL_TEXTURE_2D,
        id: texture_id,
        format: gpu_gl::Format::RGBA8.into(),
        protected: Protected::No,
    };
    // SAFETY: `make_gl` is `unsafe` because Skia trusts the caller's
    // claim that the GL texture id is live and bound to a real
    // `GL_TEXTURE_2D`. The `OpenGlSurfaceAdapter` registry maintains
    // that invariant for the lifetime of the inner `WriteGuard` /
    // `ReadGuard` we hold downstream.
    unsafe { backend_textures::make_gl((width, height), Mipmapped::No, info, "streamlib-skia-gl") }
}

/// Map [`SurfaceFormat`] → Skia [`ColorType`]. Both BGRA8 and RGBA8
/// surfaces ride a `GL_RGBA8` storage format internally; the
/// [`ColorType`] is what tells Skia how to interpret the bytes.
fn surface_format_to_color_type(format: SurfaceFormat) -> Option<ColorType> {
    match format {
        SurfaceFormat::Bgra8 => Some(ColorType::BGRA8888),
        SurfaceFormat::Rgba8 => Some(ColorType::RGBA8888),
        SurfaceFormat::Nv12 => None,
    }
}

impl SurfaceAdapter for SkiaGlSurfaceAdapter {
    type ReadView<'g> = SkiaGlReadView<'g>;
    type WriteView<'g> = SkiaGlWriteView<'g>;

    fn acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'g, Self>, AdapterError> {
        let inner_guard = self.inner.acquire_read(surface)?;
        self.wrap_read(inner_guard, surface)
    }

    fn acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'g, Self>, AdapterError> {
        let inner_guard = self.inner.acquire_write(surface)?;
        self.wrap_write(inner_guard, surface)
    }

    fn try_acquire_read<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'g, Self>>, AdapterError> {
        match self.inner.try_acquire_read(surface)? {
            Some(g) => self.wrap_read(g, surface).map(Some),
            None => Ok(None),
        }
    }

    fn try_acquire_write<'g>(
        &'g self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'g, Self>>, AdapterError> {
        match self.inner.try_acquire_write(surface)? {
            Some(g) => self.wrap_write(g, surface).map(Some),
            None => Ok(None),
        }
    }

    fn end_read_access(&self, _surface_id: SurfaceId) {
        // No-op — see end_write_access.
    }

    fn end_write_access(&self, _surface_id: SurfaceId) {
        // No-op. The view's drop hook is the one place that flushes
        // Skia + releases the EGL lock + drops the inner guard
        // (which fires inner.end_write_access → glFinish). Doing
        // anything here would be a use-after-release.
    }
}
