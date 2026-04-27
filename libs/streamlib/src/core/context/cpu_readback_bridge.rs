// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side dispatch trait the escalate handler uses to drive
//! cpu-readback acquires on behalf of subprocess customers.
//!
//! The trait lives here (in `streamlib`) because the escalate IPC
//! handler is here. Implementations live in adapter crates (or
//! application code that wraps an adapter) — those can depend on
//! `streamlib`, the reverse cannot. Register an implementation via
//! [`crate::core::context::GpuContext::set_cpu_readback_bridge`] before
//! spawning subprocesses that issue `acquire_cpu_readback`.

#![cfg(target_os = "linux")]

use std::sync::Arc;

use streamlib_adapter_abi::{SurfaceFormat, SurfaceId};

use crate::adapter_support::VulkanPixelBuffer;

/// Wire-format access mode mirrored from the escalate schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CpuReadbackAccessMode {
    Read,
    Write,
}

/// Per-plane staging buffer the bridge hands back on a successful
/// acquire. The escalate handler check-ins each `staging` buffer with
/// the surface-share service and surfaces the registered surface IDs to
/// the subprocess.
pub struct CpuReadbackPlane {
    pub staging: Arc<VulkanPixelBuffer>,
    pub width: u32,
    pub height: u32,
    pub bytes_per_pixel: u32,
}

/// Result of [`CpuReadbackBridge::acquire`]. Owns the underlying adapter
/// guard (so its `Drop` runs the CPU→GPU flush on write release + signals
/// the timeline) and exposes the per-plane staging buffers the escalate
/// handler will publish via surface-share.
pub struct CpuReadbackAcquired {
    pub width: u32,
    pub height: u32,
    pub format: SurfaceFormat,
    pub planes: Vec<CpuReadbackPlane>,
    /// Type-erased guard kept alive until the bridge is asked to release
    /// the matching `bridge_handle`. Dropping it triggers the adapter's
    /// release-side work.
    pub guard: Box<dyn Send + Sync>,
}

/// Dispatch trait the host runtime uses to talk to a cpu-readback
/// adapter without taking a build-time dependency on it.
///
/// Implementations are registered on [`crate::core::context::GpuContext`]
/// via [`crate::core::context::GpuContext::set_cpu_readback_bridge`]; the
/// escalate handler reaches them from inside `escalate(|full| ...)` so
/// the bridge call always runs with `FullAccess` capability.
pub trait CpuReadbackBridge: Send + Sync {
    /// Acquire scoped read or write access to a host-registered surface.
    /// The returned [`CpuReadbackAcquired::guard`] keeps the underlying
    /// adapter guard alive until the bridge sees a matching `release`.
    fn acquire(
        &self,
        surface_id: SurfaceId,
        mode: CpuReadbackAccessMode,
    ) -> Result<CpuReadbackAcquired, String>;

    /// Non-blocking acquire. Returns `Ok(None)` if the surface is already
    /// write-held (or, for `Write` mode, read-held); returns `Ok(Some(_))`
    /// on success with the same shape as [`Self::acquire`]. Errors map
    /// onto adapter failures (surface not registered, GPU submit failure)
    /// — *not* contention.
    fn try_acquire(
        &self,
        surface_id: SurfaceId,
        mode: CpuReadbackAccessMode,
    ) -> Result<Option<CpuReadbackAcquired>, String>;
}
