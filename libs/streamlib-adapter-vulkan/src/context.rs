// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `VulkanContext` — the customer-facing one-stop API.
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
//! The context is a thin convenience over [`crate::VulkanSurfaceAdapter`];
//! every operation maps to a [`streamlib_adapter_abi::SurfaceAdapter`]
//! method. Provided here so the customer-facing API matches the
//! parallel polyglot wrappers (`streamlib.vulkan.context()` in Python,
//! `streamlib.vulkan.context()` in Deno) and so adapter authors have a
//! single import path: `streamlib_adapter_vulkan::{VulkanContext,
//! raw_handles}`.

use std::sync::Arc;

use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, WriteGuard,
};

use crate::adapter::VulkanSurfaceAdapter;
use crate::raw_handles::{raw_handles, RawVulkanHandles};

/// Customer-facing handle bound to a single host runtime.
///
/// Holds a shared reference to a [`VulkanSurfaceAdapter`] (typically
/// stored on the runtime itself); cheap to clone. Customers obtain one
/// via the runtime; tests construct one directly.
#[derive(Clone)]
pub struct VulkanContext {
    adapter: Arc<VulkanSurfaceAdapter>,
}

impl VulkanContext {
    pub fn new(adapter: Arc<VulkanSurfaceAdapter>) -> Self {
        Self { adapter }
    }

    pub fn adapter(&self) -> &Arc<VulkanSurfaceAdapter> {
        &self.adapter
    }

    /// Blocking read acquire. The guard's
    /// [`streamlib_adapter_abi::WriteGuard::view`] / `view_mut` returns a
    /// [`crate::VulkanReadView`] exposing the host's `VkImage` and the
    /// layout the adapter transitioned it to.
    pub fn acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'a, VulkanSurfaceAdapter>, AdapterError> {
        self.adapter.acquire_read(surface)
    }

    /// Blocking write acquire.
    pub fn acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'a, VulkanSurfaceAdapter>, AdapterError> {
        self.adapter.acquire_write(surface)
    }

    /// Non-blocking read acquire — `Ok(None)` on contention, never blocks.
    pub fn try_acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'a, VulkanSurfaceAdapter>>, AdapterError> {
        self.adapter.try_acquire_read(surface)
    }

    /// Non-blocking write acquire.
    pub fn try_acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'a, VulkanSurfaceAdapter>>, AdapterError> {
        self.adapter.try_acquire_write(surface)
    }

    /// Power-user surface — raw Vulkan handles (`VkInstance`, `VkDevice`,
    /// `VkQueue`, …). The customer assumes responsibility for queue
    /// mutex discipline and lifetime; the adapter does not track work
    /// they submit through this path.
    pub fn raw_handles(&self) -> RawVulkanHandles {
        raw_handles(self.adapter.device())
    }
}
