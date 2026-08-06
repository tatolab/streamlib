// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextFullAccessVTable` texture-readback construction +
//! teardown callbacks (M32 #1261 fill-in against the reserved v11 slots).
//!
//! `create_texture_readback` validates its scope token via
//! [`with_full_scope_or_err`], decodes + validates the format (rejecting
//! planar `Nv12`), runs the engine's
//! [`crate::core::context::GpuContext::create_texture_readback`]
//! primitive, boxes the resulting `Arc<VulkanTextureReadback>`, and
//! writes the opaque handle plus the two cached-POD out-params
//! (`out_handle_id`, `out_staging_size` — both sourced from the
//! primitive, never recomputed ABI-side).
//!
//! `drop_texture_readback` reclaims the boxed Arc under the panic net —
//! the primitive's Drop can block on the pending timeline, which is
//! sound inside `run_host_extern_c`.

use std::ffi::c_void;

#[cfg(target_os = "linux")]
use std::sync::Arc;

use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::write_err;
#[cfg(target_os = "linux")]
use super::super::super::shared::wire::slice_from_raw;
#[cfg(target_os = "linux")]
use super::super::scope_token::with_full_scope_or_err;

// ---------------- Construction (Linux-only) ----------------

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_texture_readback(
    gpu_handle: *const c_void,
    label_ptr: *const u8,
    label_len: usize,
    width: u32,
    height: u32,
    format_raw: u32,
    out_readback_handle: *mut *const c_void,
    out_handle_id: *mut u64,
    out_staging_size: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_texture_readback",
        || -> i32 {
            if out_readback_handle.is_null()
                || out_handle_id.is_null()
                || out_staging_size.is_null()
            {
                write_err(
                    "create_texture_readback: null out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match format_raw {
                0 => streamlib_consumer_rhi::TextureFormat::Rgba8Unorm,
                1 => streamlib_consumer_rhi::TextureFormat::Rgba8UnormSrgb,
                2 => streamlib_consumer_rhi::TextureFormat::Bgra8Unorm,
                3 => streamlib_consumer_rhi::TextureFormat::Bgra8UnormSrgb,
                4 => streamlib_consumer_rhi::TextureFormat::Rgba16Float,
                5 => streamlib_consumer_rhi::TextureFormat::Rgba32Float,
                6 => {
                    // Planar Nv12 has no single tightly-packed
                    // bytes-per-pixel; the readback staging model assumes
                    // a flat interleaved plane. Reject with a typed error
                    // rather than silently produce a mis-sized buffer.
                    write_err(
                        "create_texture_readback: planar Nv12 is not supported \
                         (readback assumes a flat interleaved plane)",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                _ => {
                    write_err(
                        &format!("create_texture_readback: invalid format_raw {format_raw}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // SAFETY: caller-supplied `(label_ptr, label_len)` UTF-8
            // slice, valid for the dispatch.
            let label_bytes = unsafe { slice_from_raw(label_ptr, label_len) };
            let label = String::from_utf8_lossy(label_bytes).into_owned();
            let descriptor = crate::core::rhi::TextureReadbackDescriptor {
                label: &label,
                format,
                width,
                height,
            };
            let result = with_full_scope_or_err(
                gpu_handle,
                "create_texture_readback",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_texture_readback(&descriptor),
            );
            match result {
                Some(Ok(arc)) => {
                    // Cached POD sourced from the primitive itself — never
                    // recomputed ABI-side (amendment 6).
                    let handle_id = arc.handle_id();
                    let staging_size = arc.staging_size();
                    // Box-shaped opaque handle: `Box<Arc<VulkanTextureReadback>>`.
                    // `!Clone` — drop_texture_readback reclaims it.
                    let boxed: Box<Arc<crate::vulkan::rhi::VulkanTextureReadback>> =
                        Box::new(arc);
                    let raw = Box::into_raw(boxed) as *const c_void;
                    // SAFETY: out pointers null-checked above.
                    unsafe {
                        std::ptr::write(out_readback_handle, raw);
                        std::ptr::write(out_handle_id, handle_id);
                        std::ptr::write(out_staging_size, staging_size);
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1, // err_buf populated by with_full_scope_or_err
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_texture_readback(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_texture_readback",
        || {
            if handle.is_null() {
                return;
            }
            // SAFETY: paired with `Box::into_raw(Box<Arc<VulkanTextureReadback>>)`
            // in `host_gpu_full_create_texture_readback`. Reclaiming via
            // `Box::from_raw` drops the Arc; the primitive's Drop can
            // block on the pending timeline, which is sound inside the
            // panic net (a caught panic logs, never converts — void return).
            unsafe {
                let _ = Box::from_raw(
                    handle as *mut Arc<crate::vulkan::rhi::VulkanTextureReadback>,
                );
            }
        },
        (),
    )
}

// ---------------- Non-Linux stubs ----------------
//
// `VulkanTextureReadback` is Linux-only (`vulkan/rhi/mod.rs` is
// `#[cfg(target_os = "linux")]`), so the construction slot returns a
// typed "not available on this platform" error off-Linux. The drop slot
// is a null-safe no-op: no create slot yields a handle off-Linux, so
// drop is never called with one.

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_texture_readback(
    _gpu_handle: *const c_void,
    _label_ptr: *const u8,
    _label_len: usize,
    _width: u32,
    _height: u32,
    _format_raw: u32,
    _out_readback_handle: *mut *const c_void,
    _out_handle_id: *mut u64,
    _out_staging_size: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_texture_readback",
        || -> i32 {
            write_err(
                "create_texture_readback: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_texture_readback(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_texture_readback",
        || {
            let _ = handle;
        },
        (),
    )
}
