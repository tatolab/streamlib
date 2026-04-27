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
        // Snapshot plane geometry up-front so the response can describe
        // the staging buffers without re-locking the adapter after the
        // acquire (which would otherwise have to expose more internals).
        let snapshot = self
            .adapter
            .snapshot_plane_geometry(surface_id)
            .ok_or_else(|| {
                format!(
                    "cpu-readback bridge: surface_id {surface_id} not registered with adapter"
                )
            })?;

        // SAFETY: Extending the guard's lifetime to `'static`. The
        // guard is owned by `BridgeGuard` boxed below, which itself is
        // owned by the escalate handler's
        // `EscalateHandleRegistry::CpuReadback` slot for the duration
        // of the matching `release_handle`. The adapter is held by
        // `self.adapter: Arc<CpuReadbackSurfaceAdapter>`; the guards
        // contain a back-reference to the adapter via
        // `ReadGuard` / `WriteGuard`, but those references are valid
        // as long as the bridge (and thus the Arc) lives. The host
        // runtime drops the registry before dropping the bridge, so
        // the ordering invariant holds.
        let adapter_static: &'static CpuReadbackSurfaceAdapter =
            unsafe { std::mem::transmute(self.adapter.as_ref()) };

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

        Ok(CpuReadbackAcquired {
            width: snapshot.width,
            height: snapshot.height,
            format: snapshot.format,
            planes,
            guard: Box::new(BridgeGuardKeepalive {
                _guard: guard,
                _adapter_keepalive: Arc::clone(&self.adapter),
            }),
        })
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
