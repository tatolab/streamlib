// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `GpuContextLimitedAccessVTable` + `GpuContextFullAccessVTable`
//! callbacks + static vtables + accessors.
//!
//! The LimitedAccess vtable is the cdylib-facing surface for sandboxed
//! GPU work; the FullAccess vtable is reached only inside
//! `escalate(|full| ...)` scopes via the LimitedAccess vtable's
//! `escalate_begin` callback. Every body deref's the opaque `handle`
//! pointer back to a host-owned Rust type (`Arc<GpuContext>` for
//! Limited; a `ScopeToken` for Full).

use std::ffi::c_void;
#[cfg(test)]
use std::sync::Arc;

use streamlib_plugin_abi::{
    GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION,
    GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION, GpuContextFullAccessVTable,
    GpuContextLimitedAccessVTable,
};

use super::host_callbacks;
use super::run_host_extern_c;

mod full;
mod limited;
mod scope_token;
mod shared;

use full::{
    host_gpu_full_acquire_output_texture, host_gpu_full_acquire_render_target_dma_buf_image,
    host_gpu_full_build_tlas, host_gpu_full_build_triangles_blas, host_gpu_full_check_in_surface,
    host_gpu_full_color_converter, host_gpu_full_create_command_recorder,
    host_gpu_full_create_timeline_semaphore, host_gpu_full_drop_handle,
    host_gpu_full_gpu_capabilities, host_gpu_full_host_vulkan_device_arc,
    host_gpu_full_host_vulkan_texture_arc, host_gpu_full_import_dma_buf_storage_buffer,
    host_gpu_full_supports_ray_tracing_pipeline, host_gpu_full_upload_pixel_buffer_as_texture,
    host_gpu_full_wait_device_idle,
};
use full::{
    host_gpu_full_clone_acceleration_structure, host_gpu_full_clone_color_converter,
    host_gpu_full_clone_command_recorder, host_gpu_full_clone_compute_kernel,
    host_gpu_full_clone_graphics_kernel, host_gpu_full_clone_ray_tracing_kernel,
    host_gpu_full_clone_texture_ring, host_gpu_full_create_compute_kernel,
    host_gpu_full_create_graphics_kernel, host_gpu_full_create_ray_tracing_kernel,
    host_gpu_full_create_texture_ring, host_gpu_full_drop_acceleration_structure,
    host_gpu_full_drop_color_converter, host_gpu_full_drop_command_recorder,
    host_gpu_full_drop_compute_kernel, host_gpu_full_drop_graphics_kernel,
    host_gpu_full_drop_ray_tracing_kernel, host_gpu_full_drop_texture_ring,
};
use limited::{
    host_gpu_lim_acquire_index_buffer, host_gpu_lim_acquire_pixel_buffer,
    host_gpu_lim_acquire_storage_buffer, host_gpu_lim_acquire_texture,
    host_gpu_lim_acquire_uniform_buffer, host_gpu_lim_acquire_vertex_buffer,
    host_gpu_lim_blit_copy, host_gpu_lim_blit_copy_iosurface, host_gpu_lim_check_out_surface,
    host_gpu_lim_clear_video_source_timeline_semaphore, host_gpu_lim_clone_index_buffer,
    host_gpu_lim_clone_pixel_buffer, host_gpu_lim_clone_rhi_command_queue,
    host_gpu_lim_clone_storage_buffer, host_gpu_lim_clone_texture,
    host_gpu_lim_clone_texture_registration, host_gpu_lim_clone_uniform_buffer,
    host_gpu_lim_clone_vertex_buffer, host_gpu_lim_command_queue,
    host_gpu_lim_commit_and_wait_command_buffer, host_gpu_lim_commit_command_buffer,
    host_gpu_lim_copy_pixel_buffer_to_texture, host_gpu_lim_copy_texture_command_buffer,
    host_gpu_lim_create_command_buffer, host_gpu_lim_create_command_buffer_from_queue,
    host_gpu_lim_drop_command_buffer, host_gpu_lim_drop_index_buffer,
    host_gpu_lim_drop_pixel_buffer, host_gpu_lim_drop_pooled_texture_handle,
    host_gpu_lim_drop_rhi_command_queue, host_gpu_lim_drop_storage_buffer,
    host_gpu_lim_drop_texture, host_gpu_lim_drop_texture_registration,
    host_gpu_lim_drop_uniform_buffer, host_gpu_lim_drop_vertex_buffer, host_gpu_lim_escalate_begin,
    host_gpu_lim_escalate_end, host_gpu_lim_get_pixel_buffer,
    host_gpu_lim_host_video_source_timeline_arc, host_gpu_lim_plane_base_address_pixel_buffer,
    host_gpu_lim_plane_size_pixel_buffer, host_gpu_lim_register_texture,
    host_gpu_lim_resolve_pixel_buffer_by_surface_id, host_gpu_lim_resolve_texture_by_surface_id,
    host_gpu_lim_resolve_texture_registration_by_surface_id,
    host_gpu_lim_set_video_source_timeline_semaphore, host_gpu_lim_strong_count_pixel_buffer,
    host_gpu_lim_surface_store, host_gpu_lim_texture_native_dma_buf_fd,
    host_gpu_lim_texture_registration_current_layout, host_gpu_lim_texture_registration_texture,
    host_gpu_lim_texture_registration_update_layout, host_gpu_lim_unregister_texture,
    host_gpu_lim_update_texture_registration_layout, host_gpu_lim_wait_timeline_semaphore,
};

