// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side dispatch trait the escalate handler uses to drive
//! cpu-readback copies on behalf of subprocess customers.
//!
//! Post-Path-E (#562) this trait is a thin trigger interface: the
//! subprocess sends a `run_cpu_readback_copy` IPC, the host runs the
//! GPU copy on its queue, signals the surface's shared timeline at
//! end-of-submit, and replies with the timeline value. The subprocess
//! waits on its imported `ConsumerVulkanTimelineSemaphore` for that
//! value before reading or writing the staging buffer it imported
//! once at registration time.
//!
//! The bridge does **NOT** allocate or pass DMA-BUF FDs — the
//! staging buffers + timeline are pre-registered with the surface-
//! share service via `register_pixel_buffer_with_timeline` and
//! imported by the subprocess at startup. The bridge interface
//! carries only the surface_id, the copy direction, and the
//! resulting timeline value.
//!
//! The trait lives here (in `streamlib`) because the escalate IPC
//! handler is here. Implementations live in application setup glue
//! (or in `streamlib-adapter-cpu-readback` test utilities) — those
//! can depend on `streamlib`; the reverse cannot. Register an impl
//! via [`crate::core::context::GpuContext::set_cpu_readback_bridge`]
//! before spawning subprocesses that issue `run_cpu_readback_copy`.

#![cfg(target_os = "linux")]

use streamlib_adapter_abi::SurfaceId;

/// Which direction the host runs the GPU copy.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CpuReadbackCopyDirection {
    /// `vkCmdCopyImageToBuffer` — image → staging. Triggered by the
    /// subprocess at acquire time so it can read the freshly-copied
    /// staging bytes (or overwrite them, on write).
    ImageToBuffer,
    /// `vkCmdCopyBufferToImage` — staging → image. Triggered by the
    /// subprocess on write release to flush its edits back into the
    /// host's source image.
    BufferToImage,
}

/// Dispatch trait the host runtime uses to drive cpu-readback copies
/// for subprocess customers.
///
/// Post-#562 the surface-share registration carries staging buffers +
/// timeline once at registration time; per-acquire IPC reduces to
/// "run this copy, return the timeline value to wait on."
pub trait CpuReadbackBridge: Send + Sync {
    /// Run the requested copy direction on the host's queue, signal a
    /// new value on the surface's shared timeline at end-of-submit,
    /// and return that value.
    ///
    /// Errors map onto host adapter failures (surface not registered,
    /// GPU submit failure) — wire-encoded as
    /// [`crate::_generated_::com_streamlib_escalate_response::EscalateResponseErr`].
    fn run_copy(
        &self,
        surface_id: SurfaceId,
        direction: CpuReadbackCopyDirection,
    ) -> Result<u64, String>;

    /// Non-blocking variant. Returns `Ok(None)` if the host adapter's
    /// registry would have blocked instead of running the copy
    /// (`try_acquire_*` semantics). Errors flow through identically.
    fn try_run_copy(
        &self,
        surface_id: SurfaceId,
        direction: CpuReadbackCopyDirection,
    ) -> Result<Option<u64>, String>;
}
