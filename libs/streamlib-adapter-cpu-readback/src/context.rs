// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `CpuReadbackContext<D>` — customer-facing one-stop API.
//!
//! Generic over the device flavor `D: VulkanRhiDevice` so the same
//! shape works in-process Rust (host-flavor adapter) and in a
//! subprocess cdylib (consumer-flavor adapter). Customers acquire
//! scoped read / write access via
//!
//! ```ignore
//! let mut guard = ctx.acquire_write(&surface)?;
//! let mut view = guard.view_mut();
//! view.plane_mut(0).bytes_mut(); // tightly-packed pixel bytes
//! ```
//!
//! On the host side the bytes are observable after `acquire_*`
//! returns (a `vkCmdCopyImageToBuffer` already ran via the in-process
//! trigger); on the consumer side an IPC trigger ran on the host and
//! a timeline wait observed the result before this call returned.
//! Multi-plane surfaces (NV12) report `plane_count() == 2`.

use std::sync::Arc;

use streamlib_consumer_rhi::VulkanRhiDevice;
use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, WriteGuard,
};

use crate::adapter::CpuReadbackSurfaceAdapter;

/// Customer-facing handle bound to a single runtime / cdylib.
pub struct CpuReadbackContext<D: VulkanRhiDevice + 'static> {
    adapter: Arc<CpuReadbackSurfaceAdapter<D>>,
}

impl<D: VulkanRhiDevice + 'static> Clone for CpuReadbackContext<D> {
    fn clone(&self) -> Self {
        Self {
            adapter: Arc::clone(&self.adapter),
        }
    }
}

impl<D: VulkanRhiDevice + 'static> CpuReadbackContext<D> {
    pub fn new(adapter: Arc<CpuReadbackSurfaceAdapter<D>>) -> Self {
        Self { adapter }
    }

    pub fn adapter(&self) -> &Arc<CpuReadbackSurfaceAdapter<D>> {
        &self.adapter
    }
}

#[cfg(target_os = "linux")]
impl<D: VulkanRhiDevice + 'static> CpuReadbackContext<D> {
    /// Blocking read acquire.
    pub fn acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'a, CpuReadbackSurfaceAdapter<D>>, AdapterError> {
        self.adapter.acquire_read(surface)
    }

    /// Blocking write acquire.
    pub fn acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'a, CpuReadbackSurfaceAdapter<D>>, AdapterError> {
        self.adapter.acquire_write(surface)
    }

    /// Non-blocking read acquire — `Ok(None)` on contention.
    pub fn try_acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'a, CpuReadbackSurfaceAdapter<D>>>, AdapterError> {
        self.adapter.try_acquire_read(surface)
    }

    /// Non-blocking write acquire.
    pub fn try_acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'a, CpuReadbackSurfaceAdapter<D>>>, AdapterError> {
        self.adapter.try_acquire_write(surface)
    }
}
