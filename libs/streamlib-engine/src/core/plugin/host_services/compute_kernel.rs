// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `VulkanComputeKernelMethodsVTable` callbacks + static
//! vtable + accessor (issue #949).
//!
//! Each wrapper reconstructs the kernel borrow from the raw `Arc`
//! handle the cdylib passes (`Arc::into_raw(Arc<VulkanComputeKernelInner>)`
//! per the β-shape's `from_arc_into_raw`), runs the inner method,
//! and converts the `Result<()>` into the FFI's `i32 + err_buf`
//! shape. All bodies wrapped in `run_host_extern_c` so a panic in
//! the inner method becomes a non-zero return.

use std::ffi::c_void;
use std::sync::Arc;

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::borrow::{
    make_acceleration_structure_borrow, make_compute_kernel_borrow,
    make_index_buffer_borrow, make_pixel_buffer_borrow, make_storage_buffer_borrow,
    make_texture_borrow, make_uniform_buffer_borrow, make_vertex_buffer_borrow,
};
use super::shared::wire::{slice_from_raw, write_err};

// ---- VulkanComputeKernelMethodsVTable wrappers (#949) ----------------------
//
// Each wrapper reconstructs the kernel borrow from the raw `Arc`
// handle the cdylib passes (`Arc::into_raw(Arc<VulkanComputeKernelInner>)`
// per the β-shape's `from_arc_into_raw`), runs the inner method,
// and converts the `Result<()>` into the FFI's `i32 + err_buf`
// shape. All bodies are wrapped in `run_host_extern_c` so a panic
// in the inner method becomes a non-zero return.
//
// First slice (this PR): `set_push_constants` + `dispatch`. The
// buffer/texture-input variants need a small trait redesign (the
// inner method's `B: VulkanStorageBindable` generic can't cross the
// FFI as-is — concrete β-shape inputs need a separate accessor on
// the trait) and land in a follow-up sub-issue.

