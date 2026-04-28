// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VulkanContext<D>` — the customer-facing one-stop API.
//!
//! Customers call:
//!
//! ```ignore
//! let ctx = streamlib_adapter_vulkan::VulkanContext::new(adapter);
//! {
//!     let mut guard = ctx.acquire_write(&surface)?;
//!     // guard.view_mut() is a VulkanWriteView with a VkImage handle.
//! }
//! ```
//!
//! The context is a thin convenience over
//! [`crate::VulkanSurfaceAdapter`]; every operation maps to a
//! [`streamlib_adapter_abi::SurfaceAdapter`] method. Generic over the
//! device flavor `D: VulkanRhiDevice` so it works against either
//! `HostVulkanDevice` (host-side) or `ConsumerVulkanDevice` (cdylib).

use std::sync::Arc;

use streamlib_consumer_rhi::VulkanRhiDevice;
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, WriteGuard,
};

use crate::adapter::VulkanSurfaceAdapter;
use crate::raw_handles::{raw_handles, RawVulkanHandles};

// Power-user `raw_handles()` is generic over `VulkanRhiDevice` —
// works against either host- or consumer-flavored devices. The
// streamlib RHI takes the per-queue mutex internally; raw users
// assume the responsibility.

/// Customer-facing handle bound to a single runtime, generic over the
/// device flavor.
///
/// Holds a shared reference to a [`VulkanSurfaceAdapter`] (typically
/// stored on the runtime itself); cheap to clone. Customers obtain one
/// via the runtime; tests construct one directly.
pub struct VulkanContext<D: VulkanRhiDevice + 'static> {
    adapter: Arc<VulkanSurfaceAdapter<D>>,
}

impl<D: VulkanRhiDevice + 'static> Clone for VulkanContext<D> {
    fn clone(&self) -> Self {
        Self {
            adapter: Arc::clone(&self.adapter),
        }
    }
}

impl<D: VulkanRhiDevice + 'static> VulkanContext<D> {
    pub fn new(adapter: Arc<VulkanSurfaceAdapter<D>>) -> Self {
        Self { adapter }
    }

    pub fn adapter(&self) -> &Arc<VulkanSurfaceAdapter<D>> {
        &self.adapter
    }

    /// Blocking read acquire. The guard's view returns a
    /// [`crate::VulkanReadView`] exposing the `VkImage` and the layout
    /// the adapter transitioned it to.
    pub fn acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'a, VulkanSurfaceAdapter<D>>, AdapterError> {
        self.adapter.acquire_read(surface)
    }

    /// Blocking write acquire.
    pub fn acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'a, VulkanSurfaceAdapter<D>>, AdapterError> {
        self.adapter.acquire_write(surface)
    }

    /// Non-blocking read acquire — `Ok(None)` on contention, never blocks.
    pub fn try_acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'a, VulkanSurfaceAdapter<D>>>, AdapterError> {
        self.adapter.try_acquire_read(surface)
    }

    /// Non-blocking write acquire.
    pub fn try_acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'a, VulkanSurfaceAdapter<D>>>, AdapterError> {
        self.adapter.try_acquire_write(surface)
    }
}

impl<D: VulkanRhiDevice + 'static> VulkanContext<D> {
    /// Power-user surface — raw Vulkan handles. Caller assumes queue
    /// mutex discipline and lifetime; the adapter does not track work
    /// they submit through this path.
    pub fn raw_handles(&self) -> RawVulkanHandles {
        raw_handles(self.adapter.device().as_ref())
    }
}
