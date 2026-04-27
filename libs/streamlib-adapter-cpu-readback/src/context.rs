// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `CpuReadbackContext` — the customer-facing one-stop API.
//!
//! ```ignore
//! let ctx = streamlib_adapter_cpu_readback::CpuReadbackContext::new(adapter);
//! {
//!     let mut guard = ctx.acquire_write(&surface)?;
//!     // guard.view().bytes() / view_mut().bytes_mut() are the
//!     // tightly-packed BGRA bytes; mutate freely. On guard drop the
//!     // adapter flushes them back to the host VkImage.
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

    /// Blocking read acquire. The guard's view exposes the pixel bytes
    /// as `&[u8]` (tightly packed, `width * height * bytes_per_pixel`).
    /// The GPU→CPU copy is performed before this call returns; release
    /// is a no-op flush plus timeline signal.
    pub fn acquire_read<'a>(
        &'a self,
        surface: &StreamlibSurface,
    ) -> Result<ReadGuard<'a, CpuReadbackSurfaceAdapter>, AdapterError> {
        self.adapter.acquire_read(surface)
    }

    /// Blocking write acquire. The guard's view exposes the pixel bytes
    /// as `&mut [u8]`. On guard drop, the modified bytes are flushed
    /// back to the host `VkImage` via `vkCmdCopyBufferToImage` before
    /// the timeline release-value signals.
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