// pointers and reading nothing about layout.

// ---------------- GpuContextLimitedAccess vtable ----------------
//
// Host-side implementations of every callback on the
// [`GpuContextLimitedAccessVTable`]. The static at the bottom of
// this block (`HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE`) wires them
// up; the cdylib-side mirror lives in the cdylib's statically-
// linked engine copy and reads through the host-installed pointer
// on [`HostServices::gpu_context_limited_access_vtable`].

unsafe extern "C" fn host_gpu_lim_clone_handle(borrowed_handle: *const c_void) -> *const c_void {
    run_host_extern_c(
        "host_gpu_lim_clone_handle",
        || {
            if borrowed_handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `borrowed_handle` was produced by
            // `GpuContextLimitedAccess::new` (or a prior
            // `clone_handle`) as
            // `Box::into_raw(Box::new(Arc::new(GpuContext)))`.
            // Reading through `&*` and cloning the Arc bumps the
            // underlying refcount; we re-leak via
            // `Box::into_raw(Box::new(...))` so the caller gets a
            // fresh owned handle that matches `drop_handle`'s
            // expected shape.
            let original = unsafe {
                &*(borrowed_handle as *const std::sync::Arc<crate::core::context::GpuContext>)
            };
            Box::into_raw(Box::new(original.clone())) as *const c_void
        },
        std::ptr::null(),
    )
}

unsafe extern "C" fn host_gpu_lim_drop_handle(owned_handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: paired with `GpuContextLimitedAccess::new` and
            // `host_gpu_lim_clone_handle` — both produce
            // `Box::into_raw(Box::new(Arc<GpuContext>))`. Reclaiming
            // via `Box::from_raw` drops the Arc, which decrements
            // the host's `Arc<GpuContext>` refcount and frees the
            // underlying `GpuContext` when the count reaches zero.
            unsafe {
                let _ = Box::from_raw(
                    owned_handle as *mut std::sync::Arc<crate::core::context::GpuContext>,
                );
            }
        },
        (),
    )
}

/// Static [`GpuContextLimitedAccessVTable`] installed once per process.
/// Paired with the per-RuntimeContext gpu-limited handle returned by
/// [`HOST_RUNTIME_CONTEXT_VTABLE`]`::gpu_limited_access`.
// =============================================================================
// HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE — Phase C2
// =============================================================================
//
// FullAccess vtable bodies. Reached from cdylib code via the
// vtable-dispatched path of `GpuContextLimitedAccess::escalate`; the
// `gpu_handle` slot on every method is an opaque scope token issued
// by the LimitedAccess vtable's `escalate_begin` callback (Phase C3).
// Each body resolves the token to its bound `Arc<GpuContext>` via
// `with_full_scope_or_err`; missing tokens return
// `Error::InvalidEscalateScope`. The engine-internal in-process path
// constructs `GpuContextFullAccess` via `Self::new(GpuContext)` and
// reaches the same engine methods through `host_inner` rather than
// the vtable, so these callback bodies don't ever see an
// engine-internal `Box<Arc<GpuContext>>`-shaped handle.
//
// Kernel return handles: `*const VulkanComputeKernel` / etc., shaped
// as `Arc::into_raw(arc)`. Cdylib's `clone_*` / `drop_*` callbacks
// route refcount accounting through host-compiled code.

