// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextFullAccessVTable` Phase D (#906) privileged callbacks.
//!
//! Every method validates its `scope_token` via
//! [`with_full_scope_or_err`] (resolving the bound `Arc<GpuContext>`
//! from the escalate-scope registry) before dispatching to the
//! resolved context: `wait_device_idle`, `acquire_output_texture`,
//! `upload_pixel_buffer_as_texture`, `color_converter`,
//! `create_command_recorder`, `build_triangles_blas`, `build_tlas`,
//! `supports_ray_tracing_pipeline`, `gpu_capabilities`,
//! `create_timeline_semaphore`, `import_dma_buf_storage_buffer`,
//! `check_in_surface`, `host_vulkan_device_arc`,
//! `host_vulkan_texture_arc`.

use std::ffi::c_void;
#[cfg(target_os = "linux")]
use std::sync::Arc;

use super::super::scope_token::with_full_scope_or_err;
use super::super::shared::pixel_format_from_raw;
use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::write_err;

// ============================================================================
// Phase D (#906) — privileged-only FullAccess host callbacks.
// Each callback validates the `scope_token` via `with_full_scope_or_err`
// (resolving the bound `Arc<GpuContext>` from the escalate-scope registry)
// before dispatching to the resolved context.
// ============================================================================

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_wait_device_idle(
    scope_token: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_wait_device_idle",
        || -> i32 {
            let result = with_full_scope_or_err(
                scope_token,
                "wait_device_idle",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.wait_device_idle(),
            );
            match result {
                Some(Ok(())) => 0,
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_acquire_output_texture(
    scope_token: *const c_void,
    width: u32,
    height: u32,
    format_raw: u32,
    out_id_buf: *mut u8,
    out_id_cap: usize,
    out_id_len: *mut usize,
    out_texture: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_acquire_output_texture",
        || -> i32 {
            if out_texture.is_null() || out_id_buf.is_null() || out_id_len.is_null() {
                write_err(
                    "acquire_output_texture: null out_texture / out_id_buf / out_id_len",
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
                            "acquire_output_texture: invalid format_raw {}",
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
                "acquire_output_texture",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.acquire_output_texture(width, height, format),
            );
            match result {
                Some(Ok((id, texture))) => {
                    let id_bytes = id.as_bytes();
                    if id_bytes.len() > out_id_cap {
                        write_err(
                            "acquire_output_texture: surface id buffer too small",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            id_bytes.as_ptr(),
                            out_id_buf,
                            id_bytes.len(),
                        );
                        std::ptr::write(out_id_len, id_bytes.len());
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
                None => 1,
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_upload_pixel_buffer_as_texture(
    scope_token: *const c_void,
    surface_id_ptr: *const u8,
    surface_id_len: usize,
    pixel_buffer: *const c_void,
    width: u32,
    height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_upload_pixel_buffer_as_texture",
        || -> i32 {
            if surface_id_ptr.is_null() || pixel_buffer.is_null() {
                write_err(
                    "upload_pixel_buffer_as_texture: null surface_id / pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let id_slice =
                unsafe { std::slice::from_raw_parts(surface_id_ptr, surface_id_len) };
            let surface_id = match std::str::from_utf8(id_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!(
                            "upload_pixel_buffer_as_texture: surface_id not UTF-8: {e}"
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // SAFETY: pixel_buffer is a borrowed `*const PixelBuffer`
            // pointer from the cdylib; valid for the duration of the call.
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let result = with_full_scope_or_err(
                scope_token,
                "upload_pixel_buffer_as_texture",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.upload_pixel_buffer_as_texture(surface_id, pb, width, height),
            );
            match result {
                Some(Ok(())) => 0,
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_upload_pixel_buffer_as_texture(
    _scope_token: *const c_void,
    _surface_id_ptr: *const u8,
    _surface_id_len: usize,
    _pixel_buffer: *const c_void,
    _width: u32,
    _height: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "upload_pixel_buffer_as_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_color_converter(
    scope_token: *const c_void,
    src_format_raw: u32,
    dst_format_raw: u32,
    out_converter: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_color_converter",
        || -> i32 {
            if out_converter.is_null() {
                write_err(
                    "color_converter: null out_converter",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let src = match pixel_format_from_raw(src_format_raw) {
                Some(f) => f,
                None => {
                    write_err(
                        &format!(
                            "color_converter: invalid src_format_raw {}",
                            src_format_raw
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let dst = match pixel_format_from_raw(dst_format_raw) {
                Some(f) => f,
                None => {
                    write_err(
                        &format!(
                            "color_converter: invalid dst_format_raw {}",
                            dst_format_raw
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
                "color_converter",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.color_converter(src, dst),
            );
            match result {
                Some(Ok(converter)) => {
                    // `converter` is the β-shape; its `handle` is the
                    // `Arc::into_raw(Arc<RhiColorConverterInner>)` pointer.
                    let raw = converter.handle;
                    std::mem::forget(converter);
                    unsafe { std::ptr::write(out_converter, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_color_converter(
    _scope_token: *const c_void,
    _src_format_raw: u32,
    _dst_format_raw: u32,
    _out_converter: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "color_converter: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_command_recorder(
    scope_token: *const c_void,
    label_ptr: *const u8,
    label_len: usize,
    out_recorder: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_command_recorder",
        || -> i32 {
            if out_recorder.is_null() || label_ptr.is_null() {
                write_err(
                    "create_command_recorder: null label_ptr / out_recorder",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_slice =
                unsafe { std::slice::from_raw_parts(label_ptr, label_len) };
            let label = match std::str::from_utf8(label_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!("create_command_recorder: label not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "create_command_recorder",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_command_recorder(label),
            );
            match result {
                Some(Ok(recorder)) => {
                    // SAFETY: `recorder` is the β-shape — a
                    // `#[repr(C)] { handle: *const c_void, vtable: *const VTable }`
                    // 16-byte POD. Layout is byte-identical
                    // by `#[repr(C)]` invariant, not by rustc-version
                    // coupling. The cdylib reads the bits via
                    // `MaybeUninit::assume_init`; its `Drop` later
                    // dispatches through the vtable's
                    // `drop_command_recorder` slot which runs
                    // `Box::from_raw + drop` host-side.
                    unsafe {
                        std::ptr::write(
                            out_recorder as *mut crate::vulkan::rhi::RhiCommandRecorder,
                            recorder,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_command_recorder(
    _scope_token: *const c_void,
    _label_ptr: *const u8,
    _label_len: usize,
    _out_recorder: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_command_recorder: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_build_triangles_blas(
    scope_token: *const c_void,
    label_ptr: *const u8,
    label_len: usize,
    vertices_ptr: *const f32,
    vertices_len: usize,
    indices_ptr: *const u32,
    indices_len: usize,
    out_blas: *mut *const c_void,
    out_device_address: *mut u64,
    out_storage_size: *mut u64,
    out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_build_triangles_blas",
        || -> i32 {
            if out_blas.is_null()
                || label_ptr.is_null()
                || out_device_address.is_null()
                || out_storage_size.is_null()
                || out_kind.is_null()
            {
                write_err(
                    "build_triangles_blas: null label_ptr / out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_slice =
                unsafe { std::slice::from_raw_parts(label_ptr, label_len) };
            let label = match std::str::from_utf8(label_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!("build_triangles_blas: label not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let vertices: &[f32] = if vertices_len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(vertices_ptr, vertices_len) }
            };
            let indices: &[u32] = if indices_len == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(indices_ptr, indices_len) }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "build_triangles_blas",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.build_triangles_blas(label, vertices, indices),
            );
            match result {
                Some(Ok(blas)) => {
                    // `blas` is the β-shape — its `handle` is already
                    // `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`-shaped
                    // and its cached POD fields were populated by
                    // `VulkanAccelerationStructure::from_arc_into_raw`
                    // (host-mode mint path). Write them through the
                    // out-params so the cdylib's β-shape carries the
                    // real values instead of placeholder zeros. Forget
                    // the β-shape to keep the Arc strong count bumped;
                    // cdylib reconstructs its own β-shape from the
                    // handle + vtable + cached PODs.
                    let raw = blas.handle;
                    let device_address = blas.cached_device_address;
                    let storage_size = blas.cached_storage_size;
                    let kind = blas.cached_kind;
                    std::mem::forget(blas);
                    unsafe {
                        std::ptr::write(out_blas, raw);
                        std::ptr::write(out_device_address, device_address);
                        std::ptr::write(out_storage_size, storage_size);
                        std::ptr::write(out_kind, kind);
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_build_triangles_blas(
    _scope_token: *const c_void,
    _label_ptr: *const u8,
    _label_len: usize,
    _vertices_ptr: *const f32,
    _vertices_len: usize,
    _indices_ptr: *const u32,
    _indices_len: usize,
    _out_blas: *mut *const c_void,
    _out_device_address: *mut u64,
    _out_storage_size: *mut u64,
    _out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "build_triangles_blas: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_build_tlas(
    scope_token: *const c_void,
    label_ptr: *const u8,
    label_len: usize,
    instances_ptr: *const c_void,
    instances_len: usize,
    out_tlas: *mut *const c_void,
    out_device_address: *mut u64,
    out_storage_size: *mut u64,
    out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_build_tlas",
        || -> i32 {
            if out_tlas.is_null()
                || label_ptr.is_null()
                || out_device_address.is_null()
                || out_storage_size.is_null()
                || out_kind.is_null()
            {
                write_err(
                    "build_tlas: null label_ptr / out-parameter pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let label_slice =
                unsafe { std::slice::from_raw_parts(label_ptr, label_len) };
            let label = match std::str::from_utf8(label_slice) {
                Ok(s) => s,
                Err(e) => {
                    write_err(
                        &format!("build_tlas: label not UTF-8: {e}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let instances: &[crate::vulkan::rhi::TlasInstanceDesc] = if instances_len
                == 0
            {
                &[]
            } else {
                // SAFETY: `instances_ptr` is `*const TlasInstanceDesc`
                // from the cdylib; layout is byte-identical under
                // rustc-version coupling. The slice is borrowed for
                // the call's duration.
                unsafe {
                    std::slice::from_raw_parts(
                        instances_ptr as *const crate::vulkan::rhi::TlasInstanceDesc,
                        instances_len,
                    )
                }
            };
            let result = with_full_scope_or_err(
                scope_token,
                "build_tlas",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.build_tlas(label, instances),
            );
            match result {
                Some(Ok(tlas)) => {
                    // Same shape as `host_gpu_full_build_triangles_blas`:
                    // the β-shape's cached PODs are real (populated by
                    // `from_arc_into_raw` host-side); write them
                    // through the out-params so the cdylib's reassembled
                    // β-shape carries real values.
                    let raw = tlas.handle;
                    let device_address = tlas.cached_device_address;
                    let storage_size = tlas.cached_storage_size;
                    let kind = tlas.cached_kind;
                    std::mem::forget(tlas);
                    unsafe {
                        std::ptr::write(out_tlas, raw);
                        std::ptr::write(out_device_address, device_address);
                        std::ptr::write(out_storage_size, storage_size);
                        std::ptr::write(out_kind, kind);
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_build_tlas(
    _scope_token: *const c_void,
    _label_ptr: *const u8,
    _label_len: usize,
    _instances_ptr: *const c_void,
    _instances_len: usize,
    _out_tlas: *mut *const c_void,
    _out_device_address: *mut u64,
    _out_storage_size: *mut u64,
    _out_kind: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "build_tlas: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_supports_ray_tracing_pipeline(
    scope_token: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_supports_ray_tracing_pipeline",
        || -> i32 {
            let result = with_full_scope_or_err(
                scope_token,
                "supports_ray_tracing_pipeline",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| Ok::<bool, crate::core::Error>(gpu.supports_ray_tracing_pipeline()),
            );
            match result {
                Some(Ok(true)) => 1,
                Some(Ok(false)) => 0,
                Some(Err(_)) | None => -1,
            }
        },
        -1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_supports_ray_tracing_pipeline(
    _scope_token: *const c_void,
    _err_buf: *mut u8,
    _err_buf_cap: usize,
    _err_len: *mut usize,
) -> i32 {
    0
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_gpu_capabilities(
    scope_token: *const c_void,
    out_caps: *mut streamlib_plugin_abi::GpuCapabilitiesRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_gpu_capabilities",
        || -> i32 {
            if out_caps.is_null() {
                write_err(
                    "gpu_capabilities: null out_caps pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                scope_token,
                "gpu_capabilities",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| Ok::<_, crate::core::Error>(gpu.gpu_capabilities()),
            );
            match result {
                Some(Ok(snapshot)) => {
                    let mut repr = streamlib_plugin_abi::GpuCapabilitiesRepr {
                        device_name: [0u8; 256],
                        device_name_len: 0,
                        supports_external_memory: u8::from(
                            snapshot.supports_external_memory,
                        ),
                        supports_cross_device_dma_buf_probe: u8::from(
                            snapshot.supports_cross_device_dma_buf_probe,
                        ),
                        supports_ray_tracing_pipeline: u8::from(
                            snapshot.supports_ray_tracing_pipeline,
                        ),
                        _reserved_padding: 0,
                    };
                    let bytes = snapshot.device_name.as_bytes();
                    let n = bytes.len().min(repr.device_name.len());
                    repr.device_name[..n].copy_from_slice(&bytes[..n]);
                    repr.device_name_len = n as u32;
                    unsafe { std::ptr::write(out_caps, repr) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_gpu_capabilities(
    _scope_token: *const c_void,
    _out_caps: *mut streamlib_plugin_abi::GpuCapabilitiesRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "gpu_capabilities: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_timeline_semaphore(
    scope_token: *const c_void,
    initial_value: u64,
    out_handle: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_timeline_semaphore",
        || -> i32 {
            if out_handle.is_null() {
                write_err(
                    "create_timeline_semaphore: null out_handle pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                scope_token,
                "create_timeline_semaphore",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_timeline_semaphore(initial_value),
            );
            match result {
                Some(Ok(arc)) => {
                    let raw = Arc::into_raw(arc) as *const c_void;
                    unsafe { std::ptr::write(out_handle, raw) };
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_timeline_semaphore(
    _scope_token: *const c_void,
    _initial_value: u64,
    _out_handle: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "create_timeline_semaphore: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_import_dma_buf_storage_buffer(
    scope_token: *const c_void,
    fd: i32,
    byte_size: u64,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_import_dma_buf_storage_buffer",
        || -> i32 {
            if out_buffer.is_null() {
                write_err(
                    "import_dma_buf_storage_buffer: null out_buffer pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                scope_token,
                "import_dma_buf_storage_buffer",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.import_dma_buf_storage_buffer(fd, byte_size),
            );
            match result {
                Some(Ok(buf)) => {
                    unsafe {
                        std::ptr::write(
                            out_buffer as *mut crate::core::rhi::StorageBuffer,
                            buf,
                        );
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_import_dma_buf_storage_buffer(
    _scope_token: *const c_void,
    _fd: i32,
    _byte_size: u64,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "import_dma_buf_storage_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_check_in_surface(
    scope_token: *const c_void,
    pixel_buffer: *const c_void,
    out_id_buf: *mut u8,
    out_id_cap: usize,
    out_id_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_check_in_surface",
        || -> i32 {
            if pixel_buffer.is_null() || out_id_buf.is_null() || out_id_len.is_null() {
                write_err(
                    "check_in_surface: null pixel_buffer / out_id_buf / out_id_len",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: pixel_buffer is borrowed from the cdylib for the
            // duration of the call.
            let pb = unsafe { &*(pixel_buffer as *const crate::core::rhi::PixelBuffer) };
            let result = with_full_scope_or_err(
                scope_token,
                "check_in_surface",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.check_in_surface(pb),
            );
            match result {
                Some(Ok(id)) => {
                    let id_bytes = id.as_bytes();
                    if id_bytes.len() > out_id_cap {
                        write_err(
                            "check_in_surface: surface id buffer too small",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    }
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            id_bytes.as_ptr(),
                            out_id_buf,
                            id_bytes.len(),
                        );
                        std::ptr::write(out_id_len, id_bytes.len());
                    }
                    0
                }
                Some(Err(e)) => {
                    write_err(&format!("{}", e), err_buf, err_buf_cap, err_len);
                    1
                }
                None => 1,
            }
        },
        1,
    )
}

/// Clone the host's `Arc<HostVulkanDevice>` and return the raw
/// `Arc::into_raw` pointer. Used by in-process workspace plugin cdylibs
/// (#1004 dlopen smoke fixtures for the surface adapters) that need to
/// construct a host-flavor `XxxSurfaceAdapter<HostVulkanDevice>` to
/// exercise `acquire_write` → `view_mut` → release through the cdylib
/// boundary. On null/stale token returns a null pointer.
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_host_vulkan_device_arc(
    scope_token: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_gpu_full_host_vulkan_device_arc",
        || -> *const c_void {
            let token = scope_token as u64;
            crate::core::context::escalate_scope_registry::with_scope(token, |gpu| {
                let device = gpu.device();
                let host_device =
                    crate::host_rhi::HostGpuDeviceExt::vulkan_device(device.as_ref());
                let arc = Arc::clone(host_device);
                Arc::into_raw(arc) as *const c_void
            })
            .unwrap_or(std::ptr::null())
        },
        std::ptr::null(),
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_host_vulkan_device_arc(
    _scope_token: *const c_void,
) -> *const c_void {
    std::ptr::null()
}

/// Clone the host's `Arc<HostVulkanTexture>` backing a `Texture`
/// β-shape and return the raw `Arc::into_raw` pointer. Second bridge
/// of the cdylib-side adapter-construction chain: cdylibs can't
/// reach `Texture::host_inner()` (panics in cdylib mode), so they
/// dispatch through this slot to obtain a real
/// `Arc<HostVulkanTexture>` for calls like
/// `OpenGlSurfaceAdapter::register_host_surface`. On null
/// `texture_handle` returns a null pointer.
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_host_vulkan_texture_arc(
    texture_handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_gpu_full_host_vulkan_texture_arc",
        || -> *const c_void {
            if texture_handle.is_null() {
                return std::ptr::null();
            }
            // SAFETY: `texture_handle` is the same opaque
            // `Arc::into_raw(Arc<TextureInner>)` pointer cached on the
            // `Texture` β-shape's `handle` field (see
            // `Texture::from_arc_into_raw`). The leaked strong count
            // keeps the `TextureInner` alive at least until the
            // β-shape's `Drop` runs. We borrow without taking
            // ownership, clone the inner `Arc<HostVulkanTexture>`, and
            // return its raw pointer with the strong count bumped by 1.
            let inner = unsafe {
                &*(texture_handle
                    as *const crate::core::rhi::texture::TextureInner)
            };
            let arc = Arc::clone(&inner.inner);
            Arc::into_raw(arc) as *const c_void
        },
        std::ptr::null(),
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_host_vulkan_texture_arc(
    _texture_handle: *const c_void,
) -> *const c_void {
    std::ptr::null()
}

