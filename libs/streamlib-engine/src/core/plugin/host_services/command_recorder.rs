// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `RhiCommandRecorderMethodsVTable` callbacks + static
//! vtable + accessor (Phase E sub-lift slice B — issue #984).
//!
//! Each wrapper reconstructs the recorder borrow from the raw
//! `Box::into_raw(Box<RhiCommandRecorderInner>)` handle the cdylib
//! passes, reconstructs the texture / buffer / kernel borrows via the
//! same `make_*_borrow` ManuallyDrop pattern the compute / graphics
//! kernel wrappers use, decodes the typed integer enum payloads
//! (`VulkanLayout` / `VulkanStage` / `VulkanAccess`), runs the inner
//! method, and converts the `Result<()>` into the FFI's `i32 +
//! err_buf` shape. All bodies wrapped in `run_host_extern_c` so a
//! panic in the inner method becomes a non-zero return.

use std::ffi::c_void;
use std::sync::Arc;

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::borrow::{
    make_compute_kernel_borrow, make_graphics_kernel_borrow, make_pixel_buffer_borrow,
    make_storage_buffer_borrow, make_texture_borrow,
};
use super::shared::wire::write_err;


// =============================================================================
// RhiCommandRecorderMethodsVTable wrappers (Phase E sub-lift slice B — #984).
// Each wrapper reconstructs the recorder borrow from the raw
// `Box::into_raw(Box<RhiCommandRecorderInner>)` handle the cdylib
// passes, reconstructs the texture / buffer / kernel borrows via the
// same `make_*_borrow` ManuallyDrop pattern the compute / graphics
// kernel wrappers use, decodes the typed integer enum payloads
// (`VulkanLayout` / `VulkanStage` / `VulkanAccess`), runs the inner
// method, and converts the `Result<()>` into the FFI's `i32 +
// err_buf` shape. All bodies are wrapped in `run_host_extern_c` so a
// panic in the inner method becomes a non-zero return.
// =============================================================================

/// SAFETY: caller must hand a `handle` that came from
/// `Box::into_raw(Box<RhiCommandRecorderInner>)` (the β-shape's
/// `handle` field). The host borrows mutably for the call's duration;
/// the cdylib retains ownership and the next `Drop` runs
/// `Box::from_raw + drop` via the parent vtable's
/// `drop_command_recorder` slot.
#[cfg(target_os = "linux")]
unsafe fn handle_as_command_recorder_mut(
    handle: *const c_void,
) -> Option<&'static mut crate::vulkan::rhi::RhiCommandRecorderInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe {
        &mut *(handle as *mut crate::vulkan::rhi::RhiCommandRecorderInner)
    })
}

