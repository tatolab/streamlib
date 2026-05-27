// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextLimitedAccessVTable` RhiCommandQueue + CommandBuffer
//! lifecycle + command-queue / command-buffer / blit method dispatch
//! (v7).
//!
//! Combines three banner-bounded sections of the original file:
//!
//! - RhiCommandQueue Arc-handle lifecycle + create_command_buffer
//!   from a queue handle.
//! - CommandBuffer lifecycle: `drop_command_buffer` plus the
//!   consume-semantics `commit` and `commit_and_wait` pair.
//! - High-level GpuContextLimitedAccess command-queue accessor,
//!   `create_command_buffer` shortcut, `copy_pixel_buffer_to_texture`,
//!   `blit_copy`, and the macOS `blit_copy_iosurface` slot.

use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::sync::Arc;

use super::super::shared::handle_as_gpu_context;
use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::{slice_from_raw, write_err};

// -------------------------------------------------------------------------
// RhiCommandQueue Arc-handle lifecycle + create_command_buffer
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clone_rhi_command_queue(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_clone_rhi_command_queue",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: `handle` is `Arc::into_raw(Arc<RhiCommandQueueInner>)`-shaped.
            unsafe {
                Arc::increment_strong_count(
                    handle as *const crate::core::rhi::command_queue::RhiCommandQueueInner,
                );
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_rhi_command_queue(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_rhi_command_queue",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with the `Arc::into_raw` in
            // `RhiCommandQueue::from_arc_into_raw`.
            unsafe {
                Arc::decrement_strong_count(
                    handle as *const crate::core::rhi::command_queue::RhiCommandQueueInner,
                );
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_create_command_buffer_from_queue(
    queue_handle: *const c_void,
    out_cb: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_create_command_buffer_from_queue",
        || -> i32 {
            if queue_handle.is_null() {
                write_err(
                    "create_command_buffer_from_queue: null queue handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if out_cb.is_null() {
                write_err(
                    "create_command_buffer_from_queue: null out_cb",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: `queue_handle` is
            // `Arc::into_raw(Arc<RhiCommandQueueInner>)`-shaped; the
            // Arc's strong count keeps the inner alive for the duration.
            let inner = unsafe {
                &*(queue_handle
                    as *const crate::core::rhi::command_queue::RhiCommandQueueInner)
            };
            let result = inner.inner.create_command_buffer();
            match result {
                Ok(platform_cb) => {
                    let cb_inner =
                        crate::core::rhi::command_buffer::CommandBufferInner {
                            inner: platform_cb,
                        };
                    let cb = crate::core::rhi::CommandBuffer::from_inner(cb_inner);
                    // SAFETY: out_cb points at caller-allocated stack
                    // storage for a CommandBuffer value.
                    unsafe {
                        std::ptr::write(
                            out_cb as *mut crate::core::rhi::CommandBuffer,
                            cb,
                        );
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

// -------------------------------------------------------------------------
// CommandBuffer lifecycle: drop + consume-semantics commits (v7)
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_drop_command_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_drop_command_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `Box::into_raw` in
            // `CommandBuffer::from_inner`.
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut crate::core::rhi::command_buffer::CommandBufferInner,
                );
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_commit_command_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_commit_command_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: matched with `Box::into_raw` in
            // `CommandBuffer::from_inner`; the cdylib's commit(self)
            // nulls its local fields after this call so Drop won't
            // double-free. We move-out of the Box so the platform
            // commit can take ownership of the inner by-value.
            let cb_box = unsafe {
                Box::from_raw(
                    handle as *mut crate::core::rhi::command_buffer::CommandBufferInner,
                )
            };
            let cb_inner = *cb_box;
            cb_inner.inner.commit();
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_commit_and_wait_command_buffer(handle: *const c_void) {
    run_host_extern_c(
        "host_gpu_lim_commit_and_wait_command_buffer",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: see `host_gpu_lim_commit_command_buffer`.
            let cb_box = unsafe {
                Box::from_raw(
                    handle as *mut crate::core::rhi::command_buffer::CommandBufferInner,
                )
            };
            let cb_inner = *cb_box;
            cb_inner.inner.commit_and_wait();
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_copy_texture_command_buffer(
    handle: *const c_void,
    src: *const c_void,
    dst: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_copy_texture_command_buffer",
        || {
            if handle.is_null() || src.is_null() || dst.is_null() {
                return;
            }
            // SAFETY: handle is `Box::into_raw(...)`-shaped; `&mut` is
            // sound because the cdylib's `&mut self` guarantees no
            // concurrent reference. src/dst are
            // `*const Texture` (layout locked by `texture_layout` test).
            unsafe {
                let cb_inner = &mut *(handle
                    as *mut crate::core::rhi::command_buffer::CommandBufferInner);
                let src_tex = &*(src as *const crate::core::rhi::Texture);
                let dst_tex = &*(dst as *const crate::core::rhi::Texture);
                // Re-use the existing platform-specific copy_texture
                // surface inside CommandBufferInner's `inner`.
                #[cfg(all(
                    not(feature = "backend-vulkan"),
                    any(feature = "backend-metal", any(target_os = "macos", target_os = "ios"))
                ))]
                {
                    cb_inner.inner.copy_texture(
                        &src_tex.host_inner().inner,
                        &dst_tex.host_inner().inner,
                    );
                }
                #[cfg(any(
                    feature = "backend-vulkan",
                    all(target_os = "linux", not(feature = "backend-metal"))
                ))]
                {
                    use crate::host_rhi::HostTextureExt;
                    cb_inner
                        .inner
                        .copy_texture(src_tex.vulkan_inner(), dst_tex.vulkan_inner());
                }
                #[cfg(target_os = "windows")]
                {
                    cb_inner.inner.copy_texture(
                        &src_tex.host_inner().inner,
                        &dst_tex.host_inner().inner,
                    );
                }
            }
        },
        (),
    )
}

// -------------------------------------------------------------------------
// GpuContextLimitedAccess command-queue / command-buffer / blit methods
// -------------------------------------------------------------------------

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_command_queue(
    gpu_handle: *const c_void,
    out_queue: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_command_queue",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err("command_queue: null gpu handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if out_queue.is_null() {
                write_err("command_queue: null out_queue", err_buf, err_buf_cap, err_len);
                return 1;
            }
            // `gpu.command_queue()` returns `&RhiCommandQueue` (a borrow
            // from GpuContext's stored field). Clone into a fresh owned
            // β-shape for the caller — the Clone impl runs the host's
            // `clone_rhi_command_queue` callback (Arc refcount bump).
            let owned = gpu.command_queue().clone();
            // SAFETY: out_queue points at caller-allocated stack storage.
            unsafe {
                std::ptr::write(
                    out_queue as *mut crate::core::rhi::RhiCommandQueue,
                    owned,
                );
            }
            0
        },
        1,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_create_command_buffer(
    gpu_handle: *const c_void,
    out_cb: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_create_command_buffer",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "create_command_buffer: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_cb.is_null() {
                write_err(
                    "create_command_buffer: null out_cb",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            match gpu.create_command_buffer() {
                Ok(cb) => {
                    // SAFETY: out_cb points at caller-allocated storage.
                    unsafe {
                        std::ptr::write(
                            out_cb as *mut crate::core::rhi::CommandBuffer,
                            cb,
                        );
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
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_copy_pixel_buffer_to_texture(
    gpu_handle: *const c_void,
    pixel_buffer: *const c_void,
    texture: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    width: u32,
    height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_copy_pixel_buffer_to_texture",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "copy_pixel_buffer_to_texture: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer.is_null() || texture.is_null() {
                write_err(
                    "copy_pixel_buffer_to_texture: null pixel_buffer or texture",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: pixel_buffer / texture point at β-shape values
            // whose layouts are locked by per-type regression tests.
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let tex = unsafe { &*(texture as *const crate::core::rhi::Texture) };
            let id_bytes = unsafe { slice_from_raw(surface_id_ptr, surface_id_len) };
            let id_str = match std::str::from_utf8(id_bytes) {
                Ok(s) => s,
                Err(_) => {
                    write_err(
                        "copy_pixel_buffer_to_texture: surface_id not valid UTF-8",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match gpu.copy_pixel_buffer_to_texture(pb, tex, id_str, width, height) {
                Ok(()) => 0,
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
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_copy_pixel_buffer_to_texture(
    _gpu_handle: *const c_void,
    _pixel_buffer: *const c_void,
    _texture: *const c_void,
    _surface_id_ptr: *const u8,
    _surface_id_len: usize,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "copy_pixel_buffer_to_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_blit_copy(
    gpu_handle: *const c_void,
    src: *const c_void,
    dst: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_blit_copy",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err("blit_copy: null gpu handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            if src.is_null() || dst.is_null() {
                write_err("blit_copy: null src or dst", err_buf, err_buf_cap, err_len);
                return 1;
            }
            // SAFETY: src / dst point at β-shape PixelBuffer values.
            let src_pb = unsafe { &*(src as *const crate::core::rhi::PixelBuffer) };
            let dst_pb = unsafe { &*(dst as *const crate::core::rhi::PixelBuffer) };
            match gpu.blit_copy(src_pb, dst_pb) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "macos")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_blit_copy_iosurface(
    gpu_handle: *const c_void,
    src_iosurface_ref: *const c_void,
    dst_pixel_buffer: *const c_void,
    width: u32,
    height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_blit_copy_iosurface",
        || -> i32 {
            let Some(gpu) = (unsafe { handle_as_gpu_context(gpu_handle) }) else {
                write_err(
                    "blit_copy_iosurface: null gpu handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if dst_pixel_buffer.is_null() {
                write_err(
                    "blit_copy_iosurface: null dst_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let dst_pb = unsafe {
                &*(dst_pixel_buffer as *const crate::core::rhi::PixelBuffer)
            };
            let src_io = src_iosurface_ref as crate::apple::corevideo_ffi::IOSurfaceRef;
            match unsafe { gpu.blit_copy_iosurface(src_io, dst_pb, width, height) } {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(not(target_os = "macos"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_blit_copy_iosurface(
    _gpu_handle: *const c_void,
    _src_iosurface_ref: *const c_void,
    _dst_pixel_buffer: *const c_void,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "blit_copy_iosurface: not available on this platform (macOS-only)",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
