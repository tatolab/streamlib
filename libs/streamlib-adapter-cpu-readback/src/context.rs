// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `CpuReadbackContext` — the customer-facing one-stop API.
//!
//! ```ignore
//! let ctx = streamlib_adapter_cpu_readback::CpuReadbackContext::new(adapter);
//! {
//!     let mut guard = ctx.acquire_write(&surface)?;
//!     // guard.view().plane(i).bytes() (read) /
//!     // guard.view_mut().plane_mut(i).bytes_mut() (write) are the
//!     // tightly-packed bytes for plane i. Single-plane formats
//!     // (BGRA8/RGBA8) report plane_count() == 1; multi-plane (NV12)
//!     // reports 2 (Y at 0, UV at 1). On guard drop the adapter
//!     // flushes every plane back to the host VkImage.
//! }
//! ```

use std::sync::Arc;

use streamlib_adapter_abi::{
    AdapterError, ReadGuard, StreamlibSurface, SurfaceAdapter, WriteGuard,
};

use crate::adapter::CpuReadbackSurfaceAdapter;

/// Customer-facing handle bound to a single host runtime.
#[derive(Clone)]
pub struct CpuReadbackContext {
    adapter: Arc<CpuReadbackSurfaceAdapter>,
}

impl CpuReadbackContext {
    pub fn new(adapter: Arc<CpuReadbackSurfaceAdapter>) -> Self {
        Self { adapter }
    }

    pub fn adapter(&self) -> &Arc<CpuReadbackSurfaceAdapter> {
        &self.adapter
    }

    /// Blocking read acquire. The guard's view exposes per-plane byte
    /// slices via `view.plane(i).bytes()` (tightly packed,
    /// `plane_width * plane_height * plane_bytes_per_pixel`). The
    /// GPU→CPU copy is performed before this call returns; release is a
    /// no-op flush plus timeline signal.
    pub fn acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'a, CpuReadbackSurfaceAdapter>, AdapterError> {
        self.adapter.acquire_read(surface)
    }

    /// Blocking write acquire. The guard's view exposes mutable per-
    /// plane byte slices via `view_mut().plane_mut(i).bytes_mut()`. On
    /// guard drop, every plane's modified bytes are flushed back to the
    /// host `VkImage` via per-plane `vkCmdCopyBufferToImage` before the
    /// timeline release-value signals.
    pub fn acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<WriteGuard<'a, CpuReadbackSurfaceAdapter>, AdapterError> {
        self.adapter.acquire_write(surface)
    }

    /// Non-blocking read acquire — `Ok(None)` on contention, never blocks.
    pub fn try_acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<ReadGuard<'a, CpuReadbackSurfaceAdapter>>, AdapterError> {
        self.adapter.try_acquire_read(surface)
    }

    /// Non-blocking write acquire.
    pub fn try_acquire_write<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<Option<WriteGuard<'a, CpuReadbackSurfaceAdapter>>, AdapterError> {
        self.adapter.try_acquire_write(surface)
    }
}
