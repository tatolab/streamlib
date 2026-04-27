// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `OpenGlContext` — the customer-facing one-stop API.
//!
//! ```ignore
//! let runtime = streamlib_adapter_opengl::EglRuntime::new()?;
//! let adapter = std::sync::Arc::new(
//!     streamlib_adapter_opengl::OpenGlSurfaceAdapter::new(runtime),
//! );
//! let ctx = streamlib_adapter_opengl::OpenGlContext::new(adapter);
//! {
//!     let mut guard = ctx.acquire_write(&surface)?;
//!     let view = guard.view();
//!     // view.gl_texture_id() is a regular GL_TEXTURE_2D — bind it as a
//!     // sampler or attach to an FBO color attachment.
//! }
//! ```
//!
//! The context is a thin convenience over [`crate::OpenGlSurfaceAdapter`];
//! every operation maps to a [`streamlib_adapter_abi::SurfaceAdapter`]
//! method. Provided here so the customer-facing API matches the
//! parallel polyglot wrappers (`streamlib.opengl.context()` in Python,
//! `streamlib.opengl.context()` in Deno).

use std::sync::Arc;

use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, WriteGuard,
};

use crate::adapter::OpenGlSurfaceAdapter;

/// Customer-facing handle bound to a single subprocess GL stack.
///
/// Holds a shared reference to an [`OpenGlSurfaceAdapter`]; cheap to
/// clone. Customers obtain one via the runtime; tests construct one
/// directly.
#[derive(Clone)]
pub struct OpenGlContext {
    adapter: Arc<OpenGlSurfaceAdapter>,
}

impl OpenGlContext {
    pub fn new(adapter: Arc<OpenGlSurfaceAdapter>) -> Self {
        Self { adapter }
    }

    pub fn adapter(&self) -> &Arc<OpenGlSurfaceAdapter> {
        &self.adapter
    }

    /// Blocking read acquire. The guard's `view` returns an
    /// [`crate::OpenGlReadView`] exposing the bound `GL_TEXTURE_2D`
    /// id.
    pub fn acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'a, OpenGlSurfaceAdapter>, AdapterError> {
        self.adapter.acquire_read(surface)
    }

    /// Blocking write acquire.
    pub fn acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'a, OpenGlSurfaceAdapter>, AdapterError> {
        self.adapter.acquire_write(surface)
    }

    /// Non-blocking read acquire — `Ok(None)` on contention, never blocks.
    pub fn try_acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'a, OpenGlSurfaceAdapter>>, AdapterError> {
        self.adapter.try_acquire_read(surface)
    }

    /// Non-blocking write acquire.
    pub fn try_acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'a, OpenGlSurfaceAdapter>>, AdapterError> {
        self.adapter.try_acquire_write(surface)
    }
}
