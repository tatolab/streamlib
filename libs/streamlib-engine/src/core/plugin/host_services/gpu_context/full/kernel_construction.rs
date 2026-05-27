// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextFullAccessVTable` kernel + texture-ring construction
//! callbacks (Linux-only).
//!
//! `create_compute_kernel`, `create_graphics_kernel`,
//! `create_ray_tracing_kernel`, and `create_texture_ring`. Each
//! validates its scope_token via [`with_full_scope_or_err`], decodes
//! the kernel descriptor msgpack from the cdylib, and runs the
//! engine's privileged constructor (`VulkanComputeKernel::new`,
//! `VulkanGraphicsKernel::new`, `VulkanRayTracingKernel::new`,
//! `GpuContextFullAccess::create_texture_ring`).
//!
//! Output handles are returned as `*const c_void` cast of
//! `Arc::into_raw(arc)`; cdylib carries the Arc and routes refcount
//! through this module's `kernel_lifecycle` siblings.

use std::ffi::c_void;

use streamlib_plugin_abi::{
    ComputeKernelDescriptorRepr, GraphicsKernelDescriptorRepr, RayTracingKernelDescriptorRepr,
};

#[cfg(target_os = "linux")]
use super::super::scope_token::with_full_scope_or_err;
#[cfg(target_os = "linux")]
use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::write_err;

// ---------------- Kernel construction (Linux-only) ----------------

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_compute_kernel(
    scope_token: *const c_void,
    desc: *const ComputeKernelDescriptorRepr,
    out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_compute_kernel",
        || -> i32 {
            if desc.is_null() || out_kernel.is_null() {
                write_err(
                    "create_compute_kernel: null desc or out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let repr: &ComputeKernelDescriptorRepr = unsafe { &*desc };
            let result = with_full_scope_or_err(
                scope_token,
                "create_compute_kernel",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| unsafe {
                    crate::core::rhi::plugin_abi_bridge::with_decoded_compute_kernel_descriptor(
                        repr,
                        |rust_desc| gpu.create_compute_kernel(rust_desc),
                    )
                },
            );
            match result {
                Some(Ok(kernel)) => {
                    // `kernel` is the β-shape; its `handle` is the
                    // `Arc::into_raw(Arc<<Type>Inner>)` raw pointer
                    // already. Forget the β-shape so the strong ref
                    // transfers to cdylib; the cdylib reconstructs its
                    // own β-shape from { handle: raw, vtable } and
                    // never sees the `Arc<X>` internal layout.
                    let raw = kernel.handle;
                    std::mem::forget(kernel);
                    unsafe { std::ptr::write(out_kernel, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1, // err_buf populated by helper
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_graphics_kernel(
    scope_token: *const c_void,
    desc: *const GraphicsKernelDescriptorRepr,
    out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_graphics_kernel",
        || -> i32 {
            if desc.is_null() || out_kernel.is_null() {
                write_err(
                    "create_graphics_kernel: null desc or out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let repr: &GraphicsKernelDescriptorRepr = unsafe { &*desc };
            let result = with_full_scope_or_err(
                scope_token,
                "create_graphics_kernel",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| unsafe {
                    crate::core::rhi::plugin_abi_bridge::with_decoded_graphics_kernel_descriptor(
                        repr,
                        |rust_desc| gpu.create_graphics_kernel(rust_desc),
                    )
                },
            );
            match result {
                Some(Ok(kernel)) => {
                    // β-shape: extract the opaque handle (which is
                    // already `Arc::into_raw(Arc<<Type>Inner>)`-shaped)
                    // and `mem::forget` the wrapper so the strong ref
                    // transfers to cdylib. The cdylib reconstructs a
                    // fresh β-shape from { handle, vtable } and never
                    // sees the host's `Arc<X>` allocation header.
                    let raw = kernel.handle;
                    std::mem::forget(kernel);
                    unsafe { std::ptr::write(out_kernel, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_ray_tracing_kernel(
    gpu_handle: *const c_void,
    desc: *const RayTracingKernelDescriptorRepr,
    out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_ray_tracing_kernel",
        || -> i32 {
            if desc.is_null() || out_kernel.is_null() {
                write_err(
                    "create_ray_tracing_kernel: null desc or out pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let repr: &RayTracingKernelDescriptorRepr = unsafe { &*desc };
            let result = with_full_scope_or_err(
                gpu_handle,
                "create_ray_tracing_kernel",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| unsafe {
                    crate::core::rhi::plugin_abi_bridge::with_decoded_ray_tracing_kernel_descriptor(
                        repr,
                        |rust_desc| gpu.create_ray_tracing_kernel(rust_desc),
                    )
                },
            );
            match result {
                Some(Ok(kernel)) => {
                    // β-shape: extract the opaque handle (which is
                    // already `Arc::into_raw(Arc<<Type>Inner>)`-shaped)
                    // and `mem::forget` the wrapper so the strong ref
                    // transfers to cdylib. The cdylib reconstructs a
                    // fresh β-shape from { handle, vtable } and never
                    // sees the host's `Arc<X>` allocation header.
                    let raw = kernel.handle;
                    std::mem::forget(kernel);
                    unsafe { std::ptr::write(out_kernel, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_texture_ring(
    scope_token: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    usage_bits: u32,
    count: usize,
    out_ring: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_texture_ring",
        || -> i32 {
            if out_ring.is_null() {
                write_err(
                    "create_texture_ring: null out_ring pointer",
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
                        &format!("create_texture_ring: invalid format_raw {format_raw}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let usages =
                streamlib_consumer_rhi::TextureUsages::from_bits_truncate(usage_bits);
            let result = with_full_scope_or_err(
                scope_token,
                "create_texture_ring",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_texture_ring(width, height, format, usages, count),
            );
            match result {
                Some(Ok(ring)) => {
                    // `ring` is the β-shape; its handle is
                    // `Arc::into_raw(Arc<TextureRingInner>)`-shaped.
                    let raw = ring.handle;
                    std::mem::forget(ring);
                    unsafe { std::ptr::write(out_ring, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

// Non-Linux stubs for the create_* callbacks.
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_compute_kernel(
    _gpu_handle: *const c_void,
    _desc: *const ComputeKernelDescriptorRepr,
    _out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_compute_kernel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_graphics_kernel(
    _gpu_handle: *const c_void,
    _desc: *const GraphicsKernelDescriptorRepr,
    _out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_graphics_kernel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_ray_tracing_kernel(
    _gpu_handle: *const c_void,
    _desc: *const RayTracingKernelDescriptorRepr,
    _out_kernel: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_ray_tracing_kernel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}
#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_texture_ring(
    _gpu_handle: *const c_void,
    _width: u32,
    _height: u32,
    _format_raw: u32,
    _usage_bits: u32,
    _count: usize,
    _out_ring: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_texture_ring: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

