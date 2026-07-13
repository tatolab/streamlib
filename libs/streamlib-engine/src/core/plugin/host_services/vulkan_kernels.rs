// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `VulkanGraphicsKernelMethodsVTable` +
//! `VulkanRayTracingKernelMethodsVTable` callbacks + static vtables +
//! accessors (issues #951, #953).
//!
//! The two kernel families share a single submodule because the
//! Linux callback bodies and the non-Linux platform stubs are
//! interleaved in the source layout. Both share the same shape:
//! reconstruct the kernel borrow from `Arc::into_raw(Arc<...Inner>)`,
//! run the inner method, convert `Result<()>` into the plugin ABI's
//! `i32 + err_buf` shape, with `run_host_extern_c` catching any
//! panic in the inner method.

use std::ffi::c_void;
use std::sync::Arc;

use super::host_callbacks;
use super::run_host_extern_c;
#[cfg(target_os = "linux")]
use super::shared::borrow::{
    make_acceleration_structure_borrow, make_index_buffer_borrow, make_pixel_buffer_borrow,
    make_storage_buffer_borrow, make_texture_borrow, make_uniform_buffer_borrow,
    make_vertex_buffer_borrow,
};
use super::shared::wire::{slice_from_raw, write_err};

// ---- VulkanGraphicsKernelMethodsVTable wrappers (#951) ---------------------
//
// Each wrapper reconstructs the kernel borrow from the raw `Arc`
// handle the cdylib passes (`Arc::into_raw(Arc<VulkanGraphicsKernelInner>)`
// per the PluginAbiObject's `from_arc_into_raw`), runs the inner method,
// and converts the `Result<()>` into the plugin ABI's `i32 + err_buf`
// shape. All bodies are wrapped in `run_host_extern_c` so a panic
// in the inner method becomes a non-zero return.
//
// Buffer / texture borrow reconstruction reuses the
// `make_*_buffer_borrow` / `make_texture_borrow` helpers from the
// compute-kernel section above — same `ManuallyDrop`-wrapped
// plugin-handle pattern, same "cached PODs are never read"
// invariant. See the comment block above
// `make_pixel_buffer_borrow` for the load-bearing details.

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<VulkanGraphicsKernelInner>)`. The leaked
/// strong count keeps the kernel alive for the call's duration.
#[cfg(target_os = "linux")]
unsafe fn handle_as_graphics_kernel(
    handle: *const c_void,
) -> Option<&'static crate::vulkan::rhi::VulkanGraphicsKernelInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::vulkan::rhi::VulkanGraphicsKernelInner) })
}

#[cfg(target_os = "linux")]
fn index_type_from_repr(raw: u32) -> Option<crate::core::rhi::IndexType> {
    match raw {
        0 => Some(crate::core::rhi::IndexType::Uint16),
        1 => Some(crate::core::rhi::IndexType::Uint32),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_storage_buffer_pixel(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_storage_buffer_pixel",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "set_storage_buffer_pixel: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer_handle.is_null() {
                write_err(
                    "set_storage_buffer_pixel: null pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_pixel_buffer_borrow(pixel_buffer_handle);
            match kernel.set_storage_buffer(frame_index, binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_buffer_pixel: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_storage_buffer_storage(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_storage_buffer_storage",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "set_storage_buffer_storage: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if storage_buffer_handle.is_null() {
                write_err(
                    "set_storage_buffer_storage: null storage_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_storage_buffer_borrow(storage_buffer_handle);
            match kernel.set_storage_buffer(frame_index, binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_buffer_storage: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_uniform_buffer(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_uniform_buffer",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "set_uniform_buffer: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if uniform_buffer_handle.is_null() {
                write_err(
                    "set_uniform_buffer: null uniform_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_uniform_buffer_borrow(uniform_buffer_handle);
            match kernel.set_uniform_buffer(frame_index, binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_uniform_buffer: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_sampled_texture(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_sampled_texture",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "set_sampled_texture: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "set_sampled_texture: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_texture_borrow(texture_handle);
            match kernel.set_sampled_texture(frame_index, binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_sampled_texture: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_storage_image(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_storage_image",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "set_storage_image: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "set_storage_image: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_texture_borrow(texture_handle);
            match kernel.set_storage_image(frame_index, binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_image: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_vertex_buffer(
    kernel_handle: *const c_void,
    frame_index: u32,
    binding: u32,
    vertex_buffer_handle: *const c_void,
    offset: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_vertex_buffer",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "set_vertex_buffer: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if vertex_buffer_handle.is_null() {
                write_err(
                    "set_vertex_buffer: null vertex_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_vertex_buffer_borrow(vertex_buffer_handle);
            match kernel.set_vertex_buffer(frame_index, binding, &*borrow, offset) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_vertex_buffer: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_index_buffer(
    kernel_handle: *const c_void,
    frame_index: u32,
    index_buffer_handle: *const c_void,
    offset: u64,
    index_type_raw: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_index_buffer",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "set_index_buffer: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if index_buffer_handle.is_null() {
                write_err(
                    "set_index_buffer: null index_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let Some(index_type) = index_type_from_repr(index_type_raw) else {
                write_err(
                    &format!("set_index_buffer: unknown index_type discriminant {index_type_raw}"),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let borrow = make_index_buffer_borrow(index_buffer_handle);
            match kernel.set_index_buffer(frame_index, &*borrow, offset, index_type) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_index_buffer: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_set_push_constants(
    kernel_handle: *const c_void,
    frame_index: u32,
    bytes_ptr: *const u8,
    bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_set_push_constants",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "set_push_constants: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if bytes_ptr.is_null() && bytes_len != 0 {
                write_err(
                    "set_push_constants: null bytes_ptr with non-zero len",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let bytes = if bytes_len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(bytes_ptr, bytes_len) }
            };
            match kernel.set_push_constants(frame_index, bytes) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_push_constants: {e}"),
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

#[cfg(target_os = "linux")]
fn draw_call_from_repr(repr: &streamlib_plugin_abi::DrawCallRepr) -> crate::core::rhi::DrawCall {
    crate::core::rhi::DrawCall {
        vertex_count: repr.vertex_count,
        instance_count: repr.instance_count,
        first_vertex: repr.first_vertex,
        first_instance: repr.first_instance,
        viewport: if repr.viewport_present != 0 {
            Some(crate::core::rhi::Viewport {
                x: repr.viewport.x,
                y: repr.viewport.y,
                width: repr.viewport.width,
                height: repr.viewport.height,
                min_depth: repr.viewport.min_depth,
                max_depth: repr.viewport.max_depth,
            })
        } else {
            None
        },
        scissor: if repr.scissor_present != 0 {
            Some(crate::core::rhi::ScissorRect {
                x: repr.scissor.x,
                y: repr.scissor.y,
                width: repr.scissor.width,
                height: repr.scissor.height,
            })
        } else {
            None
        },
    }
}

#[cfg(target_os = "linux")]
fn draw_indexed_call_from_repr(
    repr: &streamlib_plugin_abi::DrawIndexedCallRepr,
) -> crate::core::rhi::DrawIndexedCall {
    crate::core::rhi::DrawIndexedCall {
        index_count: repr.index_count,
        instance_count: repr.instance_count,
        first_index: repr.first_index,
        vertex_offset: repr.vertex_offset,
        first_instance: repr.first_instance,
        viewport: if repr.viewport_present != 0 {
            Some(crate::core::rhi::Viewport {
                x: repr.viewport.x,
                y: repr.viewport.y,
                width: repr.viewport.width,
                height: repr.viewport.height,
                min_depth: repr.viewport.min_depth,
                max_depth: repr.viewport.max_depth,
            })
        } else {
            None
        },
        scissor: if repr.scissor_present != 0 {
            Some(crate::core::rhi::ScissorRect {
                x: repr.scissor.x,
                y: repr.scissor.y,
                width: repr.scissor.width,
                height: repr.scissor.height,
            })
        } else {
            None
        },
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_offscreen_render(
    kernel_handle: *const c_void,
    frame_index: u32,
    color_texture_handles: *const *const c_void,
    color_clear_present: *const u32,
    color_clear_values: *const [f32; 4],
    target_count: usize,
    extent_width: u32,
    extent_height: u32,
    draw: *const streamlib_plugin_abi::OffscreenDrawRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_offscreen_render",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "offscreen_render: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if draw.is_null() {
                write_err(
                    "offscreen_render: null draw pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if target_count != 0
                && (color_texture_handles.is_null()
                    || color_clear_present.is_null()
                    || color_clear_values.is_null())
            {
                write_err(
                    "offscreen_render: null parallel-array pointer with non-zero target_count",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let handles = if target_count == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(color_texture_handles, target_count) }
            };
            let present_flags = if target_count == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(color_clear_present, target_count) }
            };
            let clear_values = if target_count == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(color_clear_values, target_count) }
            };
            // Reconstruct ManuallyDrop-wrapped Texture borrows for each
            // attachment. The Vec keeps the wrappers alive for the
            // duration of the inner call; OffscreenColorTarget then
            // borrows into those wrappers.
            let mut texture_borrows: Vec<std::mem::ManuallyDrop<crate::core::rhi::Texture>> =
                Vec::with_capacity(target_count);
            for (i, &handle) in handles.iter().enumerate() {
                if handle.is_null() {
                    write_err(
                        &format!("offscreen_render: null texture handle at color target {i}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
                texture_borrows.push(make_texture_borrow(handle));
            }
            let targets: Vec<crate::vulkan::rhi::OffscreenColorTarget<'_>> = texture_borrows
                .iter()
                .enumerate()
                .map(|(i, borrow)| {
                    let clear_color = if present_flags[i] != 0 {
                        Some(clear_values[i])
                    } else {
                        None
                    };
                    crate::vulkan::rhi::OffscreenColorTarget {
                        texture: &**borrow,
                        clear_color,
                    }
                })
                .collect();
            let draw_repr = unsafe { &*draw };
            let inner_draw = match draw_repr.kind {
                k if k == streamlib_plugin_abi::OffscreenDrawKindRepr::Draw as u32 => {
                    crate::vulkan::rhi::OffscreenDraw::Draw(draw_call_from_repr(
                        &draw_repr.draw_call,
                    ))
                }
                k if k == streamlib_plugin_abi::OffscreenDrawKindRepr::DrawIndexed as u32 => {
                    crate::vulkan::rhi::OffscreenDraw::DrawIndexed(draw_indexed_call_from_repr(
                        &draw_repr.draw_indexed_call,
                    ))
                }
                other => {
                    write_err(
                        &format!("offscreen_render: unknown draw kind discriminant {other}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match kernel.offscreen_render(
                frame_index,
                &targets,
                (extent_width, extent_height),
                inner_draw,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("offscreen_render: {e}"),
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

// ---- VulkanRayTracingKernelMethodsVTable wrappers (#953) -------------------
//
// Each wrapper reconstructs the kernel borrow from the raw `Arc`
// handle the cdylib passes (`Arc::into_raw(Arc<VulkanRayTracingKernelInner>)`
// per the PluginAbiObject's `from_arc_into_raw`), runs the inner method,
// and converts the `Result<()>` into the plugin ABI's `i32 + err_buf`
// shape. All bodies are wrapped in `run_host_extern_c` so a panic
// in the inner method becomes a non-zero return.
//
// Buffer / texture borrow reconstruction reuses the
// `make_*_buffer_borrow` / `make_texture_borrow` helpers from the
// compute-kernel section above — same `ManuallyDrop`-wrapped
// plugin-handle pattern, same "cached PODs are never read"
// invariant. See the comment block above
// `make_pixel_buffer_borrow` for the load-bearing details.
//
// The AS-binding wrapper reconstructs an AS borrow via
// `make_acceleration_structure_borrow` — same `ManuallyDrop` shape
// as the buffer/texture helpers. The PluginAbiObject's `kind()` /
// `device_address()` / `storage_size()` getters read the cached
// fields on the borrow directly (no vtable dispatch); the helper
// populates those fields at construction time from the host-internal
// `Inner`, so the inner kernel's `set_acceleration_structure` reads
// the real values rather than the placeholder zeros that would
// trip the kernel's `TopLevel` check. `vk_handle()` stays host-only
// (vulkanalia handle, no cdylib path) and is only called from the
// host wrapper here, after the kind check passes.

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<VulkanRayTracingKernelInner>)`. The leaked
/// strong count keeps the kernel alive for the call's duration.
#[cfg(target_os = "linux")]
unsafe fn handle_as_ray_tracing_kernel(
    handle: *const c_void,
) -> Option<&'static crate::vulkan::rhi::VulkanRayTracingKernelInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::vulkan::rhi::VulkanRayTracingKernelInner) })
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_acceleration_structure(
    kernel_handle: *const c_void,
    binding: u32,
    acceleration_structure_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_acceleration_structure",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) }) else {
                write_err(
                    "set_acceleration_structure: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if acceleration_structure_handle.is_null() {
                write_err(
                    "set_acceleration_structure: null acceleration_structure handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_acceleration_structure_borrow(acceleration_structure_handle);
            match kernel.set_acceleration_structure(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_acceleration_structure: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_buffer_pixel(
    kernel_handle: *const c_void,
    binding: u32,
    pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_storage_buffer_pixel",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) }) else {
                write_err(
                    "set_storage_buffer_pixel: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if pixel_buffer_handle.is_null() {
                write_err(
                    "set_storage_buffer_pixel: null pixel_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_pixel_buffer_borrow(pixel_buffer_handle);
            match kernel.set_storage_buffer(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_buffer_pixel: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_buffer_storage(
    kernel_handle: *const c_void,
    binding: u32,
    storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_storage_buffer_storage",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) }) else {
                write_err(
                    "set_storage_buffer_storage: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if storage_buffer_handle.is_null() {
                write_err(
                    "set_storage_buffer_storage: null storage_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_storage_buffer_borrow(storage_buffer_handle);
            match kernel.set_storage_buffer(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_buffer_storage: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_uniform_buffer(
    kernel_handle: *const c_void,
    binding: u32,
    uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_uniform_buffer",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) }) else {
                write_err(
                    "set_uniform_buffer: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if uniform_buffer_handle.is_null() {
                write_err(
                    "set_uniform_buffer: null uniform_buffer handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_uniform_buffer_borrow(uniform_buffer_handle);
            match kernel.set_uniform_buffer(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_uniform_buffer: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_sampled_texture(
    kernel_handle: *const c_void,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_sampled_texture",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) }) else {
                write_err(
                    "set_sampled_texture: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "set_sampled_texture: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_texture_borrow(texture_handle);
            match kernel.set_sampled_texture(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_sampled_texture: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_image(
    kernel_handle: *const c_void,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_storage_image",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) }) else {
                write_err(
                    "set_storage_image: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if texture_handle.is_null() {
                write_err(
                    "set_storage_image: null texture handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let borrow = make_texture_borrow(texture_handle);
            match kernel.set_storage_image(binding, &*borrow) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_image: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_set_push_constants(
    kernel_handle: *const c_void,
    bytes_ptr: *const u8,
    bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_set_push_constants",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) }) else {
                write_err(
                    "set_push_constants: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if bytes_ptr.is_null() && bytes_len != 0 {
                write_err(
                    "set_push_constants: null bytes_ptr with non-zero len",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let bytes = if bytes_len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(bytes_ptr, bytes_len) }
            };
            match kernel.set_push_constants(bytes) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_push_constants: {e}"),
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

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_trace_rays(
    kernel_handle: *const c_void,
    width: u32,
    height: u32,
    depth: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_trace_rays",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) }) else {
                write_err(
                    "trace_rays: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel.trace_rays(width, height, depth) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("trace_rays: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// ---- Non-Linux platform stubs (vtable layout stays unconditional) ----------

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_storage_buffer_pixel(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_buffer_pixel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_storage_buffer_storage(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_buffer_storage: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_uniform_buffer(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_uniform_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_sampled_texture(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_sampled_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_storage_image(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_image: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_vertex_buffer(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _binding: u32,
    _vertex_buffer_handle: *const c_void,
    _offset: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_vertex_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_index_buffer(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _index_buffer_handle: *const c_void,
    _offset: u64,
    _index_type_raw: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_index_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_set_push_constants(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _bytes_ptr: *const u8,
    _bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_push_constants: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_offscreen_render(
    _kernel_handle: *const c_void,
    _frame_index: u32,
    _color_texture_handles: *const *const c_void,
    _color_clear_present: *const u32,
    _color_clear_values: *const [f32; 4],
    _target_count: usize,
    _extent_width: u32,
    _extent_height: u32,
    _draw: *const streamlib_plugin_abi::OffscreenDrawRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "offscreen_render: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Read the graphics kernel's declared bindings into a caller-provided
/// `[GraphicsBindingSpecRepr]` buffer. v3 (introspection).
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_bindings(
    kernel_handle: *const c_void,
    out_specs_buf: *mut streamlib_plugin_abi::GraphicsBindingSpecRepr,
    out_specs_cap: usize,
    out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_bindings",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "bindings: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_specs_len.is_null() {
                write_err(
                    "bindings: null out_specs_len pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let bindings = kernel.bindings();
            let actual = bindings.len();
            unsafe { std::ptr::write(out_specs_len, actual) };
            if out_specs_cap < actual {
                return 2;
            }
            if !out_specs_buf.is_null() {
                for (i, spec) in bindings.iter().enumerate() {
                    let repr = streamlib_plugin_abi::GraphicsBindingSpecRepr::from(spec);
                    unsafe { std::ptr::write(out_specs_buf.add(i), repr) };
                }
            } else if actual > 0 {
                write_err(
                    "bindings: out_specs_buf is null but kernel has bindings",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            0
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_graphics_kernel_bindings(
    _kernel_handle: *const c_void,
    _out_specs_buf: *mut streamlib_plugin_abi::GraphicsBindingSpecRepr,
    _out_specs_cap: usize,
    _out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "bindings: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// v4 — record bind + push + draw into a caller-owned
/// `vk::CommandBuffer`. The cdylib mints + manages the command
/// buffer; this callback reconstructs the handle and forwards to
/// `VulkanGraphicsKernelInner::cmd_bind_and_draw_raw`, which does
/// the `vk::CommandBuffer::from_raw` conversion under the engine's
/// canonical vulkanalia-allowlist scope.
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_cmd_bind_and_draw(
    kernel_handle: *const c_void,
    command_buffer_handle: u64,
    frame_index: u32,
    draw: *const streamlib_plugin_abi::DrawCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_cmd_bind_and_draw",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "cmd_bind_and_draw: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if draw.is_null() {
                write_err(
                    "cmd_bind_and_draw: null draw pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let draw_repr = unsafe { &*draw };
            let inner_draw = draw_call_from_repr(draw_repr);
            match kernel.cmd_bind_and_draw_raw(command_buffer_handle, frame_index, &inner_draw) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("cmd_bind_and_draw: {e}"),
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
unsafe extern "C" fn host_graphics_kernel_cmd_bind_and_draw(
    _kernel_handle: *const c_void,
    _command_buffer_handle: u64,
    _frame_index: u32,
    _draw: *const streamlib_plugin_abi::DrawCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "cmd_bind_and_draw: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// v4 — indexed variant of [`host_graphics_kernel_cmd_bind_and_draw`].
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_graphics_kernel_cmd_bind_and_draw_indexed(
    kernel_handle: *const c_void,
    command_buffer_handle: u64,
    frame_index: u32,
    draw: *const streamlib_plugin_abi::DrawIndexedCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_graphics_kernel_cmd_bind_and_draw_indexed",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_graphics_kernel(kernel_handle) }) else {
                write_err(
                    "cmd_bind_and_draw_indexed: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if draw.is_null() {
                write_err(
                    "cmd_bind_and_draw_indexed: null draw pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let draw_repr = unsafe { &*draw };
            let inner_draw = draw_indexed_call_from_repr(draw_repr);
            match kernel.cmd_bind_and_draw_indexed_raw(
                command_buffer_handle,
                frame_index,
                &inner_draw,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("cmd_bind_and_draw_indexed: {e}"),
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
unsafe extern "C" fn host_graphics_kernel_cmd_bind_and_draw_indexed(
    _kernel_handle: *const c_void,
    _command_buffer_handle: u64,
    _frame_index: u32,
    _draw: *const streamlib_plugin_abi::DrawIndexedCallRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "cmd_bind_and_draw_indexed: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

pub static HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE:
    streamlib_plugin_abi::VulkanGraphicsKernelMethodsVTable =
    streamlib_plugin_abi::VulkanGraphicsKernelMethodsVTable {
        layout_version: streamlib_plugin_abi::VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        set_storage_buffer_pixel: host_graphics_kernel_set_storage_buffer_pixel,
        set_storage_buffer_storage: host_graphics_kernel_set_storage_buffer_storage,
        set_uniform_buffer: host_graphics_kernel_set_uniform_buffer,
        set_sampled_texture: host_graphics_kernel_set_sampled_texture,
        set_storage_image: host_graphics_kernel_set_storage_image,
        set_vertex_buffer: host_graphics_kernel_set_vertex_buffer,
        set_index_buffer: host_graphics_kernel_set_index_buffer,
        set_push_constants: host_graphics_kernel_set_push_constants,
        offscreen_render: host_graphics_kernel_offscreen_render,
        bindings: host_graphics_kernel_bindings,
        cmd_bind_and_draw: host_graphics_kernel_cmd_bind_and_draw,
        cmd_bind_and_draw_indexed: host_graphics_kernel_cmd_bind_and_draw_indexed,
    };

/// Accessor for the host's static `VulkanGraphicsKernelMethodsVTable`
/// — used by `VulkanGraphicsKernel::from_arc_into_raw` to populate
/// the PluginAbiObject's `methods_vtable` field.
pub fn host_vulkan_graphics_kernel_methods_vtable()
-> *const streamlib_plugin_abi::VulkanGraphicsKernelMethodsVTable {
    // See [`host_vulkan_compute_kernel_methods_vtable`] for the routing
    // rationale — cdylib PluginAbiObject constructors must store the host's
    // vtable pointer so dispatches actually cross the plugin ABI.
    match host_callbacks() {
        Some(c) if !c.vulkan_graphics_kernel_methods_vtable.is_null() => {
            c.vulkan_graphics_kernel_methods_vtable
        }
        _ => &HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE,
    }
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_acceleration_structure(
    _kernel_handle: *const c_void,
    _binding: u32,
    _acceleration_structure_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_acceleration_structure: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_buffer_pixel(
    _kernel_handle: *const c_void,
    _binding: u32,
    _pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_buffer_pixel: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_buffer_storage(
    _kernel_handle: *const c_void,
    _binding: u32,
    _storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_buffer_storage: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_uniform_buffer(
    _kernel_handle: *const c_void,
    _binding: u32,
    _uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_uniform_buffer: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_sampled_texture(
    _kernel_handle: *const c_void,
    _binding: u32,
    _texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_sampled_texture: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_storage_image(
    _kernel_handle: *const c_void,
    _binding: u32,
    _texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_image: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_set_push_constants(
    _kernel_handle: *const c_void,
    _bytes_ptr: *const u8,
    _bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_push_constants: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_trace_rays(
    _kernel_handle: *const c_void,
    _width: u32,
    _height: u32,
    _depth: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "trace_rays: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `VulkanRayTracingKernelMethodsVTable` populated with the
/// v2 method slots (typed binding-method dispatch for the plugin
/// handle's `set_acceleration_structure` / `set_storage_buffer_pixel`
/// / `set_storage_buffer_storage` / `set_uniform_buffer` /
/// `set_sampled_texture` / `set_storage_image` surface plus the
/// primitive-argument slots `set_push_constants` / `trace_rays`).
///
/// The `bindings()` getter and the generic
/// `set_push_constants_value::<T>` stay `host_inner`-routed —
/// `Vec<RayTracingBindingSpec>` isn't `#[repr(C)]` and the generic
/// reduces to `set_push_constants` for cdylib mode.
/// Read the ray-tracing kernel's declared bindings into a caller-
/// provided `[RayTracingBindingSpecRepr]` buffer. v3 (introspection).
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_ray_tracing_kernel_bindings(
    kernel_handle: *const c_void,
    out_specs_buf: *mut streamlib_plugin_abi::RayTracingBindingSpecRepr,
    out_specs_cap: usize,
    out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_ray_tracing_kernel_bindings",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_ray_tracing_kernel(kernel_handle) }) else {
                write_err(
                    "bindings: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            if out_specs_len.is_null() {
                write_err(
                    "bindings: null out_specs_len pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let bindings = kernel.bindings();
            let actual = bindings.len();
            unsafe { std::ptr::write(out_specs_len, actual) };
            if out_specs_cap < actual {
                return 2;
            }
            if !out_specs_buf.is_null() {
                for (i, spec) in bindings.iter().enumerate() {
                    let repr = streamlib_plugin_abi::RayTracingBindingSpecRepr::from(spec);
                    unsafe { std::ptr::write(out_specs_buf.add(i), repr) };
                }
            } else if actual > 0 {
                write_err(
                    "bindings: out_specs_buf is null but kernel has bindings",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            0
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_ray_tracing_kernel_bindings(
    _kernel_handle: *const c_void,
    _out_specs_buf: *mut streamlib_plugin_abi::RayTracingBindingSpecRepr,
    _out_specs_cap: usize,
    _out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "bindings: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

pub static HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE:
    streamlib_plugin_abi::VulkanRayTracingKernelMethodsVTable =
    streamlib_plugin_abi::VulkanRayTracingKernelMethodsVTable {
        layout_version:
            streamlib_plugin_abi::VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        set_acceleration_structure: host_ray_tracing_kernel_set_acceleration_structure,
        set_storage_buffer_pixel: host_ray_tracing_kernel_set_storage_buffer_pixel,
        set_storage_buffer_storage: host_ray_tracing_kernel_set_storage_buffer_storage,
        set_uniform_buffer: host_ray_tracing_kernel_set_uniform_buffer,
        set_sampled_texture: host_ray_tracing_kernel_set_sampled_texture,
        set_storage_image: host_ray_tracing_kernel_set_storage_image,
        set_push_constants: host_ray_tracing_kernel_set_push_constants,
        trace_rays: host_ray_tracing_kernel_trace_rays,
        bindings: host_ray_tracing_kernel_bindings,
    };

/// Accessor for the host's static
/// `VulkanRayTracingKernelMethodsVTable` — used by
/// `VulkanRayTracingKernel::from_arc_into_raw` to populate the
/// PluginAbiObject's `methods_vtable` field.
pub fn host_vulkan_ray_tracing_kernel_methods_vtable()
-> *const streamlib_plugin_abi::VulkanRayTracingKernelMethodsVTable {
    &HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE
}

#[cfg(all(test, target_os = "linux"))]
mod graphics_kernel_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v2 method slots on
    //! `VulkanGraphicsKernelMethodsVTable`. Each wrapper must
    //! reject a null kernel handle before reaching any kernel-side
    //! state (i.e. before any deref) so cdylib callers get a clean
    //! error return on the wire-format path instead of UB.
    //!
    //! The null-buffer-handle / null-texture-handle guards live in
    //! the same wrappers and fire when the kernel handle is valid;
    //! they're exercised end-to-end by the graphics-kernel dlopen
    //! smoke test (which holds a real kernel).

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn set_storage_buffer_pixel_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_storage_buffer_pixel)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_storage_buffer_pixel: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_buffer_storage_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_storage_buffer_storage)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_storage_buffer_storage: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_uniform_buffer_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_uniform_buffer)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_uniform_buffer: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_sampled_texture_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_sampled_texture)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_sampled_texture: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_image_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_storage_image)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_storage_image: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_vertex_buffer_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_vertex_buffer)(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_vertex_buffer: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_index_buffer_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_index_buffer)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_index_buffer: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_push_constants_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.set_push_constants)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_push_constants: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn offscreen_render_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let draw: streamlib_plugin_abi::OffscreenDrawRepr = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.offscreen_render)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                0,
                0,
                &draw,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("offscreen_render: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    // v4 — cmd_bind_and_draw / cmd_bind_and_draw_indexed wrappers.

    #[test]
    fn cmd_bind_and_draw_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let draw: streamlib_plugin_abi::DrawCallRepr = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.cmd_bind_and_draw)(
                std::ptr::null(),
                0,
                0,
                &draw,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("cmd_bind_and_draw: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn cmd_bind_and_draw_indexed_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let draw: streamlib_plugin_abi::DrawIndexedCallRepr = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            (HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.cmd_bind_and_draw_indexed)(
                std::ptr::null(),
                0,
                0,
                &draw,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("cmd_bind_and_draw_indexed: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod ray_tracing_kernel_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v2 method slots on
    //! `VulkanRayTracingKernelMethodsVTable`. Each wrapper must
    //! reject a null kernel handle before reaching any kernel-side
    //! state (i.e. before any deref) so cdylib callers get a clean
    //! error return on the wire-format path instead of UB.
    //!
    //! The null-AS-handle / null-buffer-handle / null-texture-handle
    //! guards live in the same wrappers and fire when the kernel
    //! handle is valid; they're exercised end-to-end by the
    //! ray-tracing-kernel dlopen smoke test (which holds a real
    //! kernel).

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn set_acceleration_structure_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_acceleration_structure)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_acceleration_structure: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_buffer_pixel_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_storage_buffer_pixel)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_storage_buffer_pixel: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_buffer_storage_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_storage_buffer_storage)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_storage_buffer_storage: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_uniform_buffer_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_uniform_buffer)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_uniform_buffer: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_sampled_texture_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_sampled_texture)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_sampled_texture: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_image_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_storage_image)(
                std::ptr::null(),
                0,
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_storage_image: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_push_constants_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.set_push_constants)(
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
            err_buf_as_str(&buf, len).contains("set_push_constants: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn trace_rays_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.trace_rays)(
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
            err_buf_as_str(&buf, len).contains("trace_rays: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}
