// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `GpuContextFullAccessVTable` callbacks, split per
//! banner-bounded section of the original `gpu_context.rs` file. Each
//! submodule owns the wrappers for one concern; the parent's
//! `HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE` static wires the function
//! pointers up by name.
//!
//! `drop_handle` is a defensive no-op kept here at the FullAccess root
//! because `GpuContextFullAccess::Drop` dispatches on the struct's
//! `handle_kind` discriminator directly without routing through this
//! vtable slot — host-mode (Boxed) runs `Box::from_raw` in-process;
//! cdylib-mode (ScopeToken) releases the gate via the LimitedAccess
//! vtable's `escalate_end` callback. The slot is preserved at the same
//! vtable offset for layout-version stability.

use std::ffi::c_void;

use super::super::run_host_extern_c;

#[cfg(target_os = "linux")]
mod kernel_construction;
#[cfg(target_os = "linux")]
mod kernel_lifecycle;
mod methods;
mod render_target;

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) use kernel_construction::{
    host_gpu_full_create_compute_kernel, host_gpu_full_create_graphics_kernel,
    host_gpu_full_create_ray_tracing_kernel, host_gpu_full_create_texture_ring,
};
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) use kernel_lifecycle::{
    host_gpu_full_clone_acceleration_structure, host_gpu_full_clone_color_converter,
    host_gpu_full_clone_command_recorder, host_gpu_full_clone_compute_kernel,
    host_gpu_full_clone_graphics_kernel, host_gpu_full_clone_ray_tracing_kernel,
    host_gpu_full_clone_texture_ring, host_gpu_full_drop_acceleration_structure,
    host_gpu_full_drop_color_converter, host_gpu_full_drop_command_recorder,
    host_gpu_full_drop_compute_kernel, host_gpu_full_drop_graphics_kernel,
    host_gpu_full_drop_ray_tracing_kernel, host_gpu_full_drop_texture_ring,
};
pub(in crate::core::plugin::host_services) use methods::{
    host_gpu_full_acquire_output_texture, host_gpu_full_build_tlas,
    host_gpu_full_build_triangles_blas, host_gpu_full_check_in_surface,
    host_gpu_full_color_converter, host_gpu_full_create_command_recorder,
    host_gpu_full_create_timeline_semaphore, host_gpu_full_gpu_capabilities,
    host_gpu_full_host_vulkan_device_arc, host_gpu_full_host_vulkan_texture_arc,
    host_gpu_full_import_dma_buf_storage_buffer, host_gpu_full_supports_ray_tracing_pipeline,
    host_gpu_full_upload_pixel_buffer_as_texture, host_gpu_full_wait_device_idle,
};
pub(in crate::core::plugin::host_services) use render_target::host_gpu_full_acquire_render_target_dma_buf_image;

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_handle(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_handle",
        || {
            let _ = handle;
        },
        (),
    )
}