/// Reconstruct a stack-allocated `VulkanComputeKernel` β-shape
/// borrow from an `Arc::into_raw(Arc<VulkanComputeKernelInner>)`
/// handle. Same ManuallyDrop contract as `make_storage_buffer_borrow`
/// / `make_texture_borrow` — the borrow's Drop must NOT run, or it
/// would decrement the kernel's Arc refcount through the vtable
/// while the cdylib still holds an outstanding plugin handle.
///
/// The cached POD fields are populated from the host-side
/// `VulkanComputeKernelInner` via the same two-step dance the other
/// `make_*_borrow` helpers use: build a minimal borrow with zeroed
/// fields, reach the inner through `host_inner()`, then construct
/// the final borrow with the cached fields filled. Mirrors the
/// contract `from_arc_into_raw` honors at construction.
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_begin(
    recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_begin",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "begin: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match recorder.begin() {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("begin: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_begin(
    _recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err("begin: Linux-only", err_buf, err_buf_cap, err_len);
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_image_barrier(
    recorder_handle: *const c_void,
    texture_handle: *const c_void,
    from_layout_raw: i32,
    to_layout_raw: i32,
    from_stage_raw: i64,
    to_stage_raw: i64,
    from_access_raw: i64,
    to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_image_barrier",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_image_barrier: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "record_image_barrier: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let texture_borrow = make_texture_borrow(texture_handle);
            let from_layout =
                streamlib_consumer_rhi::VulkanLayout(from_layout_raw);
            let to_layout = streamlib_consumer_rhi::VulkanLayout(to_layout_raw);
            let from_stage =
                crate::vulkan::rhi::VulkanStage(from_stage_raw as u64);
            let to_stage = crate::vulkan::rhi::VulkanStage(to_stage_raw as u64);
            let from_access =
                crate::vulkan::rhi::VulkanAccess(from_access_raw as u64);
            let to_access =
                crate::vulkan::rhi::VulkanAccess(to_access_raw as u64);
            match recorder.record_image_barrier(
                &*texture_borrow,
                from_layout,
                to_layout,
                from_stage,
                to_stage,
                from_access,
                to_access,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_image_barrier: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_image_barrier(
    _recorder_handle: *const c_void,
    _texture_handle: *const c_void,
    _from_layout_raw: i32,
    _to_layout_raw: i32,
    _from_stage_raw: i64,
    _to_stage_raw: i64,
    _from_access_raw: i64,
    _to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_image_barrier: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_buffer_barrier(
    recorder_handle: *const c_void,
    storage_buffer_handle: *const c_void,
    from_stage_raw: i64,
    to_stage_raw: i64,
    from_access_raw: i64,
    to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_buffer_barrier",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_buffer_barrier: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if storage_buffer_handle.is_null() {
                write_err(
                    "record_buffer_barrier: null storage_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let buffer_borrow =
                make_storage_buffer_borrow(storage_buffer_handle);
            let from_stage =
                crate::vulkan::rhi::VulkanStage(from_stage_raw as u64);
            let to_stage = crate::vulkan::rhi::VulkanStage(to_stage_raw as u64);
            let from_access =
                crate::vulkan::rhi::VulkanAccess(from_access_raw as u64);
            let to_access =
                crate::vulkan::rhi::VulkanAccess(to_access_raw as u64);
            match recorder.record_buffer_barrier(
                &*buffer_borrow,
                from_stage,
                to_stage,
                from_access,
                to_access,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_buffer_barrier: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_buffer_barrier(
    _recorder_handle: *const c_void,
    _storage_buffer_handle: *const c_void,
    _from_stage_raw: i64,
    _to_stage_raw: i64,
    _from_access_raw: i64,
    _to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_buffer_barrier: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_dispatch(
    recorder_handle: *const c_void,
    kernel_handle: *const c_void,
    group_x: u32,
    group_y: u32,
    group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_dispatch",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_dispatch: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if kernel_handle.is_null() {
                write_err(
                    "record_dispatch: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let kernel_borrow = make_compute_kernel_borrow(kernel_handle);
            match recorder.record_dispatch(
                &*kernel_borrow,
                group_x,
                group_y,
                group_z,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_dispatch: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_dispatch(
    _recorder_handle: *const c_void,
    _kernel_handle: *const c_void,
    _group_x: u32,
    _group_y: u32,
    _group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err("record_dispatch: Linux-only", err_buf, err_buf_cap, err_len);
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_copy_image_to_buffer(
    recorder_handle: *const c_void,
    src_texture_handle: *const c_void,
    src_layout_raw: i32,
    dst_storage_buffer_handle: *const c_void,
    region: *const streamlib_plugin_abi::ImageCopyRegionRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_copy_image_to_buffer",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_copy_image_to_buffer: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_texture_handle.is_null() {
                write_err(
                    "record_copy_image_to_buffer: null src texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_storage_buffer_handle.is_null() {
                write_err(
                    "record_copy_image_to_buffer: null dst storage_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if region.is_null() {
                write_err(
                    "record_copy_image_to_buffer: null region pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let region_ref = unsafe { &*region };
            let src_borrow = make_texture_borrow(src_texture_handle);
            let dst_borrow =
                make_storage_buffer_borrow(dst_storage_buffer_handle);
            let src_layout =
                streamlib_consumer_rhi::VulkanLayout(src_layout_raw);
            let region_rust = crate::vulkan::rhi::ImageCopyRegion {
                width: region_ref.width,
                height: region_ref.height,
                buffer_offset: region_ref.buffer_offset,
                buffer_row_length: region_ref.buffer_row_length,
                buffer_image_height: region_ref.buffer_image_height,
                mip_level: region_ref.mip_level,
                array_layer: region_ref.array_layer,
            };
            match recorder.record_copy_image_to_buffer(
                &*src_borrow,
                src_layout,
                &*dst_borrow,
                region_rust,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_copy_image_to_buffer: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_copy_image_to_buffer(
    _recorder_handle: *const c_void,
    _src_texture_handle: *const c_void,
    _src_layout_raw: i32,
    _dst_storage_buffer_handle: *const c_void,
    _region: *const streamlib_plugin_abi::ImageCopyRegionRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_copy_image_to_buffer: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_pixel_buffer_barrier(
    recorder_handle: *const c_void,
    pixel_buffer_handle: *const c_void,
    from_stage_raw: i64,
    to_stage_raw: i64,
    from_access_raw: i64,
    to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_pixel_buffer_barrier",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_pixel_buffer_barrier: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer_handle.is_null() {
                write_err(
                    "record_pixel_buffer_barrier: null pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let buffer_borrow =
                make_pixel_buffer_borrow(pixel_buffer_handle);
            let from_stage =
                crate::vulkan::rhi::VulkanStage(from_stage_raw as u64);
            let to_stage = crate::vulkan::rhi::VulkanStage(to_stage_raw as u64);
            let from_access =
                crate::vulkan::rhi::VulkanAccess(from_access_raw as u64);
            let to_access =
                crate::vulkan::rhi::VulkanAccess(to_access_raw as u64);
            match recorder.record_buffer_barrier(
                &*buffer_borrow,
                from_stage,
                to_stage,
                from_access,
                to_access,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_pixel_buffer_barrier: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_pixel_buffer_barrier(
    _recorder_handle: *const c_void,
    _pixel_buffer_handle: *const c_void,
    _from_stage_raw: i64,
    _to_stage_raw: i64,
    _from_access_raw: i64,
    _to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_pixel_buffer_barrier: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_copy_image_to_pixel_buffer(
    recorder_handle: *const c_void,
    src_texture_handle: *const c_void,
    src_layout_raw: i32,
    dst_pixel_buffer_handle: *const c_void,
    region: *const streamlib_plugin_abi::ImageCopyRegionRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_copy_image_to_pixel_buffer",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_copy_image_to_pixel_buffer: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if src_texture_handle.is_null() {
                write_err(
                    "record_copy_image_to_pixel_buffer: null src texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if dst_pixel_buffer_handle.is_null() {
                write_err(
                    "record_copy_image_to_pixel_buffer: null dst pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if region.is_null() {
                write_err(
                    "record_copy_image_to_pixel_buffer: null region pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let region_ref = unsafe { &*region };
            let src_borrow = make_texture_borrow(src_texture_handle);
            let dst_borrow =
                make_pixel_buffer_borrow(dst_pixel_buffer_handle);
            let src_layout =
                streamlib_consumer_rhi::VulkanLayout(src_layout_raw);
            let region_rust = crate::vulkan::rhi::ImageCopyRegion {
                width: region_ref.width,
                height: region_ref.height,
                buffer_offset: region_ref.buffer_offset,
                buffer_row_length: region_ref.buffer_row_length,
                buffer_image_height: region_ref.buffer_image_height,
                mip_level: region_ref.mip_level,
                array_layer: region_ref.array_layer,
            };
            match recorder.record_copy_image_to_buffer(
                &*src_borrow,
                src_layout,
                &*dst_borrow,
                region_rust,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_copy_image_to_pixel_buffer: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_copy_image_to_pixel_buffer(
    _recorder_handle: *const c_void,
    _src_texture_handle: *const c_void,
    _src_layout_raw: i32,
    _dst_pixel_buffer_handle: *const c_void,
    _region: *const streamlib_plugin_abi::ImageCopyRegionRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_copy_image_to_pixel_buffer: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_submit_signaling_timeline(
    recorder_handle: *const c_void,
    timeline_handle: *const c_void,
    signal_value: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_submit_signaling_timeline",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "submit_signaling_timeline: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if timeline_handle.is_null() {
                write_err(
                    "submit_signaling_timeline: null timeline handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: `timeline_handle` is a borrowed pointer from
            // the cdylib's
            // `RhiCommandRecorder::dispatch_submit_signaling_timeline_via_vtable`
            // (which gets it via `self as *const Self` on the
            // β-shape's outer `HostVulkanTimelineSemaphore` borrow,
            // same convention as the v13
            // `wait_timeline_semaphore` slot). The borrow lasts
            // only for the duration of this call.
            let timeline = unsafe {
                &*(timeline_handle
                    as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore)
            };
            match recorder.submit_signaling_timeline(timeline, signal_value) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("submit_signaling_timeline: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_submit_signaling_timeline(
    _recorder_handle: *const c_void,
    _timeline_handle: *const c_void,
    _signal_value: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "submit_signaling_timeline: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

// -------------------------------------------------------------------------
// v3 (#1066) — swapchain render-path wrappers
// -------------------------------------------------------------------------

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_swapchain_image_barrier(
    recorder_handle: *const c_void,
    image_raw: u64,
    from_layout_raw: i32,
    to_layout_raw: i32,
    from_stage_raw: i64,
    to_stage_raw: i64,
    from_access_raw: i64,
    to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_swapchain_image_barrier",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_swapchain_image_barrier: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            // Dispatch into the RHI-side `from_wire` shim — all
            // `vulkanalia` construction stays inside `vulkan/rhi/`
            // (the check-boundaries rule keeps raw vulkanalia out of
            // `core/plugin/`).
            match recorder.record_swapchain_image_barrier_from_wire(
                image_raw,
                from_layout_raw,
                to_layout_raw,
                from_stage_raw,
                to_stage_raw,
                from_access_raw,
                to_access_raw,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_swapchain_image_barrier: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_swapchain_image_barrier(
    _recorder_handle: *const c_void,
    _image_raw: u64,
    _from_layout_raw: i32,
    _to_layout_raw: i32,
    _from_stage_raw: i64,
    _to_stage_raw: i64,
    _from_access_raw: i64,
    _to_access_raw: i64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_swapchain_image_barrier: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_cmd_begin_dynamic_rendering(
    recorder_handle: *const c_void,
    image_view_raw: u64,
    extent_w: u32,
    extent_h: u32,
    has_clear_color: u32,
    clear_r: f32,
    clear_g: f32,
    clear_b: f32,
    clear_a: f32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_cmd_begin_dynamic_rendering",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "cmd_begin_dynamic_rendering: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let clear = if has_clear_color != 0 {
                Some([clear_r, clear_g, clear_b, clear_a])
            } else {
                None
            };
            // Dispatch into the RHI-side `from_wire` shim — see the
            // `record_swapchain_image_barrier` wrapper above for the
            // check-boundaries rationale.
            match recorder.cmd_begin_dynamic_rendering_from_wire(
                image_view_raw,
                extent_w,
                extent_h,
                clear,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("cmd_begin_dynamic_rendering: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_cmd_begin_dynamic_rendering(
    _recorder_handle: *const c_void,
    _image_view_raw: u64,
    _extent_w: u32,
    _extent_h: u32,
    _has_clear_color: u32,
    _clear_r: f32,
    _clear_g: f32,
    _clear_b: f32,
    _clear_a: f32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "cmd_begin_dynamic_rendering: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_command_recorder_cmd_end_dynamic_rendering(
    recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_cmd_end_dynamic_rendering",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "cmd_end_dynamic_rendering: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match recorder.cmd_end_dynamic_rendering() {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("cmd_end_dynamic_rendering: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_command_recorder_cmd_end_dynamic_rendering(
    _recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "cmd_end_dynamic_rendering: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_submit_with_semaphores(
    recorder_handle: *const c_void,
    waits_ptr: *const streamlib_plugin_abi::SemaphoreSubmitInfoRepr,
    waits_count: usize,
    signals_ptr: *const streamlib_plugin_abi::SemaphoreSubmitInfoRepr,
    signals_count: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_submit_with_semaphores",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "submit_with_semaphores: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            // SAFETY: caller-owned arrays. We only read; the buffers
            // outlive the call by the cdylib-side `Vec` they came from.
            let waits_repr: &[streamlib_plugin_abi::SemaphoreSubmitInfoRepr] =
                if waits_count == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(waits_ptr, waits_count) }
                };
            let signals_repr: &[streamlib_plugin_abi::SemaphoreSubmitInfoRepr] =
                if signals_count == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(signals_ptr, signals_count) }
                };
            // Dispatch into the RHI-side `from_wire` shim — see the
            // `record_swapchain_image_barrier` wrapper above for the
            // check-boundaries rationale.
            match recorder.submit_with_semaphores_from_wire(waits_repr, signals_repr)
            {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("submit_with_semaphores: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_submit_with_semaphores(
    _recorder_handle: *const c_void,
    _waits_ptr: *const streamlib_plugin_abi::SemaphoreSubmitInfoRepr,
    _waits_count: usize,
    _signals_ptr: *const streamlib_plugin_abi::SemaphoreSubmitInfoRepr,
    _signals_count: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "submit_with_semaphores: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_draw(
    recorder_handle: *const c_void,
    kernel_handle: *const c_void,
    frame_index: u32,
    draw: *const streamlib_plugin_abi::DrawCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_draw",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_draw: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if kernel_handle.is_null() {
                write_err(
                    "record_draw: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if draw.is_null() {
                write_err(
                    "record_draw: null draw pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let draw_ref = unsafe { &*draw };
            let viewport = if draw_ref.viewport_present != 0 {
                Some(crate::core::rhi::Viewport {
                    x: draw_ref.viewport.x,
                    y: draw_ref.viewport.y,
                    width: draw_ref.viewport.width,
                    height: draw_ref.viewport.height,
                    min_depth: draw_ref.viewport.min_depth,
                    max_depth: draw_ref.viewport.max_depth,
                })
            } else {
                None
            };
            let scissor = if draw_ref.scissor_present != 0 {
                Some(crate::core::rhi::ScissorRect {
                    x: draw_ref.scissor.x,
                    y: draw_ref.scissor.y,
                    width: draw_ref.scissor.width,
                    height: draw_ref.scissor.height,
                })
            } else {
                None
            };
            let draw_call = crate::core::rhi::DrawCall {
                vertex_count: draw_ref.vertex_count,
                instance_count: draw_ref.instance_count,
                first_vertex: draw_ref.first_vertex,
                first_instance: draw_ref.first_instance,
                viewport,
                scissor,
            };
            let kernel_borrow = make_graphics_kernel_borrow(kernel_handle);
            match recorder.record_draw(&*kernel_borrow, frame_index, &draw_call) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_draw: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_draw(
    _recorder_handle: *const c_void,
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _draw: *const streamlib_plugin_abi::DrawCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err("record_draw: Linux-only", err_buf, err_buf_cap, err_len);
    1
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_draw_indexed(
    recorder_handle: *const c_void,
    kernel_handle: *const c_void,
    frame_index: u32,
    draw: *const streamlib_plugin_abi::DrawIndexedCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_record_draw_indexed",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "record_draw_indexed: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if kernel_handle.is_null() {
                write_err(
                    "record_draw_indexed: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if draw.is_null() {
                write_err(
                    "record_draw_indexed: null draw pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let draw_ref = unsafe { &*draw };
            let viewport = if draw_ref.viewport_present != 0 {
                Some(crate::core::rhi::Viewport {
                    x: draw_ref.viewport.x,
                    y: draw_ref.viewport.y,
                    width: draw_ref.viewport.width,
                    height: draw_ref.viewport.height,
                    min_depth: draw_ref.viewport.min_depth,
                    max_depth: draw_ref.viewport.max_depth,
                })
            } else {
                None
            };
            let scissor = if draw_ref.scissor_present != 0 {
                Some(crate::core::rhi::ScissorRect {
                    x: draw_ref.scissor.x,
                    y: draw_ref.scissor.y,
                    width: draw_ref.scissor.width,
                    height: draw_ref.scissor.height,
                })
            } else {
                None
            };
            let draw_call = crate::core::rhi::DrawIndexedCall {
                index_count: draw_ref.index_count,
                instance_count: draw_ref.instance_count,
                first_index: draw_ref.first_index,
                vertex_offset: draw_ref.vertex_offset,
                first_instance: draw_ref.first_instance,
                viewport,
                scissor,
            };
            let kernel_borrow = make_graphics_kernel_borrow(kernel_handle);
            match recorder.record_draw_indexed(
                &*kernel_borrow,
                frame_index,
                &draw_call,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record_draw_indexed: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_command_recorder_record_draw_indexed(
    _recorder_handle: *const c_void,
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _draw: *const streamlib_plugin_abi::DrawIndexedCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record_draw_indexed: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// v5 — bare submit. Sibling of v1 `submit_signaling_timeline`
/// without the timeline-semaphore parameters; used by
/// `RhiToneMapper::apply_with_layouts`'s private recorder when
/// reached from cdylib-resident processor code (the per-input
/// tone-mapping normalization step in graphics-kernel wrappers
/// is the first in-tree consumer).
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_command_recorder_submit(
    recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_submit",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "submit: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match recorder.submit() {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("submit: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_command_recorder_submit(
    _recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err("submit: Linux-only", err_buf, err_buf_cap, err_len);
    1
}

/// v5 — submit and block. Sibling of [`host_command_recorder_submit`];
/// caller-side `vkWaitForFences` after submit.
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_command_recorder_submit_and_wait(
    recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_command_recorder_submit_and_wait",
        || -> i32 {
            let Some(recorder) =
                (unsafe { handle_as_command_recorder_mut(recorder_handle) })
            else {
                write_err(
                    "submit_and_wait: null recorder handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match recorder.submit_and_wait() {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("submit_and_wait: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_command_recorder_submit_and_wait(
    _recorder_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "submit_and_wait: Linux-only",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `RhiCommandRecorderMethodsVTable` wired to the
/// per-method wrappers above. Covers the v1 record-then-submit
/// slots, the v3 swapchain render-path slots used by the cdylib
/// display, and the v5 bare-submit slots used by `RhiToneMapper`
/// when reached from cdylib-resident processor code.
pub static HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE:
    streamlib_plugin_abi::RhiCommandRecorderMethodsVTable =
    streamlib_plugin_abi::RhiCommandRecorderMethodsVTable {
        layout_version:
            streamlib_plugin_abi::RHI_COMMAND_RECORDER_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        begin: host_command_recorder_begin,
        record_image_barrier: host_command_recorder_record_image_barrier,
        record_buffer_barrier: host_command_recorder_record_buffer_barrier,
        record_dispatch: host_command_recorder_record_dispatch,
        record_copy_image_to_buffer:
            host_command_recorder_record_copy_image_to_buffer,
        submit_signaling_timeline:
            host_command_recorder_submit_signaling_timeline,
        record_pixel_buffer_barrier:
            host_command_recorder_record_pixel_buffer_barrier,
        record_copy_image_to_pixel_buffer:
            host_command_recorder_record_copy_image_to_pixel_buffer,
        record_swapchain_image_barrier:
            host_command_recorder_record_swapchain_image_barrier,
        cmd_begin_dynamic_rendering:
            host_command_recorder_cmd_begin_dynamic_rendering,
        cmd_end_dynamic_rendering:
            host_command_recorder_cmd_end_dynamic_rendering,
        submit_with_semaphores: host_command_recorder_submit_with_semaphores,
        record_draw: host_command_recorder_record_draw,
        record_draw_indexed: host_command_recorder_record_draw_indexed,
        submit: host_command_recorder_submit,
        submit_and_wait: host_command_recorder_submit_and_wait,
    };

/// Accessor for the host's static `RhiCommandRecorderMethodsVTable`
/// — used by `RhiCommandRecorder::from_inner` to populate the
/// β-shape's `methods_vtable` field.
///
/// See [`host_vulkan_compute_kernel_methods_vtable`] for the routing
/// rationale — cdylib β-shape constructors must store the host's
/// vtable pointer so dispatches actually cross DSO boundaries.
pub fn host_rhi_command_recorder_methods_vtable(
) -> *const streamlib_plugin_abi::RhiCommandRecorderMethodsVTable {
    match host_callbacks() {
        Some(c) if !c.rhi_command_recorder_methods_vtable.is_null() => {
            c.rhi_command_recorder_methods_vtable
        }
        _ => &HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE,
    }
}
#[cfg(all(test, target_os = "linux"))]
mod gpu_rhi_command_recorder_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v1 method slots on
    //! `RhiCommandRecorderMethodsVTable`. Each wrapper must reject a
    //! null recorder handle before reaching any recorder-side state
    //! (i.e. before any deref) so cdylib callers get a clean error
    //! return on the wire-format path instead of UB.
    //!
    //! The secondary null-handle guards (texture / storage_buffer /
    //! kernel / timeline) live in the same wrappers and fire when the
    //! recorder handle is valid; they're exercised end-to-end by the
    //! camera-package dlopen smoke test (which holds a real recorder).
    //! Tier-1 cannot reach them without first passing the recorder-
    //! handle deref — passing a non-null garbage handle for the
    //! recorder trips a misaligned-pointer-deref panic before any
    //! subsequent guard runs. This mirrors the precedent set by
    //! `gpu_rhi_color_converter_methods_vtable_null_tests`.
    //!
    //! Success-path coverage (real Box<RhiCommandRecorderInner>, a
    //! full begin → record_* → submit_signaling_timeline cycle)
    //! requires a real Vulkan device and arrives in the camera-
    //! package dlopen smoke test.

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    fn dummy_region() -> streamlib_plugin_abi::ImageCopyRegionRepr {
        streamlib_plugin_abi::ImageCopyRegionRepr::default()
    }

    #[test]
    fn begin_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.begin)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("begin: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_image_barrier_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.record_image_barrier)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_image_barrier: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_buffer_barrier_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.record_buffer_barrier)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_buffer_barrier: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_dispatch_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.record_dispatch)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_dispatch: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_copy_image_to_buffer_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let region = dummy_region();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.record_copy_image_to_buffer)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                &region,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_copy_image_to_buffer: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn submit_signaling_timeline_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.submit_signaling_timeline)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("submit_signaling_timeline: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    // v5 — submit / submit_and_wait wrappers.

    #[test]
    fn submit_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.submit)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("submit: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn submit_and_wait_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.submit_and_wait)(
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("submit_and_wait: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_pixel_buffer_barrier_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE.record_pixel_buffer_barrier)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_pixel_buffer_barrier: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_copy_image_to_pixel_buffer_rejects_null_recorder_handle() {
        let (mut buf, mut len) = make_err_buf();
        let region = dummy_region();
        let rc = unsafe {
            (HOST_RHI_COMMAND_RECORDER_METHODS_VTABLE
                .record_copy_image_to_pixel_buffer)(
                std::ptr::null(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                &region,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record_copy_image_to_pixel_buffer: null recorder handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}
