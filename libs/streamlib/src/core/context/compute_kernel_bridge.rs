// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side dispatch trait the escalate handler uses to drive compute
//! kernel registration and dispatch on behalf of subprocess customers.
//!
//! Mirrors the [`super::cpu_readback_bridge::CpuReadbackBridge`] shape
//! introduced in #562: the subprocess sends a typed IPC, the host runs
//! privileged Vulkan work via its [`crate::core::context::GpuContextFullAccess`],
//! and the bridge keeps the FullAccess capability boundary on the host
//! side of the IPC seam.
//!
//! Compute is register-once-dispatch-many: the subprocess sends the
//! SPIR-V blob once, the host reflects bindings + builds the
//! [`crate::vulkan::rhi::VulkanComputeKernel`] (with on-disk pipeline
//! cache persistence — see `STREAMLIB_PIPELINE_CACHE_DIR`), and caches
//! it keyed by SHA-256(spv). Subsequent `run` calls reference the
//! cached kernel by handle.
//!
//! The trait lives here (in `streamlib`) because the escalate IPC
//! handler is here. Implementations live in application setup glue
//! (or in `streamlib-adapter-vulkan` test utilities) — those can
//! depend on `streamlib`; the reverse cannot. Register an impl via
//! [`crate::core::context::GpuContext::set_compute_kernel_bridge`]
//! before spawning subprocesses that issue
//! `register_compute_kernel` / `run_compute_kernel`.

#![cfg(target_os = "linux")]

/// Dispatch trait the host runtime uses to drive compute kernel
/// registration and per-dispatch invocation for subprocess customers.
///
/// Compute dispatch on the host is synchronous (the kernel waits on
/// its own fence inside [`crate::vulkan::rhi::VulkanComputeKernel::dispatch`]),
/// so `run` returns when the GPU work has retired. No timeline value
/// is exchanged — by the time the subprocess receives the `ok`
/// response, it can advance its surface-share timeline and trust the
/// host's writes are visible.
pub trait ComputeKernelBridge: Send + Sync {
    /// Register a compute kernel. Returns a stable `kernel_id` keyed
    /// by SHA-256(spv) — re-registering identical SPIR-V hits the
    /// host-side cache and returns the same id without re-reflecting
    /// or rebuilding the pipeline.
    ///
    /// `push_constant_size` is validated against the shader's
    /// reflected push-constant range; a mismatch returns an error.
    fn register(
        &self,
        spv: &[u8],
        push_constant_size: u32,
    ) -> Result<String, String>;

    /// Dispatch a previously-registered kernel against the surface
    /// registered under `surface_uuid`.
    ///
    /// `surface_uuid` is the UUID string used for surface-share
    /// registration (`SurfaceStore::register_texture`); the bridge
    /// implementation maintains an application-provided UUID →
    /// host-side `StreamTexture` map so it can look up the `VkImage`
    /// to bind. Bound as a `storage_image` at slot 0 (the
    /// single-output convention enforced for v1).
    ///
    /// Pushes `push_constants`, dispatches `(group_count_x,
    /// group_count_y, group_count_z)`, and waits for the kernel's
    /// fence before returning. Errors include unrecognized
    /// `kernel_id`, surface lookup failure, and submit failure.
    fn run(
        &self,
        kernel_id: &str,
        surface_uuid: &str,
        push_constants: &[u8],
        group_count_x: u32,
        group_count_y: u32,
        group_count_z: u32,
    ) -> Result<(), String>;
}