/// SAFETY: caller must hand a `handle` that came from
/// `Arc::into_raw(Arc<VulkanComputeKernelInner>)`. The leaked
/// strong count keeps the kernel alive for the call's duration.
#[cfg(target_os = "linux")]
unsafe fn handle_as_compute_kernel(
    handle: *const c_void,
) -> Option<&'static crate::vulkan::rhi::VulkanComputeKernelInner> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const crate::vulkan::rhi::VulkanComputeKernelInner) })
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_push_constants(
    kernel_handle: *const c_void,
    bytes_ptr: *const u8,
    bytes_len: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_push_constants",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
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
                    write_err(&format!("set_push_constants: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_dispatch(
    kernel_handle: *const c_void,
    group_x: u32,
    group_y: u32,
    group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_dispatch",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "dispatch: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel.dispatch(group_x, group_y, group_z) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(&format!("dispatch: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// ---- Binding-method wrappers (typed by input wrapper) ---------------------
//
// Each wrapper reconstructs a stack-allocated plugin-handle borrow
// from the raw `Arc::into_raw` handle the cdylib passes, then
// forwards to the inner kernel's binding method.
//
// `ManuallyDrop` is **load-bearing**, not defensive: removing it
// would let the borrow's Drop run on scope exit, which calls the
// vtable's `drop_*` slot and decrements the host's Arc refcount —
// while the cdylib still holds an outstanding plugin handle that
// expects to own a strong reference. The result is an under-counted
// Arc and a use-after-free on the cdylib's eventual Drop. The
// cdylib owns ownership for the call's duration; the host wrapper
// only borrows.
//
// **Invariant the helpers depend on:** the inner kernel's binding
// methods (`set_storage_buffer`, `set_uniform_buffer`,
// `set_sampled_texture`, `set_storage_image`) only deref
// `self.handle` to reach the host-internal allocation; they do NOT
// read the cached POD fields (`width`, `height`, `format_raw`,
// `byte_size_cached`, `mapped_ptr_cached`, etc.). The reconstructed
// borrow zeros every POD field for that reason. If a future
// engine-side binding method starts reading `buffer.width` /
// `texture.format()` / similar through the wrapper, the zeroed POD
// silently produces wrong results — the helper site is the place to
// add a populated-field stage if that invariant ever shifts.
//
// The vtable pointer is filled with the host's limited-access vtable
// (matching what `from_arc_into_raw` would have written) so the
// borrow is well-formed for any field-only read, even though no
// vtable callback is supposed to fire while the borrow is alive.

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_storage_buffer_pixel(
    kernel_handle: *const c_void,
    binding: u32,
    pixel_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_storage_buffer_pixel",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
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
unsafe extern "C" fn host_compute_kernel_set_storage_buffer_storage(
    kernel_handle: *const c_void,
    binding: u32,
    storage_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_storage_buffer_storage",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
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
unsafe extern "C" fn host_compute_kernel_set_uniform_buffer(
    kernel_handle: *const c_void,
    binding: u32,
    uniform_buffer_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_uniform_buffer",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
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
unsafe extern "C" fn host_compute_kernel_set_sampled_texture(
    kernel_handle: *const c_void,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_sampled_texture",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
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
unsafe extern "C" fn host_compute_kernel_set_storage_image(
    kernel_handle: *const c_void,
    binding: u32,
    texture_handle: *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_storage_image",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
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

// ---- Non-Linux platform stubs (vtable layout stays unconditional) ----------

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_push_constants(
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
unsafe extern "C" fn host_compute_kernel_dispatch(
    _kernel_handle: *const c_void,
    _group_x: u32,
    _group_y: u32,
    _group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "dispatch: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_storage_buffer_pixel(
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
unsafe extern "C" fn host_compute_kernel_set_storage_buffer_storage(
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
unsafe extern "C" fn host_compute_kernel_set_uniform_buffer(
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
unsafe extern "C" fn host_compute_kernel_set_sampled_texture(
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
unsafe extern "C" fn host_compute_kernel_set_storage_image(
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

/// Read the compute kernel's declared bindings into a caller-provided
/// `[ComputeBindingSpecRepr]` buffer. v4 (introspection).
#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_bindings(
    kernel_handle: *const c_void,
    out_specs_buf: *mut streamlib_plugin_abi::ComputeBindingSpecRepr,
    out_specs_cap: usize,
    out_specs_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_bindings",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
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
                    let repr = streamlib_plugin_abi::ComputeBindingSpecRepr::from(spec);
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
unsafe extern "C" fn host_compute_kernel_bindings(
    _kernel_handle: *const c_void,
    _out_specs_buf: *mut streamlib_plugin_abi::ComputeBindingSpecRepr,
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

// ---- Raw-vulkanalia-handle slots (v5 — #1073) -----------------------------
//
// Engine SDK code (`RgbToNv12Converter::convert`,
// `Nv12ToRgbConverter::convert`) reaches three `pub(crate)`
// `VulkanComputeKernel` setter methods that take raw `vk::ImageView`
// and one `record` method that takes raw `vk::CommandBuffer`. When
// that engine SDK code is compiled into a cdylib (workspace plugin
// packages with `crate-type = ["rlib", "cdylib"]` — h264, h265,
// camera), the cdylib-compiled methods can't deref `host_inner()`
// without tripping the panic guard. These callbacks let the cdylib
// dispatch through the host's per-method vtable instead.
//
// Wire shape: `vk::ImageView` is `#[repr(transparent)] pub struct
// ImageView(u64)` and `vk::CommandBuffer` is `#[repr(transparent)]
// pub struct CommandBuffer(usize)` (vulkanalia-sys handles.rs). The
// FFI carries the raw integer as `u64`; the host reconstructs via
// `Handle::from_raw` before forwarding.

// The four callbacks below dispatch through `*_raw` shim methods on
// `VulkanComputeKernelInner` so that this file stays off the
// vulkanalia allowlist (`xtask check-boundaries`). The RHI-side shim
// is the canonical owner of `Handle::from_raw` reconstruction.

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_compute_kernel_set_sampled_image_view(
    kernel_handle: *const c_void,
    binding: u32,
    image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_sampled_image_view",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_sampled_image_view: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel.set_sampled_image_view_raw(binding, image_view_handle) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_sampled_image_view: {e}"),
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
unsafe extern "C" fn host_compute_kernel_set_combined_image_sampler_view(
    kernel_handle: *const c_void,
    binding: u32,
    image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_combined_image_sampler_view",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_combined_image_sampler_view: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel
                .set_combined_image_sampler_view_raw(binding, image_view_handle)
            {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_combined_image_sampler_view: {e}"),
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
unsafe extern "C" fn host_compute_kernel_set_storage_image_view(
    kernel_handle: *const c_void,
    binding: u32,
    image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_set_storage_image_view",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "set_storage_image_view: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel.set_storage_image_view_raw(binding, image_view_handle) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_storage_image_view: {e}"),
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
unsafe extern "C" fn host_compute_kernel_record(
    kernel_handle: *const c_void,
    command_buffer_handle: u64,
    group_x: u32,
    group_y: u32,
    group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_compute_kernel_record",
        || -> i32 {
            let Some(kernel) = (unsafe { handle_as_compute_kernel(kernel_handle) })
            else {
                write_err(
                    "record: null kernel handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            match kernel.record_raw(
                command_buffer_handle,
                group_x,
                group_y,
                group_z,
            ) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("record: {e}"),
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

// ---- Non-Linux stubs for v5 raw-vulkanalia-handle slots --------------------

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_sampled_image_view(
    _kernel_handle: *const c_void,
    _binding: u32,
    _image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_sampled_image_view: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_combined_image_sampler_view(
    _kernel_handle: *const c_void,
    _binding: u32,
    _image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_combined_image_sampler_view: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_set_storage_image_view(
    _kernel_handle: *const c_void,
    _binding: u32,
    _image_view_handle: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "set_storage_image_view: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_compute_kernel_record(
    _kernel_handle: *const c_void,
    _command_buffer_handle: u64,
    _group_x: u32,
    _group_y: u32,
    _group_z: u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    write_err(
        "record: not available on this platform",
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

/// Host-side `VulkanComputeKernelMethodsVTable` populated with v5
/// method slots — v4's surface plus the v5 raw-vulkanalia-handle
/// slots needed by engine-SDK-internal converter code reaching out of
/// cdylib-resident processors (#1073).
pub static HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE:
    streamlib_plugin_abi::VulkanComputeKernelMethodsVTable =
    streamlib_plugin_abi::VulkanComputeKernelMethodsVTable {
        layout_version: streamlib_plugin_abi::VULKAN_COMPUTE_KERNEL_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        set_push_constants: host_compute_kernel_set_push_constants,
        dispatch: host_compute_kernel_dispatch,
        set_storage_buffer_pixel: host_compute_kernel_set_storage_buffer_pixel,
        set_storage_buffer_storage: host_compute_kernel_set_storage_buffer_storage,
        set_uniform_buffer: host_compute_kernel_set_uniform_buffer,
        set_sampled_texture: host_compute_kernel_set_sampled_texture,
        set_storage_image: host_compute_kernel_set_storage_image,
        bindings: host_compute_kernel_bindings,
        set_sampled_image_view: host_compute_kernel_set_sampled_image_view,
        set_combined_image_sampler_view:
            host_compute_kernel_set_combined_image_sampler_view,
        set_storage_image_view: host_compute_kernel_set_storage_image_view,
        record: host_compute_kernel_record,
    };

/// Accessor for the host's static `VulkanComputeKernelMethodsVTable`
/// — used by `VulkanComputeKernel::from_arc_into_raw` to populate
/// the β-shape's `methods_vtable` field.
pub fn host_vulkan_compute_kernel_methods_vtable(
) -> *const streamlib_plugin_abi::VulkanComputeKernelMethodsVTable {
    // Same routing as `host_gpu_context_limited_access_vtable`:
    // cdylib β-shape constructors must store the host's vtable
    // pointer so dispatches actually cross to host code (whose
    // `host_callbacks()` returns `None`). Without this routing, the
    // β-shape stored the cdylib's local static and dispatched to the
    // cdylib's own copy of the wrapper — where `host_callbacks()`
    // returns `Some` and any reach through `Texture::host_inner()` or
    // sibling β-shape `host_inner()` accessors panics.
    match host_callbacks() {
        Some(c) if !c.vulkan_compute_kernel_methods_vtable.is_null() => {
            c.vulkan_compute_kernel_methods_vtable
        }
        _ => &HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE,
    }
}
#[cfg(all(test, target_os = "linux"))]
mod compute_kernel_methods_vtable_null_tests {
    //! Tier-1 wire-format tests for the v3 typed binding-method
    //! slots on `VulkanComputeKernelMethodsVTable`. Each wrapper must
    //! reject a null kernel handle before reaching any kernel-side
    //! state (i.e. before any deref) so cdylib callers get a clean
    //! error return on the wire-format path instead of UB.
    //!
    //! The null-buffer-handle / null-texture-handle guards live in
    //! the same wrappers and fire when the kernel handle is valid;
    //! they're exercised end-to-end by the CPU-reference dlopen
    //! integration test (which holds a real kernel and is the only
    //! place a Tier-1 null-input test can reach without panicking on
    //! the kernel-handle deref).

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
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_storage_buffer_pixel)(
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
            err_buf_as_str(&buf, len)
                .contains("set_storage_buffer_pixel: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_buffer_storage_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_storage_buffer_storage)(
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
            err_buf_as_str(&buf, len)
                .contains("set_storage_buffer_storage: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_uniform_buffer_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_uniform_buffer)(
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
            err_buf_as_str(&buf, len)
                .contains("set_uniform_buffer: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_sampled_texture_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_sampled_texture)(
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
            err_buf_as_str(&buf, len)
                .contains("set_sampled_texture: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_image_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_storage_image)(
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
            err_buf_as_str(&buf, len)
                .contains("set_storage_image: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    // ---- v5 raw-vulkanalia-handle slots (#1073) -----------------------------
    //
    // The null-kernel-handle case is the tier-1 reach: passing a real
    // `vk::ImageView` / `vk::CommandBuffer` raw handle with a null
    // kernel pointer must fail cleanly before any deref. Non-null but
    // garbage kernel handles still trip the host_inner deref's pointer
    // alignment / segfault — the same precedent the v3/v4 tests
    // document.

    #[test]
    fn set_sampled_image_view_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_sampled_image_view)(
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
            err_buf_as_str(&buf, len)
                .contains("set_sampled_image_view: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_combined_image_sampler_view_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE
                .set_combined_image_sampler_view)(
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
            err_buf_as_str(&buf, len)
                .contains("set_combined_image_sampler_view: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn set_storage_image_view_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.set_storage_image_view)(
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
            err_buf_as_str(&buf, len)
                .contains("set_storage_image_view: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn record_rejects_null_kernel_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.record)(
                std::ptr::null(),
                0,
                1, 1, 1,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("record: null kernel handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod kernel_bindings_vtable_tier1_wire_format_tests {
    //! Tier-1 wire-format tests for the `bindings` introspection slot
    //! on each kernel methods vtable (compute v4, graphics v3, ray-
    //! tracing v3).

    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn compute_bindings_returns_error_on_null_kernel() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_len: usize = 0;
        let rc = unsafe {
            (HOST_VULKAN_COMPUTE_KERNEL_METHODS_VTABLE.bindings)(
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
                &mut out_len as *mut usize,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("bindings: null kernel handle"), "got: {msg}");
    }

    #[test]
    fn graphics_bindings_returns_error_on_null_kernel() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_len: usize = 0;
        let rc = unsafe {
            (super::super::HOST_VULKAN_GRAPHICS_KERNEL_METHODS_VTABLE.bindings)(
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
                &mut out_len as *mut usize,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("bindings: null kernel handle"), "got: {msg}");
    }

    #[test]
    fn ray_tracing_bindings_returns_error_on_null_kernel() {
        let (mut buf, mut len) = make_err_buf();
        let mut out_len: usize = 0;
        let rc = unsafe {
            (super::super::HOST_VULKAN_RAY_TRACING_KERNEL_METHODS_VTABLE.bindings)(
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
                &mut out_len as *mut usize,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_buf_as_str(&buf, len);
        assert!(msg.contains("bindings: null kernel handle"), "got: {msg}");
    }
}
