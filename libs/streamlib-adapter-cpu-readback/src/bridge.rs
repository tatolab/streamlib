// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`streamlib::core::context::CpuReadbackBridge`] implementation
//! that drives this crate's [`CpuReadbackSurfaceAdapter`].
//!
//! Application code constructs one `CpuReadbackBridgeImpl` per
//! `Arc<CpuReadbackSurfaceAdapter>` and registers it on the runtime's
//! `GpuContext` via
//! [`streamlib::core::context::GpuContext::set_cpu_readback_bridge`].
//! After that, any subprocess customer that issues an
//! `acquire_cpu_readback` escalate request reaches this bridge, which
//! performs the host-side acquire and hands back per-plane staging
//! buffers for the escalate handler to publish via surface-share.

use std::sync::Arc;

use streamlib::core::context::{
    CpuReadbackAccessMode, CpuReadbackAcquired, CpuReadbackBridge, CpuReadbackPlane,
};
use streamlib_adapter_abi::{ReadGuard, SurfaceId, WriteGuard};

use crate::adapter::CpuReadbackSurfaceAdapter;

/// Either an active read or write guard from the cpu-readback adapter,
/// type-erased into the engine-side [`CpuReadbackBridge`] contract.
/// The variant fields are read by the guards' `Drop` impls only — the
/// adapter's release-side work (CPU→GPU flush on write + timeline
/// signal) runs there.
#[allow(dead_code)]
enum BridgeGuard {
    Read(ReadGuard<'static, CpuReadbackSurfaceAdapter>),
    Write(WriteGuard<'static, CpuReadbackSurfaceAdapter>),
}

// SAFETY: The guards' lifetime parameter is `'static` only because we
// extend the borrow via the `Arc<CpuReadbackSurfaceAdapter>` field
// `_adapter_keepalive` on the bridge — the adapter outlives every
// guard the bridge produces because guards are always dropped before
// the bridge itself, and the bridge holds the only reference path that
// could otherwise drop the adapter.
unsafe impl Send for BridgeGuard {}
unsafe impl Sync for BridgeGuard {}

/// `CpuReadbackBridge` impl backed by an in-process
/// [`CpuReadbackSurfaceAdapter`].
pub struct CpuReadbackBridgeImpl {
    adapter: Arc<CpuReadbackSurfaceAdapter>,
}

impl CpuReadbackBridgeImpl {
    pub fn new(adapter: Arc<CpuReadbackSurfaceAdapter>) -> Self {
        Self { adapter }
    }
}

impl CpuReadbackBridge for CpuReadbackBridgeImpl {
    fn acquire(
        &self,
        surface_id: SurfaceId,
        mode: CpuReadbackAccessMode,
    ) -> Result<CpuReadbackAcquired, String> {
        let snapshot = self.snapshot_or_err(surface_id)?;
        let adapter_static = self.adapter_static();
        let guard = match mode {
            CpuReadbackAccessMode::Read => {
                let g = adapter_static
                    .acquire_read_by_id(surface_id)
                    .map_err(|e| format!("cpu-readback adapter.acquire_read failed: {e}"))?;
                BridgeGuard::Read(g)
            }
            CpuReadbackAccessMode::Write => {
                let g = adapter_static
                    .acquire_write_by_id(surface_id)
                    .map_err(|e| format!("cpu-readback adapter.acquire_write failed: {e}"))?;
                BridgeGuard::Write(g)
            }
        };
        Ok(self.assemble_acquired(snapshot, guard))
    }

    fn try_acquire(
        &self,
        surface_id: SurfaceId,
        mode: CpuReadbackAccessMode,
    ) -> Result<Option<CpuReadbackAcquired>, String> {
        let snapshot = self.snapshot_or_err(surface_id)?;
        let adapter_static = self.adapter_static();
        let guard = match mode {
            CpuReadbackAccessMode::Read => {
                let result = adapter_static
                    .try_acquire_read_by_id(surface_id)
                    .map_err(|e| format!("cpu-readback adapter.try_acquire_read failed: {e}"))?;
                match result {
                    Some(g) => BridgeGuard::Read(g),
                    None => return Ok(None),
                }
            }
            CpuReadbackAccessMode::Write => {
                let result = adapter_static
                    .try_acquire_write_by_id(surface_id)
                    .map_err(|e| format!("cpu-readback adapter.try_acquire_write failed: {e}"))?;
                match result {
                    Some(g) => BridgeGuard::Write(g),
                    None => return Ok(None),
                }
            }
        };
        Ok(Some(self.assemble_acquired(snapshot, guard)))
    }
}

impl CpuReadbackBridgeImpl {
    fn snapshot_or_err(
        &self,
        surface_id: SurfaceId,
    ) -> Result<crate::adapter::CpuReadbackSurfaceSnapshot, String> {
        // Snapshot plane geometry up-front so the response can describe
        // the staging buffers without re-locking the adapter after the
        // acquire (which would otherwise have to expose more internals).
        self.adapter
            .snapshot_plane_geometry(surface_id)
            .ok_or_else(|| {
                format!(
                    "cpu-readback bridge: surface_id {surface_id} not registered with adapter"
                )
            })
    }

    /// SAFETY: extends the adapter borrow's lifetime to `'static`. The
    /// guards we produce live no longer than the
    /// `BridgeGuardKeepalive` that holds an `Arc` clone of the same
    /// adapter, so the back-reference in each guard stays valid across
    /// the lifetime of every emitted `CpuReadbackAcquired`. The host
    /// runtime drops the escalate registry (which owns the
    /// keepalives) before dropping the bridge, so ordering holds.
    fn adapter_static(&self) -> &'static CpuReadbackSurfaceAdapter {
        unsafe { std::mem::transmute(self.adapter.as_ref()) }
    }

    fn assemble_acquired(
        &self,
        snapshot: crate::adapter::CpuReadbackSurfaceSnapshot,
        guard: BridgeGuard,
    ) -> CpuReadbackAcquired {
        let planes = snapshot
            .planes
            .into_iter()
            .map(|p| CpuReadbackPlane {
                staging: p.staging,
                width: p.width,
                height: p.height,
                bytes_per_pixel: p.bytes_per_pixel,
            })
            .collect();

        CpuReadbackAcquired {
            width: snapshot.width,
            height: snapshot.height,
            format: snapshot.format,
            planes,
            guard: Box::new(BridgeGuardKeepalive {
                _guard: guard,
                _adapter_keepalive: Arc::clone(&self.adapter),
            }),
        }
    }
}

/// Keepalive bundle stored as the type-erased guard on
/// [`CpuReadbackAcquired::guard`]. Drop order matters: the inner
/// `BridgeGuard` (which holds a reference to the adapter) must drop
/// before the `Arc<CpuReadbackSurfaceAdapter>`. Rust's struct drop
/// order is field-declaration order, so `_guard` is dropped first,
/// then `_adapter_keepalive`.
struct BridgeGuardKeepalive {
    _guard: BridgeGuard,
    _adapter_keepalive: Arc<CpuReadbackSurfaceAdapter>,
}
