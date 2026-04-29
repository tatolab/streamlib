// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `SkiaGlContext` — customer-facing one-stop API for the GL-backed
//! Skia adapter.
//!
//! ```ignore
//! let gl_ctx = streamlib_adapter_opengl::OpenGlContext::new(adapter);
//! let skia_gl_ctx = streamlib_adapter_skia::SkiaGlContext::new(&gl_ctx)?;
//! {
//!     let mut guard = skia_gl_ctx.acquire_write(&surface)?;
//!     let canvas = guard.view_mut().surface_mut().canvas();
//!     canvas.clear(skia_safe::Color::BLUE);
//! } // guard drops — Skia flush + glFinish via the inner OpenGl
//!   // adapter's release path.
//! ```
//!
//! Mirror-shape of [`crate::SkiaContext`]. Customers obtain one via
//! the runtime's setup hook; tests construct one directly.

use std::sync::Arc;

use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, WriteGuard,
};
use streamlib_adapter_opengl::OpenGlContext;

use crate::error::SkiaAdapterError;
use crate::gl_adapter::SkiaGlSurfaceAdapter;

/// Customer-facing handle for the GL-backed Skia adapter.
#[derive(Clone)]
pub struct SkiaGlContext {
    adapter: Arc<SkiaGlSurfaceAdapter>,
}

impl SkiaGlContext {
    /// Build a Skia GL context on top of the given OpenGL context.
    pub fn new(opengl_ctx: &OpenGlContext) -> Result<Self, SkiaAdapterError> {
        let adapter = SkiaGlSurfaceAdapter::new(Arc::clone(opengl_ctx.adapter()))?;
        Ok(Self {
            adapter: Arc::new(adapter),
        })
    }

    /// Construct a [`SkiaGlContext`] directly from a pre-built adapter.
    /// Most callers want [`Self::new`]; this exists for tests that
    /// share an adapter `Arc` across multiple context handles.
    pub fn from_adapter(adapter: Arc<SkiaGlSurfaceAdapter>) -> Self {
        Self { adapter }
    }

    pub fn adapter(&self) -> &Arc<SkiaGlSurfaceAdapter> {
        &self.adapter
    }

    pub fn acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'a, SkiaGlSurfaceAdapter>, AdapterError> {
        self.adapter.acquire_read(surface)
    }

    pub fn acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'a, SkiaGlSurfaceAdapter>, AdapterError> {
        self.adapter.acquire_write(surface)
    }

    pub fn try_acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'a, SkiaGlSurfaceAdapter>>, AdapterError> {
        self.adapter.try_acquire_read(surface)
    }

    pub fn try_acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'a, SkiaGlSurfaceAdapter>>, AdapterError> {
        self.adapter.try_acquire_write(surface)
    }
}
