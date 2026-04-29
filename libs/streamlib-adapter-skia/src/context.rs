// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `SkiaContext<D>` — customer-facing one-stop API.
//!
//! ```ignore
//! let vk_ctx = streamlib_adapter_vulkan::VulkanContext::new(adapter);
//! let skia_ctx = streamlib_adapter_skia::SkiaContext::new(&vk_ctx)?;
//! {
//!     let mut guard = skia_ctx.acquire_write(&surface)?;
//!     let canvas = guard.view_mut().surface_mut().canvas();
//!     canvas.clear(skia_safe::Color::BLUE);
//! } // guard drops — Skia flush + timeline signal happen here
//! ```
//!
//! The context is a thin convenience over [`crate::SkiaSurfaceAdapter`];
//! every operation maps to a [`streamlib_adapter_abi::SurfaceAdapter`]
//! method on the inner adapter. Generic over the device flavor `D:
//! VulkanRhiDevice` so it works against either `HostVulkanDevice` (host
//! side) or `ConsumerVulkanDevice` (cdylib).

use std::sync::Arc;

use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, WriteGuard,
};
use streamlib_adapter_vulkan::VulkanContext;
use streamlib_consumer_rhi::VulkanRhiDevice;

use crate::adapter::SkiaSurfaceAdapter;
use crate::error::SkiaAdapterError;

/// Customer-facing handle bound to a single runtime, generic over the
/// device flavor.
///
/// Holds a shared reference to a [`SkiaSurfaceAdapter`] (typically
/// stored on the runtime itself); cheap to clone. Customers obtain
/// one via the runtime's setup hook; tests construct one directly.
pub struct SkiaContext<D: VulkanRhiDevice + 'static> {
    adapter: Arc<SkiaSurfaceAdapter<D>>,
}

impl<D: VulkanRhiDevice + 'static> Clone for SkiaContext<D> {
    fn clone(&self) -> Self {
        Self {
            adapter: Arc::clone(&self.adapter),
        }
    }
}

impl<D: VulkanRhiDevice + 'static> SkiaContext<D> {
    /// Build a Skia context on top of the given Vulkan context.
    ///
    /// Constructs a single Skia `DirectContext` shared by all
    /// `acquire_*` calls. The customer never sees `GrVkBackendContext` —
    /// it's owned internally by the adapter.
    pub fn new(
        vulkan_ctx: &VulkanContext<D>,
    ) -> Result<Self, SkiaAdapterError> {
        let adapter = SkiaSurfaceAdapter::new(Arc::clone(vulkan_ctx.adapter()))?;
        Ok(Self {
            adapter: Arc::new(adapter),
        })
    }

    /// Construct a [`SkiaContext`] directly from a pre-built adapter.
    /// Most callers want [`Self::new`]; this exists for tests that
    /// share an adapter `Arc` across multiple context handles.
    pub fn from_adapter(adapter: Arc<SkiaSurfaceAdapter<D>>) -> Self {
        Self { adapter }
    }

    pub fn adapter(&self) -> &Arc<SkiaSurfaceAdapter<D>> {
        &self.adapter
    }

    /// Blocking read acquire. Guard's view returns a
    /// [`crate::SkiaReadView`] exposing the `skia::Image`.
    pub fn acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'a, SkiaSurfaceAdapter<D>>, AdapterError> {
        self.adapter.acquire_read(surface)
    }

    /// Blocking write acquire. Guard's view returns a
    /// [`crate::SkiaWriteView`] exposing the `skia::Surface`.
    pub fn acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'a, SkiaSurfaceAdapter<D>>, AdapterError> {
        self.adapter.acquire_write(surface)
    }

    /// Non-blocking read acquire — `Ok(None)` on contention.
    pub fn try_acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'a, SkiaSurfaceAdapter<D>>>, AdapterError> {
        self.adapter.try_acquire_read(surface)
    }

    /// Non-blocking write acquire — `Ok(None)` on contention.
    pub fn try_acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'a, SkiaSurfaceAdapter<D>>>, AdapterError> {
        self.adapter.try_acquire_write(surface)
    }
}