pub static HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE: GpuContextFullAccessVTable =
    GpuContextFullAccessVTable {
        layout_version: GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        drop_handle: host_gpu_full_drop_handle,
        clone_compute_kernel: host_gpu_full_clone_compute_kernel,
        drop_compute_kernel: host_gpu_full_drop_compute_kernel,
        clone_graphics_kernel: host_gpu_full_clone_graphics_kernel,
        drop_graphics_kernel: host_gpu_full_drop_graphics_kernel,
        clone_ray_tracing_kernel: host_gpu_full_clone_ray_tracing_kernel,
        drop_ray_tracing_kernel: host_gpu_full_drop_ray_tracing_kernel,
        clone_texture_ring: host_gpu_full_clone_texture_ring,
        drop_texture_ring: host_gpu_full_drop_texture_ring,
        // v4 PluginAbiObject lifecycle slots (#917).
        clone_color_converter: host_gpu_full_clone_color_converter,
        drop_color_converter: host_gpu_full_drop_color_converter,
        clone_acceleration_structure: host_gpu_full_clone_acceleration_structure,
        drop_acceleration_structure: host_gpu_full_drop_acceleration_structure,
        clone_command_recorder: host_gpu_full_clone_command_recorder,
        drop_command_recorder: host_gpu_full_drop_command_recorder,
        create_compute_kernel: host_gpu_full_create_compute_kernel,
        create_graphics_kernel: host_gpu_full_create_graphics_kernel,
        create_ray_tracing_kernel: host_gpu_full_create_ray_tracing_kernel,
        create_texture_ring: host_gpu_full_create_texture_ring,
        acquire_render_target_dma_buf_image: host_gpu_full_acquire_render_target_dma_buf_image,
        // Phase D (#906) entries.
        wait_device_idle: host_gpu_full_wait_device_idle,
        acquire_output_texture: host_gpu_full_acquire_output_texture,
        upload_pixel_buffer_as_texture: host_gpu_full_upload_pixel_buffer_as_texture,
        color_converter: host_gpu_full_color_converter,
        create_command_recorder: host_gpu_full_create_command_recorder,
        build_triangles_blas: host_gpu_full_build_triangles_blas,
        build_tlas: host_gpu_full_build_tlas,
        supports_ray_tracing_pipeline: host_gpu_full_supports_ray_tracing_pipeline,
        check_in_surface: host_gpu_full_check_in_surface,
        gpu_capabilities: host_gpu_full_gpu_capabilities,
        create_timeline_semaphore: host_gpu_full_create_timeline_semaphore,
        import_dma_buf_storage_buffer: host_gpu_full_import_dma_buf_storage_buffer,
        host_vulkan_device_arc: host_gpu_full_host_vulkan_device_arc,
        host_vulkan_texture_arc: host_gpu_full_host_vulkan_texture_arc,
    };

/// Pointer to the [`GpuContextFullAccessVTable`] this plugin should
/// dispatch through. Same plugin-routing rule as
/// [`host_gpu_context_limited_access_vtable`]: host mode resolves to
/// the local `&HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE` static, cdylib
/// mode resolves to the host-installed pointer cached on
/// [`HostServices::gpu_context_full_access_vtable`].
pub fn host_gpu_context_full_access_vtable() -> *const GpuContextFullAccessVTable {
    match host_callbacks() {
        Some(c) if !c.gpu_context_full_access_vtable.is_null() => c.gpu_context_full_access_vtable,
        _ => &HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE,
    }
}

pub static HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE: GpuContextLimitedAccessVTable =
    GpuContextLimitedAccessVTable {
        layout_version: GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        clone_handle: host_gpu_lim_clone_handle,
        drop_handle: host_gpu_lim_drop_handle,
        clone_pixel_buffer: host_gpu_lim_clone_pixel_buffer,
        drop_pixel_buffer: host_gpu_lim_drop_pixel_buffer,
        strong_count_pixel_buffer: host_gpu_lim_strong_count_pixel_buffer,
        plane_base_address_pixel_buffer: host_gpu_lim_plane_base_address_pixel_buffer,
        plane_size_pixel_buffer: host_gpu_lim_plane_size_pixel_buffer,
        clone_texture: host_gpu_lim_clone_texture,
        drop_texture: host_gpu_lim_drop_texture,
        drop_pooled_texture_handle: host_gpu_lim_drop_pooled_texture_handle,
        register_texture: host_gpu_lim_register_texture,
        update_texture_registration_layout: host_gpu_lim_update_texture_registration_layout,
        acquire_texture: host_gpu_lim_acquire_texture,
        resolve_texture_by_surface_id: host_gpu_lim_resolve_texture_by_surface_id,
        unregister_texture: host_gpu_lim_unregister_texture,
        clone_storage_buffer: host_gpu_lim_clone_storage_buffer,
        drop_storage_buffer: host_gpu_lim_drop_storage_buffer,
        clone_uniform_buffer: host_gpu_lim_clone_uniform_buffer,
        drop_uniform_buffer: host_gpu_lim_drop_uniform_buffer,
        clone_vertex_buffer: host_gpu_lim_clone_vertex_buffer,
        drop_vertex_buffer: host_gpu_lim_drop_vertex_buffer,
        clone_index_buffer: host_gpu_lim_clone_index_buffer,
        drop_index_buffer: host_gpu_lim_drop_index_buffer,
        acquire_storage_buffer: host_gpu_lim_acquire_storage_buffer,
        acquire_uniform_buffer: host_gpu_lim_acquire_uniform_buffer,
        acquire_vertex_buffer: host_gpu_lim_acquire_vertex_buffer,
        acquire_index_buffer: host_gpu_lim_acquire_index_buffer,
        clone_texture_registration: host_gpu_lim_clone_texture_registration,
        drop_texture_registration: host_gpu_lim_drop_texture_registration,
        texture_registration_texture: host_gpu_lim_texture_registration_texture,
        texture_registration_current_layout: host_gpu_lim_texture_registration_current_layout,
        texture_registration_update_layout: host_gpu_lim_texture_registration_update_layout,
        resolve_texture_registration_by_surface_id:
            host_gpu_lim_resolve_texture_registration_by_surface_id,
        clone_rhi_command_queue: host_gpu_lim_clone_rhi_command_queue,
        drop_rhi_command_queue: host_gpu_lim_drop_rhi_command_queue,
        create_command_buffer_from_queue: host_gpu_lim_create_command_buffer_from_queue,
        drop_command_buffer: host_gpu_lim_drop_command_buffer,
        commit_command_buffer: host_gpu_lim_commit_command_buffer,
        commit_and_wait_command_buffer: host_gpu_lim_commit_and_wait_command_buffer,
        copy_texture_command_buffer: host_gpu_lim_copy_texture_command_buffer,
        command_queue: host_gpu_lim_command_queue,
        create_command_buffer: host_gpu_lim_create_command_buffer,
        copy_pixel_buffer_to_texture: host_gpu_lim_copy_pixel_buffer_to_texture,
        blit_copy: host_gpu_lim_blit_copy,
        blit_copy_iosurface: host_gpu_lim_blit_copy_iosurface,
        surface_store: host_gpu_lim_surface_store,
        check_out_surface: host_gpu_lim_check_out_surface,
        acquire_pixel_buffer: host_gpu_lim_acquire_pixel_buffer,
        get_pixel_buffer: host_gpu_lim_get_pixel_buffer,
        resolve_pixel_buffer_by_surface_id: host_gpu_lim_resolve_pixel_buffer_by_surface_id,
        escalate_begin: host_gpu_lim_escalate_begin,
        escalate_end: host_gpu_lim_escalate_end,
        texture_native_dma_buf_fd: host_gpu_lim_texture_native_dma_buf_fd,
        set_video_source_timeline_semaphore: host_gpu_lim_set_video_source_timeline_semaphore,
        clear_video_source_timeline_semaphore: host_gpu_lim_clear_video_source_timeline_semaphore,
        wait_timeline_semaphore: host_gpu_lim_wait_timeline_semaphore,
        host_video_source_timeline_arc: host_gpu_lim_host_video_source_timeline_arc,
    };

