// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextLimitedAccessVTable` Linux-only buffer lifecycle +
//! `acquire_*_buffer` method dispatch (v5).
//!
//! All four buffer types (`StorageBuffer`, `UniformBuffer`,
//! `VertexBuffer`, `IndexBuffer`) wrap `Arc<HostVulkanBuffer>` under
//! the hood. The per-type callbacks are individually addressable in
//! the vtable (so future per-type divergence doesn't force a
//! re-version) but share the same host-side bookkeeping today. On
//! non-Linux hosts the buffer types don't exist, so the callbacks
//! compile to no-ops / error returns — the vtable slot is
//! unconditional for ABI stability.

use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::sync::Arc;

use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::write_err;
#[cfg(target_os = "linux")]
use super::super::shared::handle_as_gpu_context;

// -------------------------------------------------------------------------
// Linux-only buffer Arc-handle lifecycle
// -------------------------------------------------------------------------
//
// All 4 buffer types (`StorageBuffer`, `UniformBuffer`, `VertexBuffer`,
// `IndexBuffer`) wrap `Arc<HostVulkanBuffer>` under the hood. The per-
// type callbacks are individually addressable in the vtable (so future
// per-type divergence doesn't force a re-version) but share the same
// host-side bookkeeping today. On non-Linux hosts the buffer types
// don't exist, so the callbacks compile to no-ops / error returns —
// the vtable slot is unconditional for ABI stability.

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clone_host_vulkan_buffer_arc(
    handle: *const c_void,
) {
    if handle.is_null() {
        return;
    }
    // SAFETY: `handle` is `Arc::into_raw(Arc<HostVulkanBuffer>)`-shaped
    // (see each buffer type's `from_arc_into_raw` constructor).
    unsafe {
        Arc::increment_strong_count(handle as *const crate::vulkan::rhi::HostVulkanBuffer);
    }
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_host_vulkan_buffer_arc(
    handle: *const c_void,
) {
    if handle.is_null() {
        return;
    }
    // SAFETY: matched with the `Arc::into_raw` in each buffer type's
    // `from_arc_into_raw` constructor.
    unsafe {
        Arc::decrement_strong_count(handle as *const crate::vulkan::rhi::HostVulkanBuffer);
    }
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clone_host_vulkan_buffer_arc(
    _handle: *const c_void,
) {
    // Buffer types only exist on Linux; this callback is unreachable
    // on other platforms. Defensive no-op.
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_host_vulkan_buffer_arc(
    _handle: *const c_void,
) {
    // Buffer types only exist on Linux; defensive no-op.
}

// Per-type wrappers. Each just delegates to the shared
// `host_vulkan_buffer_arc` pair today but lives in the vtable as a
// dedicated slot, so a future per-type divergence (e.g. UniformBuffer
// growing a per-type cached field that needs its own clone semantics)
// only edits the wrapper without touching the vtable surface.

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clone_storage_buffer(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_clone_storage_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_storage_buffer(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_drop_storage_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clone_uniform_buffer(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_clone_uniform_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_uniform_buffer(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_drop_uniform_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clone_vertex_buffer(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_clone_vertex_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_vertex_buffer(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_drop_vertex_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clone_index_buffer(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_clone_index_buffer",
        || unsafe { host_gpu_lim_clone_host_vulkan_buffer_arc(handle) },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_index_buffer(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_drop_index_buffer",
        || unsafe { host_gpu_lim_drop_host_vulkan_buffer_arc(handle) },
        (),
    )
}

// -------------------------------------------------------------------------
// Linux-only acquire_*_buffer method dispatch (v5)
// -------------------------------------------------------------------------

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_acquire_storage_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_storage_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_storage_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_storage_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_storage_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(out_buffer as *mut crate::core::rhi::StorageBuffer, buf);
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_acquire_uniform_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_uniform_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_uniform_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_uniform_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_uniform_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(out_buffer as *mut crate::core::rhi::UniformBuffer, buf);
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_acquire_vertex_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_vertex_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_vertex_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_vertex_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_vertex_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(out_buffer as *mut crate::core::rhi::VertexBuffer, buf);
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_acquire_index_buffer(
    handle: *const c_void,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_acquire_index_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                write_err(
                    "acquire_index_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_buffer.is_null() {
                write_err(
                    "acquire_index_buffer: null out_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.acquire_index_buffer(byte_size) {
                Ok(buf) => {
                    unsafe {
                        std::ptr::write(out_buffer as *mut crate::core::rhi::IndexBuffer, buf);
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_acquire_storage_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_storage_buffer: StorageBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_acquire_uniform_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_uniform_buffer: UniformBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_acquire_vertex_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_vertex_buffer: VertexBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_acquire_index_buffer(
    _handle: *const c_void,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_index_buffer: IndexBuffer is not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
