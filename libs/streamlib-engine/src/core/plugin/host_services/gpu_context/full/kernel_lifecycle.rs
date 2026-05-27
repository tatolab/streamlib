// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextFullAccessVTable` kernel Arc-handle lifecycle (Linux-only).
//!
//! `clone_*` / `drop_*` pairs for each kernel β-shape the cdylib
//! carries: compute kernel, graphics kernel, ray-tracing kernel,
//! texture ring, color converter (v4 #917), acceleration structure
//! (v4 #917), command recorder (v5 #984). Each pair routes
//! `Arc::increment_strong_count` / `decrement_strong_count` through
//! host-compiled code so the cdylib never has to know the inner
//! β-shape's layout.
//!
//! On non-Linux hosts the kernel types don't exist; each callback ships a
//! `#[cfg(not(target_os = "linux"))]` defensive-no-op stub so the parent
//! `HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE` static resolves on every platform
//! (ABI-version stability — slot count + offsets unchanged).

use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use super::super::super::run_host_extern_c;

// ---------------- Kernel Arc-handle lifecycle (Linux-only) ----------------

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_compute_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_compute_kernel",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle is `Arc::into_raw(Arc<VulkanComputeKernel>)`-shaped.
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanComputeKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_compute_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_compute_kernel",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle is `Arc::into_raw(Arc<VulkanComputeKernel>)`-shaped.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanComputeKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_graphics_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_graphics_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanGraphicsKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_graphics_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_graphics_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanGraphicsKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_ray_tracing_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_ray_tracing_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanRayTracingKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_ray_tracing_kernel(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_ray_tracing_kernel",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::vulkan::rhi::VulkanRayTracingKernelInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_texture_ring(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_texture_ring",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::context::TextureRingInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_texture_ring(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_texture_ring",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::context::TextureRingInner,
                );
            }
        },
        (),
    )
}

// β-shape v4 (#917) lifecycle callbacks. The handle is
// `Arc::into_raw(Arc<<Type>Inner>)`-shaped on the host side; cdylib
// code never sees the Inner layout, only the opaque handle paired
// with its β-shape vtable. Increment/decrement runs in host-compiled
// code where the Inner layout is known statically.

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_color_converter(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_color_converter",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::rhi::RhiColorConverterInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_color_converter(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_color_converter",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::rhi::RhiColorConverterInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_acceleration_structure(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_clone_acceleration_structure",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::increment_strong_count(
                    handle
                        as *const crate::vulkan::rhi::VulkanAccelerationStructureInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_acceleration_structure(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_acceleration_structure",
        || {
            if handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(
                    handle
                        as *const crate::vulkan::rhi::VulkanAccelerationStructureInner,
                );
            }
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_command_recorder(_handle: *const c_void) {
    // RhiCommandRecorder is Box-shaped (single-owner) — deliberately
    // NOT Clone per CommandBuffer precedent. This slot is reserved
    // infrastructure; the type-level absence of `Clone` for
    // `RhiCommandRecorder` ensures the host callback is never invoked
    // from typesafe code. If reached, it's a bug somewhere.
    run_host_extern_c(
        "host_gpu_full_clone_command_recorder",
        || {
            tracing::error!(
                "host_gpu_full_clone_command_recorder invoked — RhiCommandRecorder is \
                 not Clone-able (Box-shaped, single-owner). This is a bug."
            );
        },
        (),
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_command_recorder(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_full_drop_command_recorder",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: handle is `Box::into_raw(Box<RhiCommandRecorderInner>)`-shaped.
            // Reconstruct the Box and let Drop run.
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut crate::vulkan::rhi::RhiCommandRecorderInner,
                );
            }
        },
        (),
    )
}

// Non-Linux stubs (callbacks must exist for the static layout, but
// the kernel types only ship on Linux).
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_compute_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_compute_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_graphics_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_graphics_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_ray_tracing_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_ray_tracing_kernel(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_texture_ring(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_texture_ring(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_color_converter(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_color_converter(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_acceleration_structure(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_acceleration_structure(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_clone_command_recorder(_handle: *const c_void) {}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_command_recorder(_handle: *const c_void) {}