/// Pointer to the [`GpuContextLimitedAccessVTable`] this plugin should
/// dispatch through. Same plugin-routing rule as
/// [`host_runtime_context_vtable`].
pub fn host_gpu_context_limited_access_vtable() -> *const GpuContextLimitedAccessVTable {
    match host_callbacks() {
        Some(c) if !c.gpu_context_limited_access_vtable.is_null() => {
            c.gpu_context_limited_access_vtable
        }
        _ => &HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE,
    }
}
#[cfg(test)]
mod gpu_lim_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for every callback on
    //! [`GpuContextLimitedAccessVTable`].
    //!
    //! Each test passes a null `handle` (and where applicable a null
    //! out-param or invalid input) and asserts the documented contract:
    //!
    //! - Lifecycle callbacks (clone/drop, Arc refcount bumps, etc.)
    //!   short-circuit on null and do not crash.
    //! - Probe callbacks (`strong_count_pixel_buffer`,
    //!   `plane_*_pixel_buffer`, `texture_registration_current_layout`,
    //!   etc.) return their documented default value.
    //! - Result-returning callbacks (`acquire_*`, `resolve_*`,
    //!   `command_queue`, `create_command_buffer*`, `blit_copy*`, ...)
    //!   return rc=1 with a callback-prefixed UTF-8 error in `err_buf`
    //!   and leave their out-slot unwritten.
    //! - `surface_store` writes a null-handle PluginAbiObject (the "None"
    //!   sentinel) regardless of input.
    //!
    //! `escalate_begin` / `escalate_end` are covered by
    //! [`gpu_lim_escalate_vtable_tests`]; `texture_native_dma_buf_fd`
    //! by [`gpu_lim_texture_native_dma_buf_fd_tests`].
    //!
    //! The vtable's `layout_version` field is locked against
    //! `GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION` so a
    //! cdylib-side ABI bump can't drift from the host's wiring.
    //!
    //! Tests that build a real `GpuContext` via `make_host_handle`
    //! carry `#[serial]` for the same NVIDIA dual-`VkDevice` reason
    //! as the escalate-vtable suite
    //! (`docs/learnings/nvidia-dual-vulkan-device-crash.md`).

    use super::*;
    use serial_test::serial;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    // ------------------------------------------------------------------
    // Layout-version match
    // ------------------------------------------------------------------

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.layout_version,
            streamlib_plugin_abi::GPU_CONTEXT_LIMITED_ACCESS_VTABLE_LAYOUT_VERSION,
        );
    }

    // ------------------------------------------------------------------
    // Lifecycle callbacks — null is a documented no-op
    // ------------------------------------------------------------------

    /// Generates a `null_handle_no_crash` test for a single-argument
    /// lifecycle callback (clone/drop) that takes `handle: *const c_void`
    /// and returns `()` — null is documented as a no-op.
    macro_rules! null_handle_no_crash_test {
        ($test_name:ident, $field:ident) => {
            #[test]
            fn $test_name() {
                unsafe {
                    (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(std::ptr::null());
                }
            }
        };
    }

    null_handle_no_crash_test!(drop_handle_handles_null, drop_handle);
    null_handle_no_crash_test!(clone_pixel_buffer_handles_null, clone_pixel_buffer);
    null_handle_no_crash_test!(drop_pixel_buffer_handles_null, drop_pixel_buffer);
    null_handle_no_crash_test!(clone_texture_handles_null, clone_texture);
    null_handle_no_crash_test!(drop_texture_handles_null, drop_texture);
    null_handle_no_crash_test!(
        drop_pooled_texture_handle_handles_null,
        drop_pooled_texture_handle
    );
    null_handle_no_crash_test!(clone_storage_buffer_handles_null, clone_storage_buffer);
    null_handle_no_crash_test!(drop_storage_buffer_handles_null, drop_storage_buffer);
    null_handle_no_crash_test!(clone_uniform_buffer_handles_null, clone_uniform_buffer);
    null_handle_no_crash_test!(drop_uniform_buffer_handles_null, drop_uniform_buffer);
    null_handle_no_crash_test!(clone_vertex_buffer_handles_null, clone_vertex_buffer);
    null_handle_no_crash_test!(drop_vertex_buffer_handles_null, drop_vertex_buffer);
    null_handle_no_crash_test!(clone_index_buffer_handles_null, clone_index_buffer);
    null_handle_no_crash_test!(drop_index_buffer_handles_null, drop_index_buffer);
    null_handle_no_crash_test!(
        clone_texture_registration_handles_null,
        clone_texture_registration
    );
    null_handle_no_crash_test!(
        drop_texture_registration_handles_null,
        drop_texture_registration
    );
    null_handle_no_crash_test!(
        clone_rhi_command_queue_handles_null,
        clone_rhi_command_queue
    );
    null_handle_no_crash_test!(drop_rhi_command_queue_handles_null, drop_rhi_command_queue);
    null_handle_no_crash_test!(drop_command_buffer_handles_null, drop_command_buffer);
    null_handle_no_crash_test!(commit_command_buffer_handles_null, commit_command_buffer);
    null_handle_no_crash_test!(
        commit_and_wait_command_buffer_handles_null,
        commit_and_wait_command_buffer
    );

    // ------------------------------------------------------------------
    // Probe callbacks — null returns the documented sentinel
    // ------------------------------------------------------------------

    #[test]
    fn clone_handle_returns_null_on_null_input() {
        let out =
            unsafe { (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.clone_handle)(std::ptr::null()) };
        assert!(out.is_null());
    }

    #[test]
    fn strong_count_pixel_buffer_returns_zero_on_null() {
        let n = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.strong_count_pixel_buffer)(std::ptr::null())
        };
        assert_eq!(n, 0);
    }

    #[test]
    fn plane_base_address_pixel_buffer_returns_null_on_null_handle() {
        let p = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.plane_base_address_pixel_buffer)(
                std::ptr::null(),
                0,
            )
        };
        assert!(p.is_null());
    }

    #[test]
    fn plane_size_pixel_buffer_returns_zero_on_null_handle() {
        let n = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.plane_size_pixel_buffer)(std::ptr::null(), 0)
        };
        assert_eq!(n, 0);
    }

    #[test]
    fn texture_registration_texture_returns_null_on_null_handle() {
        let p = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.texture_registration_texture)(std::ptr::null())
        };
        assert!(p.is_null());
    }

    #[test]
    fn texture_registration_current_layout_returns_zero_on_null_handle() {
        let v = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.texture_registration_current_layout)(
                std::ptr::null(),
            )
        };
        assert_eq!(v, 0, "VK_IMAGE_LAYOUT_UNDEFINED == 0");
    }

    #[test]
    fn texture_registration_update_layout_handles_null_no_crash() {
        // Two-arg shape (handle, layout_raw); null handle short-circuits
        // before the atomic store. The macro above is single-arg only,
        // so this gets its own test.
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.texture_registration_update_layout)(
                std::ptr::null(),
                42,
            );
        }
    }

    // ------------------------------------------------------------------
    // Update / register callbacks (no err_buf, no return) — null gpu
    // handle is a documented no-op
    // ------------------------------------------------------------------

    #[test]
    fn register_texture_handles_null_gpu_no_crash() {
        let id = b"abc";
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.register_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                std::ptr::null(),
                0,
            );
        }
    }

    #[test]
    fn update_texture_registration_layout_handles_null_gpu_no_crash() {
        let id = b"abc";
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.update_texture_registration_layout)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                42,
            );
        }
    }

    #[test]
    fn unregister_texture_handles_null_gpu_no_crash() {
        let id = b"abc";
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.unregister_texture)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
            );
        }
    }

    #[test]
    fn copy_texture_command_buffer_handles_null_no_crash() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_texture_command_buffer)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            );
        }
    }

    // ------------------------------------------------------------------
    // surface_store — always writes a defined PluginAbiObject; null gpu_handle
    // yields the "None" sentinel (null handle + null vtable)
    // ------------------------------------------------------------------

    #[test]
    fn surface_store_writes_null_plugin_abi_object_on_null_gpu_handle() {
        // SAFETY: SurfaceStore is `#[repr(C)] (handle, vtable)`; the
        // callback always writes through the out-pointer first, so a
        // zero-init landing slot is safe to read after the call.
        let mut out: crate::core::context::SurfaceStore = unsafe { std::mem::zeroed() };
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.surface_store)(
                std::ptr::null(),
                &mut out as *mut _ as *mut c_void,
            );
        }
        assert!(
            out.is_none(),
            "null gpu_handle must produce a None PluginAbiObject"
        );
    }

    #[test]
    fn surface_store_handles_null_out_param_no_crash() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.surface_store)(
                std::ptr::null(),
                std::ptr::null_mut(),
            );
        }
    }

    // ------------------------------------------------------------------
    // Result-returning callbacks (rc=1, err_buf populated)
    // ------------------------------------------------------------------

    /// Generates a null-gpu-handle test for a callback whose signature
    /// is `(gpu_handle, out, err_buf, err_buf_cap, err_len) -> i32` —
    /// the most common shape. `err_marker` is a substring expected in
    /// the err_buf message.
    macro_rules! null_gpu_handle_err_test {
        ($test_name:ident, $field:ident, $err_marker:expr) => {
            #[test]
            fn $test_name() {
                let (mut buf, mut len) = make_err_buf();
                let mut out_storage = [0u8; 256];
                let rc = unsafe {
                    (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                        std::ptr::null(),
                        out_storage.as_mut_ptr() as *mut c_void,
                        buf.as_mut_ptr(),
                        buf.len(),
                        &mut len,
                    )
                };
                assert_eq!(rc, 1);
                let msg = err_buf_as_str(&buf, len);
                assert!(msg.contains($err_marker), "got: {msg}");
            }
        };
    }

    null_gpu_handle_err_test!(
        command_queue_returns_error_on_null_gpu_handle,
        command_queue,
        "command_queue: null gpu handle"
    );

    null_gpu_handle_err_test!(
        create_command_buffer_returns_error_on_null_gpu_handle,
        create_command_buffer,
        "create_command_buffer: null gpu handle"
    );

    #[test]
    #[serial]
    fn command_queue_returns_error_on_null_out_param() {
        // null gpu_handle path runs first; need a non-null synthetic
        // handle to reach the null-out-param branch. Build a host-mode
        // handle if available; otherwise skip — this test is purely
        // about the wrapper's null-out-param guard, which on a null
        // gpu_handle is unreachable.
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.command_queue)(
                handle,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("command_queue: null out_queue"), "got: {msg}");
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn create_command_buffer_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.create_command_buffer)(
                handle,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_command_buffer: null out_cb"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- acquire_texture ---

    #[test]
    fn acquire_texture_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_texture)(
                std::ptr::null(),
                64,
                64,
                0,
                0,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_texture: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn acquire_texture_returns_error_on_null_out_pooled_handle() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_texture)(
                handle,
                64,
                64,
                0,
                0,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_texture: null out_pooled_handle"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn acquire_texture_returns_error_on_invalid_format_raw() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_texture)(
                handle,
                64,
                64,
                99, // invalid format_raw
                0,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_texture: invalid format_raw"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- resolve_texture_by_surface_id ---

    #[test]
    fn resolve_texture_by_surface_id_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_by_surface_id)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_by_surface_id: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn resolve_texture_by_surface_id_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_by_surface_id: null out_texture"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn resolve_texture_by_surface_id_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        // 0xFF, 0xFF, 0xFF is invalid UTF-8.
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_by_surface_id: surface_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- resolve_texture_registration_by_surface_id ---

    #[test]
    fn resolve_texture_registration_by_surface_id_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_registration_by_surface_id)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_registration_by_surface_id: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn resolve_texture_registration_by_surface_id_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_registration_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_registration_by_surface_id: null out_registration"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn resolve_texture_registration_by_surface_id_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_texture_registration_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                0,
                0,
                64,
                64,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_texture_registration_by_surface_id: surface_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- acquire_{storage,uniform,vertex,index}_buffer ---
    // Linux: null gpu handle / null out_buffer → rc=1 + per-slot msg.
    // Non-Linux: always rc=1 + "not available on this platform".

    #[cfg(target_os = "linux")]
    mod buffer_acquire_linux {
        use super::*;

        macro_rules! buffer_acquire_null_gpu_test {
            ($test_name:ident, $field:ident, $err_marker:expr) => {
                #[test]
                fn $test_name() {
                    let (mut buf, mut len) = make_err_buf();
                    let mut out_storage = [0u8; 256];
                    let rc = unsafe {
                        (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                            std::ptr::null(),
                            1024,
                            out_storage.as_mut_ptr() as *mut c_void,
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut len,
                        )
                    };
                    assert_eq!(rc, 1);
                    let msg = err_buf_as_str(&buf, len);
                    assert!(msg.contains($err_marker), "got: {msg}");
                }
            };
        }

        buffer_acquire_null_gpu_test!(
            acquire_storage_buffer_returns_error_on_null_gpu_handle,
            acquire_storage_buffer,
            "acquire_storage_buffer: null gpu handle"
        );
        buffer_acquire_null_gpu_test!(
            acquire_uniform_buffer_returns_error_on_null_gpu_handle,
            acquire_uniform_buffer,
            "acquire_uniform_buffer: null gpu handle"
        );
        buffer_acquire_null_gpu_test!(
            acquire_vertex_buffer_returns_error_on_null_gpu_handle,
            acquire_vertex_buffer,
            "acquire_vertex_buffer: null gpu handle"
        );
        buffer_acquire_null_gpu_test!(
            acquire_index_buffer_returns_error_on_null_gpu_handle,
            acquire_index_buffer,
            "acquire_index_buffer: null gpu handle"
        );

        macro_rules! buffer_acquire_null_out_test {
            ($test_name:ident, $field:ident, $err_marker:expr) => {
                #[test]
                #[serial]
                fn $test_name() {
                    let Some((handle, _arc)) = make_host_handle() else {
                        return;
                    };
                    let (mut buf, mut len) = make_err_buf();
                    let rc = unsafe {
                        (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                            handle,
                            1024,
                            std::ptr::null_mut(),
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut len,
                        )
                    };
                    assert_eq!(rc, 1);
                    let msg = err_buf_as_str(&buf, len);
                    assert!(msg.contains($err_marker), "got: {msg}");
                    unsafe { free_host_handle(handle) };
                }
            };
        }

        buffer_acquire_null_out_test!(
            acquire_storage_buffer_returns_error_on_null_out_buffer,
            acquire_storage_buffer,
            "acquire_storage_buffer: null out_buffer"
        );
        buffer_acquire_null_out_test!(
            acquire_uniform_buffer_returns_error_on_null_out_buffer,
            acquire_uniform_buffer,
            "acquire_uniform_buffer: null out_buffer"
        );
        buffer_acquire_null_out_test!(
            acquire_vertex_buffer_returns_error_on_null_out_buffer,
            acquire_vertex_buffer,
            "acquire_vertex_buffer: null out_buffer"
        );
        buffer_acquire_null_out_test!(
            acquire_index_buffer_returns_error_on_null_out_buffer,
            acquire_index_buffer,
            "acquire_index_buffer: null out_buffer"
        );
    }

    #[cfg(not(target_os = "linux"))]
    mod buffer_acquire_non_linux {
        use super::*;

        macro_rules! buffer_acquire_not_available_test {
            ($test_name:ident, $field:ident, $err_marker:expr) => {
                #[test]
                fn $test_name() {
                    let (mut buf, mut len) = make_err_buf();
                    let mut out_storage = [0u8; 256];
                    let rc = unsafe {
                        (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.$field)(
                            std::ptr::null(),
                            1024,
                            out_storage.as_mut_ptr() as *mut c_void,
                            buf.as_mut_ptr(),
                            buf.len(),
                            &mut len,
                        )
                    };
                    assert_eq!(rc, 1);
                    let msg = err_buf_as_str(&buf, len);
                    assert!(msg.contains($err_marker), "got: {msg}");
                }
            };
        }

        buffer_acquire_not_available_test!(
            acquire_storage_buffer_reports_not_available,
            acquire_storage_buffer,
            "not available on this platform"
        );
        buffer_acquire_not_available_test!(
            acquire_uniform_buffer_reports_not_available,
            acquire_uniform_buffer,
            "not available on this platform"
        );
        buffer_acquire_not_available_test!(
            acquire_vertex_buffer_reports_not_available,
            acquire_vertex_buffer,
            "not available on this platform"
        );
        buffer_acquire_not_available_test!(
            acquire_index_buffer_reports_not_available,
            acquire_index_buffer,
            "not available on this platform"
        );
    }

    // --- create_command_buffer_from_queue ---

    #[test]
    fn create_command_buffer_from_queue_returns_error_on_null_queue_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.create_command_buffer_from_queue)(
                std::ptr::null(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("create_command_buffer_from_queue: null queue handle"),
            "got: {msg}"
        );
    }

    // --- copy_pixel_buffer_to_texture ---
    // Linux: tier-1 cover; non-Linux: stub returns "not available".

    #[cfg(target_os = "linux")]
    #[test]
    fn copy_pixel_buffer_to_texture_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_pixel_buffer_to_texture)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("copy_pixel_buffer_to_texture: null gpu handle"),
            "got: {msg}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn copy_pixel_buffer_to_texture_returns_error_on_null_pixel_buffer_or_texture() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_pixel_buffer_to_texture)(
                handle,
                std::ptr::null(),
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("copy_pixel_buffer_to_texture: null pixel_buffer or texture"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn copy_pixel_buffer_to_texture_reports_not_available_on_non_linux() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.copy_pixel_buffer_to_texture)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("copy_pixel_buffer_to_texture: not available on this platform"),
            "got: {msg}"
        );
    }

    // --- blit_copy ---

    #[test]
    fn blit_copy_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("blit_copy: null gpu handle"), "got: {msg}");
    }

    #[test]
    #[serial]
    fn blit_copy_returns_error_on_null_src_or_dst() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy)(
                handle,
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("blit_copy: null src or dst"), "got: {msg}");
        unsafe { free_host_handle(handle) };
    }

    // --- blit_copy_iosurface ---
    // macOS-only behaviour: null gpu / null dst → per-cause err.
    // Non-macOS: stub returns "not available on this platform (macOS-only)".

    #[cfg(target_os = "macos")]
    #[test]
    fn blit_copy_iosurface_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy_iosurface)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("blit_copy_iosurface: null gpu handle"),
            "got: {msg}"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn blit_copy_iosurface_reports_not_available_on_non_macos() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.blit_copy_iosurface)(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                64,
                64,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("blit_copy_iosurface: not available on this platform"),
            "got: {msg}"
        );
    }

    // --- check_out_surface ---

    #[test]
    fn check_out_surface_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.check_out_surface)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_out_surface: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn check_out_surface_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.check_out_surface)(
                handle,
                id.as_ptr(),
                id.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_out_surface: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn check_out_surface_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.check_out_surface)(
                handle,
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("check_out_surface: surface_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- acquire_pixel_buffer ---

    #[test]
    fn acquire_pixel_buffer_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_pixel_buffer)(
                std::ptr::null(),
                64,
                64,
                0x42475241, // valid Bgra32
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_pixel_buffer: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn acquire_pixel_buffer_returns_error_on_null_out_pixel_buffer() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_pixel_buffer)(
                handle,
                64,
                64,
                0x42475241,
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_pixel_buffer: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn acquire_pixel_buffer_returns_error_on_invalid_format_raw() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let mut id_buf = [0u8; 256];
        let mut id_len: usize = 0;
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.acquire_pixel_buffer)(
                handle,
                64,
                64,
                0xDEAD_BEEF, // invalid format_raw
                id_buf.as_mut_ptr(),
                id_buf.len(),
                &mut id_len,
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("acquire_pixel_buffer: invalid format_raw"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- get_pixel_buffer ---

    #[test]
    fn get_pixel_buffer_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let pool_id = b"pool-x";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.get_pixel_buffer)(
                std::ptr::null(),
                pool_id.as_ptr(),
                pool_id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("get_pixel_buffer: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn get_pixel_buffer_returns_error_on_null_out_pixel_buffer() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let pool_id = b"pool-x";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.get_pixel_buffer)(
                handle,
                pool_id.as_ptr(),
                pool_id.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("get_pixel_buffer: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn get_pixel_buffer_returns_error_on_invalid_utf8_pool_id() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let pool_id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.get_pixel_buffer)(
                handle,
                pool_id.as_ptr(),
                pool_id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("get_pixel_buffer: pool_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // --- resolve_pixel_buffer_by_surface_id ---

    #[test]
    fn resolve_pixel_buffer_by_surface_id_returns_error_on_null_gpu_handle() {
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_pixel_buffer_by_surface_id)(
                std::ptr::null(),
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_pixel_buffer_by_surface_id: null gpu handle"),
            "got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn resolve_pixel_buffer_by_surface_id_returns_error_on_null_out_param() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id = b"abc";
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_pixel_buffer_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_pixel_buffer_by_surface_id: null out_pixel_buffer"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    #[test]
    #[serial]
    fn resolve_pixel_buffer_by_surface_id_returns_error_on_invalid_utf8() {
        let Some((handle, _arc)) = make_host_handle() else {
            return;
        };
        let (mut buf, mut len) = make_err_buf();
        let id: [u8; 3] = [0xFF, 0xFF, 0xFF];
        let mut out_storage = [0u8; 256];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE.resolve_pixel_buffer_by_surface_id)(
                handle,
                id.as_ptr(),
                id.len(),
                out_storage.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(
            msg.contains("resolve_pixel_buffer_by_surface_id: surface_id not valid UTF-8"),
            "got: {msg}"
        );
        unsafe { free_host_handle(handle) };
    }

    // ------------------------------------------------------------------
    // Helpers — build a host-mode `gpu_handle` so the null-out / invalid-
    // input branches downstream of the null-handle guard can fire.
    //
    // Tests that take a real GpuContext are inherently unsafe in the
    // workspace lib suite when other tests construct VkDevices
    // concurrently (NVIDIA dual-VkDevice SIGSEGV per
    // `docs/learnings/nvidia-dual-vulkan-device-crash.md`). The
    // escalate-vtable tests use `#[serial]` for that reason. Tier-1
    // wire-format checks here either pass `null` (no GpuContext needed)
    // or build a fresh GpuContext per test — the latter case is
    // tolerated to be skipped via `init_for_platform` returning Err on
    // hosts without a GPU; subsequent calls then short-circuit the
    // test via early `return`. The host-handle-using tests do NOT race
    // because they never create a second VkDevice concurrently with the
    // serial escalate suite — the same `make_host_handle` shape used
    // there is reused here for symmetry.
    // ------------------------------------------------------------------

    fn make_host_handle() -> Option<(*const c_void, Arc<crate::core::context::GpuContext>)> {
        let gpu = crate::core::context::GpuContext::init_for_platform().ok()?;
        let arc = Arc::new(gpu);
        let boxed: Box<Arc<crate::core::context::GpuContext>> = Box::new(Arc::clone(&arc));
        let handle = Box::into_raw(boxed) as *const c_void;
        Some((handle, arc))
    }

    unsafe fn free_host_handle(handle: *const c_void) {
        let _ = unsafe { Box::from_raw(handle as *mut Arc<crate::core::context::GpuContext>) };
    }
}
