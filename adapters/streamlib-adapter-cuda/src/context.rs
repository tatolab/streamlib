// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `CudaContext<D>` — the customer-facing one-stop API.
//!
//! Customers call:
//!
//! ```ignore
//! let ctx = streamlib_adapter_cuda::CudaContext::new(adapter);
//! {
//!     let mut guard = ctx.acquire_write(&surface)?;
//!     // guard.view_mut() is a CudaWriteView with a vk::Buffer handle.
//! }
//! ```
//!
//! The context is a thin convenience over
//! [`crate::CudaSurfaceAdapter`]; every operation maps to a
//! [`streamlib_adapter_abi::SurfaceAdapter`] method. Generic over the
//! device flavor `D: VulkanRhiDevice` so it works against either
//! `HostVulkanDevice` (host-side, today) or `ConsumerVulkanDevice`
//! (cdylib, once #589/#590 wire it up).

use std::sync::Arc;

use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, WriteGuard,
};
use streamlib_consumer_rhi::VulkanRhiDevice;

use crate::adapter::CudaSurfaceAdapter;
use crate::view::{CudaSurfaceGuard, CudaTextureGuard};

/// Customer-facing handle bound to a single runtime, generic over the
/// device flavor. Holds a shared reference to a [`CudaSurfaceAdapter`]
/// (typically stored on the runtime itself); cheap to clone.
pub struct CudaContext<D: VulkanRhiDevice + 'static> {
    adapter: Arc<CudaSurfaceAdapter<D>>,
}

impl<D: VulkanRhiDevice + 'static> Clone for CudaContext<D> {
    fn clone(&self) -> Self {
        Self {
            adapter: Arc::clone(&self.adapter),
        }
    }
}

impl<D: VulkanRhiDevice + 'static> CudaContext<D> {
    pub fn new(adapter: Arc<CudaSurfaceAdapter<D>>) -> Self {
        Self { adapter }
    }

    pub fn adapter(&self) -> &Arc<CudaSurfaceAdapter<D>> {
        &self.adapter
    }

    /// Blocking read acquire.
    pub fn acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'a, CudaSurfaceAdapter<D>>, AdapterError> {
        self.adapter.acquire_read(surface)
    }

    /// Blocking write acquire.
    pub fn acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'a, CudaSurfaceAdapter<D>>, AdapterError> {
        self.adapter.acquire_write(surface)
    }

    /// Non-blocking read acquire — `Ok(None)` on contention.
    pub fn try_acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'a, CudaSurfaceAdapter<D>>>, AdapterError> {
        self.adapter.try_acquire_read(surface)
    }

    /// Non-blocking write acquire.
    pub fn try_acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'a, CudaSurfaceAdapter<D>>>, AdapterError> {
        self.adapter.try_acquire_write(surface)
    }

    /// Blocking acquire of read-only image access — produces a
    /// [`CudaTextureGuard`] whose view carries the `vk::Image` the
    /// cdylib uses to construct a `cudaTextureObject_t`.
    pub fn acquire_texture<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<CudaTextureGuard<'a, D>, AdapterError> {
        self.adapter.acquire_texture(surface)
    }

    /// Non-blocking variant of [`Self::acquire_texture`].
    pub fn try_acquire_texture<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<CudaTextureGuard<'a, D>>, AdapterError> {
        self.adapter.try_acquire_texture(surface)
    }

    /// Blocking acquire of read-write image access — produces a
    /// [`CudaSurfaceGuard`] whose view carries the `vk::Image` the
    /// cdylib uses to construct a `cudaSurfaceObject_t`.
    pub fn acquire_surface<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<CudaSurfaceGuard<'a, D>, AdapterError> {
        self.adapter.acquire_surface(surface)
    }

    /// Non-blocking variant of [`Self::acquire_surface`].
    pub fn try_acquire_surface<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<CudaSurfaceGuard<'a, D>>, AdapterError> {
        self.adapter.try_acquire_surface(surface)
    }
}
