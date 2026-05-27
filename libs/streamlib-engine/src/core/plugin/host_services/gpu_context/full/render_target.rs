// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextFullAccessVTable::acquire_render_target_dma_buf_image`
//! (Phase C3, Linux-only).
//!
//! Allocates a render-target-capable DMA-BUF-backed `VkImage`
//! through the engine's privileged
//! `GpuContext::acquire_render_target_dma_buf_image` path: picks a
//! tiled DRM modifier via the EGL probe and runs the host RHI's
//! exportable allocator. The resulting `Texture` β-shape is written
//! into `*out_texture` so the cdylib can take ownership.

use std::ffi::c_void;

use super::super::scope_token::with_full_scope_or_err;
use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::write_err;

// ---------------- Render-target allocation (Phase C3, Linux-only) ---------

/// Allocate a render-target-capable DMA-BUF-backed `VkImage`. Looks
/// up the bound `Arc<GpuContext>` via the scope_token; runs
/// [`crate::core::context::GpuContext::acquire_render_target_dma_buf_image`]
/// (which picks a tiled DRM modifier via the EGL probe and allocates
/// through the privileged RHI path), and writes the resulting
/// `Texture` β-shape into `*out_texture` on success.
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_acquire_render_target_dma_buf_image(
    scope_token: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_acquire_render_target_dma_buf_image",
        || -> i32 {
            if out_texture.is_null() {
                write_err(
                    "acquire_render_target_dma_buf_image: null out_texture",
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
                6 => streamlib_consumer_rhi::TextureFormat::Nv12,
                _ => {
                    write_err(
                        &format!(
                            "acquire_render_target_dma_buf_image: invalid \
                             format_raw {}",
                            format_raw
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "acquire_render_target_dma_buf_image",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.acquire_render_target_dma_buf_image(width, height, format),
            );
            match result {
                Some(Ok(texture)) => {
                    unsafe {
                        std::ptr::write(
                            out_texture as *mut crate::core::rhi::Texture,
                            texture,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1, // err_buf already populated by helper
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_acquire_render_target_dma_buf_image(
    _scope_token: *const c_void,
    _width: u32,
    _height: u32,
    _format_raw: u32,
    _out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "acquire_render_target_dma_buf_image: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

