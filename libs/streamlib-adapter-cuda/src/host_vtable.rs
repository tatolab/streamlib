// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side wiring of [`streamlib_adapter_cuda_abi::CudaSurfaceAdapterVTable`].
//!
//! Hosts that want to expose a `CudaSurfaceAdapter<D>` to a cdylib
//! plugin do this:
//!
//! 1. Construct an `Arc<CudaSurfaceAdapter<D>>` on the host side
//!    (allocate OPAQUE_FD-exportable buffers / images, register
//!    surfaces, install setup hook — same as today).
//! 2. Hand the cdylib a `(handle, vtable)` PluginAbiObject pair where:
//!    - `handle = Arc::into_raw(arc.clone())`
//!    - `vtable = host_cuda_surface_adapter_vtable::<D>()`
//! 3. The cdylib invokes the vtable methods exactly as if it held
//!    a Rust `&CudaSurfaceAdapter<D>` — every method dispatches
//!    through host-compiled code, so layout drift between
//!    rustc-minor versions and divergent dep graphs is contained
//!    inside the host plugin.
//!
//! Generic over `D: VulkanRhiDevice + 'static` so the same wiring
//! works whether the host is exposing a
//! `CudaSurfaceAdapter<HostVulkanDevice>` (canonical host-side
//! adapter) or a `CudaSurfaceAdapter<ConsumerVulkanDevice>`
//! (cdylib-internal subprocess adapter). Each monomorphization
//! materializes its own `static` vtable; cdylib code never sees the
//! type parameter — only the `*const CudaSurfaceAdapterVTable`
//! pointer.
//!
//! Panic guards inside each fn body mirror the
//! `streamlib-plugin-abi` `run_host_extern_c` shape: any panic in
//! host code is caught at the plugin ABI and converted to a
//! clean error return instead of corrupting the cdylib's stack.
//! Tier-1 null-handle tests next to this module verify the guards
//! fire correctly without an actual `Arc<CudaSurfaceAdapter>`.

#![cfg(target_os = "linux")]

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::Arc;

use streamlib_adapter_abi::{StreamlibSurface, SurfaceAdapter};
use streamlib_adapter_cuda_abi::{
    CudaBufferViewRepr, CudaImageViewRepr, CudaSurfaceAdapterVTable, TextureFormatRepr,
    CUDA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
};
use streamlib_consumer_rhi::{DevicePrivilege, VulkanLayout, VulkanRhiDevice};
use vulkanalia::vk::Handle;

use crate::adapter::CudaSurfaceAdapter;
use crate::state::{HostImageSurfaceRegistration, HostSurfaceRegistration};

/// Returns a `&'static CudaSurfaceAdapterVTable` whose method slots
/// dispatch against an `Arc<CudaSurfaceAdapter<D>>`-shaped handle.
///
/// The vtable is `const`-initialized per `D` monomorphization;
/// every call for the same `D` returns the same pointer. Multiple
/// `D`s coexist in the same host process with their own vtables.
pub fn host_cuda_surface_adapter_vtable<D: VulkanRhiDevice + 'static>(
) -> *const CudaSurfaceAdapterVTable {
    &MonoVTable::<D>::VTABLE
}

/// Type-keyed monomorphizer. The `const VTABLE` is materialized at
/// codegen time for each `D` that calls
/// [`host_cuda_surface_adapter_vtable`] elsewhere in the binary;
/// the fn-pointer slots resolve to the matching monomorphizations
/// of the free fns below.
struct MonoVTable<D: VulkanRhiDevice + 'static>(PhantomData<D>);

impl<D: VulkanRhiDevice + 'static> MonoVTable<D> {
    const VTABLE: CudaSurfaceAdapterVTable = CudaSurfaceAdapterVTable {
        layout_version: CUDA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        clone_handle: host_clone_handle::<D>,
        drop_handle: host_drop_handle::<D>,
        register_host_surface: host_register_host_surface::<D>,
        register_host_image_surface: host_register_host_image_surface::<D>,
        unregister_host_surface: host_unregister_host_surface::<D>,
        registered_count: host_registered_count::<D>,
        acquire_read: host_acquire_read::<D>,
        acquire_write: host_acquire_write::<D>,
        try_acquire_read: host_try_acquire_read::<D>,
        try_acquire_write: host_try_acquire_write::<D>,
        acquire_texture: host_acquire_texture::<D>,
        acquire_surface: host_acquire_surface::<D>,
        try_acquire_texture: host_try_acquire_texture::<D>,
        try_acquire_surface: host_try_acquire_surface::<D>,
        end_read_access: host_end_read_access::<D>,
        end_write_access: host_end_write_access::<D>,
    };
}

// =============================================================================
// Plugin ABI helpers — error buffer writer + panic guard
// =============================================================================

// Panic-safety net wrapping every `host_*` extern "C" callback is the
// shared [`streamlib_adapter_abi::ffi::run_host_extern_c`].
use streamlib_adapter_abi::ffi::run_host_extern_c;

/// Write a UTF-8 error message into a caller-provided buffer.
/// Truncates to `cap`; sets `*err_len` to the bytes actually
/// written. Null pointers are tolerated (the message is simply
/// dropped).
unsafe fn write_err(msg: &str, err_buf: *mut u8, err_buf_cap: usize, err_len: *mut usize) {
    if err_buf.is_null() || err_buf_cap == 0 {
        if !err_len.is_null() {
            unsafe { *err_len = 0 };
        }
        return;
    }
    let bytes = msg.as_bytes();
    let n = bytes.len().min(err_buf_cap);
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), err_buf, n);
        if !err_len.is_null() {
            *err_len = n;
        }
    }
}

/// Borrow the adapter from a `*const c_void` handle. Returns
/// `None` on null. SAFETY: caller asserts the handle is one of:
/// (a) a borrowed pointer produced by `Arc::as_ptr` against a live
/// host-owned Arc, or (b) an owned pointer minted by
/// [`host_clone_handle`] still in its valid lifetime. Both are
/// dereferenceable for read while the underlying Arc has at least
/// one strong refcount; the panic guard wrapping every call site
/// converts any UB-shaped mistake into a clean tracing log + the
/// callback's default return.
unsafe fn adapter_borrow<'a, D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
) -> Option<&'a CudaSurfaceAdapter<D>> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const CudaSurfaceAdapter<D>) })
}

// =============================================================================
// Handle lifetime (mirrors GpuContextLimitedAccessVTable::clone_handle / drop_handle)
// =============================================================================

unsafe extern "C" fn host_clone_handle<D: VulkanRhiDevice + 'static>(
    borrowed_handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "cuda_surface_adapter::clone_handle",
        || {
            if borrowed_handle.is_null() {
                return core::ptr::null();
            }
            // SAFETY: handle is Arc::into_raw(Arc<CudaSurfaceAdapter<D>>)-shaped.
            unsafe {
                Arc::increment_strong_count(borrowed_handle as *const CudaSurfaceAdapter<D>);
            }
            borrowed_handle
        },
        core::ptr::null(),
    )
}

unsafe extern "C" fn host_drop_handle<D: VulkanRhiDevice + 'static>(owned_handle: *const c_void) {
    run_host_extern_c(
        "cuda_surface_adapter::drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: handle is Arc::into_raw(Arc<CudaSurfaceAdapter<D>>)-shaped
            // with at least one host-side refcount remaining.
            unsafe {
                Arc::decrement_strong_count(owned_handle as *const CudaSurfaceAdapter<D>);
            }
        },
        (),
    )
}

// =============================================================================
// Registry management
// =============================================================================

unsafe extern "C" fn host_register_host_surface<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
    pixel_buffer_handle: *const c_void,
    produce_done_handle: *const c_void,
    consume_done_handle: *const c_void,
    initial_layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "cuda_surface_adapter::register_host_surface",
        || {
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => {
                    unsafe {
                        write_err(
                            "register_host_surface: null adapter handle",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                    }
                    return 1;
                }
            };
            if pixel_buffer_handle.is_null()
                || produce_done_handle.is_null()
                || consume_done_handle.is_null()
            {
                unsafe {
                    write_err(
                        "register_host_surface: null pixel_buffer, produce_done, \
                         or consume_done handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                }
                return 1;
            }
            // SAFETY: caller asserts the handles are
            // Arc::into_raw-shaped against the correct privilege
            // family for D. The host bumps the refcount and stashes
            // clones in the registry; the caller's Arcs remain
            // owned by the caller.
            let pixel_buffer: Arc<<D::Privilege as DevicePrivilege>::Buffer> = unsafe {
                Arc::increment_strong_count(
                    pixel_buffer_handle as *const <D::Privilege as DevicePrivilege>::Buffer,
                );
                Arc::from_raw(pixel_buffer_handle as *const <D::Privilege as DevicePrivilege>::Buffer)
            };
            let produce_done: Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore> = unsafe {
                Arc::increment_strong_count(
                    produce_done_handle
                        as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore,
                );
                Arc::from_raw(
                    produce_done_handle
                        as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore,
                )
            };
            let consume_done: Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore> = unsafe {
                Arc::increment_strong_count(
                    consume_done_handle
                        as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore,
                );
                Arc::from_raw(
                    consume_done_handle
                        as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore,
                )
            };
            let registration = HostSurfaceRegistration {
                pixel_buffer,
                produce_done,
                consume_done,
                initial_layout: VulkanLayout(initial_layout_raw),
            };
            match adapter.register_host_surface(surface_id, registration) {
                Ok(()) => 0,
                Err(e) => {
                    let msg = format!("register_host_surface: {e}");
                    unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_register_host_image_surface<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
    texture_handle: *const c_void,
    produce_done_handle: *const c_void,
    consume_done_handle: *const c_void,
    initial_layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "cuda_surface_adapter::register_host_image_surface",
        || {
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => {
                    unsafe {
                        write_err(
                            "register_host_image_surface: null adapter handle",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                    }
                    return 1;
                }
            };
            if texture_handle.is_null()
                || produce_done_handle.is_null()
                || consume_done_handle.is_null()
            {
                unsafe {
                    write_err(
                        "register_host_image_surface: null texture, produce_done, \
                         or consume_done handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                }
                return 1;
            }
            // SAFETY: caller asserts the handles are
            // Arc::into_raw-shaped against the correct privilege
            // family for D.
            let texture: Arc<<D::Privilege as DevicePrivilege>::Texture> = unsafe {
                Arc::increment_strong_count(
                    texture_handle as *const <D::Privilege as DevicePrivilege>::Texture,
                );
                Arc::from_raw(texture_handle as *const <D::Privilege as DevicePrivilege>::Texture)
            };
            let produce_done: Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore> = unsafe {
                Arc::increment_strong_count(
                    produce_done_handle
                        as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore,
                );
                Arc::from_raw(
                    produce_done_handle
                        as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore,
                )
            };
            let consume_done: Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore> = unsafe {
                Arc::increment_strong_count(
                    consume_done_handle
                        as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore,
                );
                Arc::from_raw(
                    consume_done_handle
                        as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore,
                )
            };
            let registration = HostImageSurfaceRegistration {
                texture,
                produce_done,
                consume_done,
                initial_layout: VulkanLayout(initial_layout_raw),
            };
            match adapter.register_host_image_surface(surface_id, registration) {
                Ok(()) => 0,
                Err(e) => {
                    let msg = format!("register_host_image_surface: {e}");
                    unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_unregister_host_surface<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
    out_was_present: *mut u32,
) {
    run_host_extern_c(
        "cuda_surface_adapter::unregister_host_surface",
        || {
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => {
                    if !out_was_present.is_null() {
                        unsafe { *out_was_present = 0 };
                    }
                    return;
                }
            };
            let was_present = adapter.unregister_host_surface(surface_id);
            if !out_was_present.is_null() {
                unsafe { *out_was_present = u32::from(was_present) };
            }
        },
        (),
    )
}

unsafe extern "C" fn host_registered_count<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
) -> usize {
    run_host_extern_c(
        "cuda_surface_adapter::registered_count",
        || {
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => return 0usize,
            };
            adapter.registered_count()
        },
        0usize,
    )
}

// =============================================================================
// SurfaceAdapter trait methods — buffer flavor
// =============================================================================

/// Common shape for buffer-flavored acquire callbacks.
unsafe fn run_buffer_acquire<D, F>(
    callback_name: &'static str,
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CudaBufferViewRepr,
    out_acquired: Option<*mut u32>,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
    body: F,
) -> i32
where
    D: VulkanRhiDevice + 'static,
    F: FnOnce(
        &CudaSurfaceAdapter<D>,
        &StreamlibSurface,
    ) -> Result<Option<CudaBufferViewRepr>, String>,
{
    run_host_extern_c(
        callback_name,
        || {
            if let Some(p) = out_acquired {
                if !p.is_null() {
                    unsafe { *p = 0 };
                }
            }
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => {
                    unsafe {
                        write_err(
                            &format!("{callback_name}: null adapter handle"),
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                    }
                    return 1;
                }
            };
            if surface_ptr.is_null() {
                unsafe {
                    write_err(
                        &format!("{callback_name}: null surface pointer"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                }
                return 1;
            }
            // SAFETY: caller asserts the pointer is a borrowed
            // StreamlibSurface from its own stack/heap, valid for
            // the duration of the call.
            let surface: &StreamlibSurface =
                unsafe { &*(surface_ptr as *const StreamlibSurface) };
            match body(adapter, surface) {
                Ok(Some(view)) => {
                    if !out_view.is_null() {
                        unsafe { *out_view = view };
                    }
                    if let Some(p) = out_acquired {
                        if !p.is_null() {
                            unsafe { *p = 1 };
                        }
                    }
                    0
                }
                Ok(None) => {
                    // Contended — try_* shape. Blocking variants
                    // never reach here (the body errors instead).
                    0
                }
                Err(msg) => {
                    unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                    1
                }
            }
        },
        1,
    )
}

/// Common shape for image-flavored acquire callbacks.
unsafe fn run_image_acquire<D, F>(
    callback_name: &'static str,
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CudaImageViewRepr,
    out_acquired: Option<*mut u32>,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
    body: F,
) -> i32
where
    D: VulkanRhiDevice + 'static,
    F: FnOnce(
        &CudaSurfaceAdapter<D>,
        &StreamlibSurface,
    ) -> Result<Option<CudaImageViewRepr>, String>,
{
    run_host_extern_c(
        callback_name,
        || {
            if let Some(p) = out_acquired {
                if !p.is_null() {
                    unsafe { *p = 0 };
                }
            }
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => {
                    unsafe {
                        write_err(
                            &format!("{callback_name}: null adapter handle"),
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                    }
                    return 1;
                }
            };
            if surface_ptr.is_null() {
                unsafe {
                    write_err(
                        &format!("{callback_name}: null surface pointer"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                }
                return 1;
            }
            let surface: &StreamlibSurface =
                unsafe { &*(surface_ptr as *const StreamlibSurface) };
            match body(adapter, surface) {
                Ok(Some(view)) => {
                    if !out_view.is_null() {
                        unsafe { *out_view = view };
                    }
                    if let Some(p) = out_acquired {
                        if !p.is_null() {
                            unsafe { *p = 1 };
                        }
                    }
                    0
                }
                Ok(None) => 0,
                Err(msg) => {
                    unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                    1
                }
            }
        },
        1,
    )
}

/// Project a buffer-flavored RAII guard into a `CudaBufferViewRepr`
/// and `mem::forget` the guard so the cdylib's mirror RAII fires
/// `end_read_access` / `end_write_access` itself.
///
/// Calling Drop here would double-signal — the cdylib's PluginAbiObject
/// guard will issue the release through the vtable's
/// `end_*_access` slot when it's dropped.
fn read_guard_to_buffer_repr<D: VulkanRhiDevice + 'static>(
    guard: streamlib_adapter_abi::ReadGuard<'_, CudaSurfaceAdapter<D>>,
) -> CudaBufferViewRepr {
    let view = guard.view();
    let repr = CudaBufferViewRepr {
        vk_buffer: view.vk_buffer().as_raw(),
        size: view.size(),
    };
    core::mem::forget(guard);
    repr
}

fn write_guard_to_buffer_repr<D: VulkanRhiDevice + 'static>(
    guard: streamlib_adapter_abi::WriteGuard<'_, CudaSurfaceAdapter<D>>,
) -> CudaBufferViewRepr {
    let view = guard.view();
    let repr = CudaBufferViewRepr {
        vk_buffer: view.vk_buffer().as_raw(),
        size: view.size(),
    };
    core::mem::forget(guard);
    repr
}

fn texture_guard_to_image_repr<D: VulkanRhiDevice + 'static>(
    guard: crate::view::CudaTextureGuard<'_, D>,
) -> CudaImageViewRepr {
    let view = guard.view();
    let repr = CudaImageViewRepr {
        vk_image: view.vk_image().as_raw(),
        width: view.width(),
        height: view.height(),
        format: TextureFormatRepr(view.format() as u32),
        _padding: 0,
    };
    core::mem::forget(guard);
    repr
}

fn surface_guard_to_image_repr<D: VulkanRhiDevice + 'static>(
    guard: crate::view::CudaSurfaceGuard<'_, D>,
) -> CudaImageViewRepr {
    let view = guard.view();
    let repr = CudaImageViewRepr {
        vk_image: view.vk_image().as_raw(),
        width: view.width(),
        height: view.height(),
        format: TextureFormatRepr(view.format() as u32),
        _padding: 0,
    };
    core::mem::forget(guard);
    repr
}

unsafe extern "C" fn host_acquire_read<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CudaBufferViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_buffer_acquire::<D, _>(
            "cuda_surface_adapter::acquire_read",
            handle,
            surface_ptr,
            out_view,
            None,
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.acquire_read(surface) {
                Ok(guard) => Ok(Some(read_guard_to_buffer_repr(guard))),
                Err(e) => Err(format!("acquire_read: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_acquire_write<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CudaBufferViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_buffer_acquire::<D, _>(
            "cuda_surface_adapter::acquire_write",
            handle,
            surface_ptr,
            out_view,
            None,
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.acquire_write(surface) {
                Ok(guard) => Ok(Some(write_guard_to_buffer_repr(guard))),
                Err(e) => Err(format!("acquire_write: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_try_acquire_read<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CudaBufferViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_buffer_acquire::<D, _>(
            "cuda_surface_adapter::try_acquire_read",
            handle,
            surface_ptr,
            out_view,
            Some(out_acquired),
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.try_acquire_read(surface) {
                Ok(Some(guard)) => Ok(Some(read_guard_to_buffer_repr(guard))),
                Ok(None) => Ok(None),
                Err(e) => Err(format!("try_acquire_read: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_try_acquire_write<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CudaBufferViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_buffer_acquire::<D, _>(
            "cuda_surface_adapter::try_acquire_write",
            handle,
            surface_ptr,
            out_view,
            Some(out_acquired),
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.try_acquire_write(surface) {
                Ok(Some(guard)) => Ok(Some(write_guard_to_buffer_repr(guard))),
                Ok(None) => Ok(None),
                Err(e) => Err(format!("try_acquire_write: {e}")),
            },
        )
    }
}

// =============================================================================
// Image-flavor acquire methods
// =============================================================================

unsafe extern "C" fn host_acquire_texture<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CudaImageViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_image_acquire::<D, _>(
            "cuda_surface_adapter::acquire_texture",
            handle,
            surface_ptr,
            out_view,
            None,
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.acquire_texture(surface) {
                Ok(guard) => Ok(Some(texture_guard_to_image_repr(guard))),
                Err(e) => Err(format!("acquire_texture: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_acquire_surface<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CudaImageViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_image_acquire::<D, _>(
            "cuda_surface_adapter::acquire_surface",
            handle,
            surface_ptr,
            out_view,
            None,
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.acquire_surface(surface) {
                Ok(guard) => Ok(Some(surface_guard_to_image_repr(guard))),
                Err(e) => Err(format!("acquire_surface: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_try_acquire_texture<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CudaImageViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_image_acquire::<D, _>(
            "cuda_surface_adapter::try_acquire_texture",
            handle,
            surface_ptr,
            out_view,
            Some(out_acquired),
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.try_acquire_texture(surface) {
                Ok(Some(guard)) => Ok(Some(texture_guard_to_image_repr(guard))),
                Ok(None) => Ok(None),
                Err(e) => Err(format!("try_acquire_texture: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_try_acquire_surface<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CudaImageViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_image_acquire::<D, _>(
            "cuda_surface_adapter::try_acquire_surface",
            handle,
            surface_ptr,
            out_view,
            Some(out_acquired),
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.try_acquire_surface(surface) {
                Ok(Some(guard)) => Ok(Some(surface_guard_to_image_repr(guard))),
                Ok(None) => Ok(None),
                Err(e) => Err(format!("try_acquire_surface: {e}")),
            },
        )
    }
}

// =============================================================================
// Release (shared between buffer + image flavors)
// =============================================================================

unsafe extern "C" fn host_end_read_access<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
) {
    run_host_extern_c(
        "cuda_surface_adapter::end_read_access",
        || {
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => return,
            };
            adapter.end_read_access(surface_id);
        },
        (),
    )
}

unsafe extern "C" fn host_end_write_access<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
) {
    run_host_extern_c(
        "cuda_surface_adapter::end_write_access",
        || {
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => return,
            };
            adapter.end_write_access(surface_id);
        },
        (),
    )
}

// =============================================================================
// Tier-1 host-side wire-format tests (null-handle guards)
// =============================================================================
//
// Each test invokes a vtable slot directly with a null handle and
// asserts the slot's documented null-handle behaviour fires. The
// success-path tests require an actual `Arc<CudaSurfaceAdapter>`
// + a live `HostVulkanDevice` and live in the existing
// `streamlib-adapter-cuda/tests/` integration tests as those
// scenarios get wired through this vtable in follow-up issues.

#[cfg(test)]
mod tier1_null_handle_tests {
    use super::*;
    use streamlib_consumer_rhi::ConsumerVulkanDevice;

    // Pick a concrete D to materialize the monomorphized vtable.
    // The null-handle tests don't care which D — they exercise the
    // panic guards before any device-shape work fires.
    type D = ConsumerVulkanDevice;

    fn vtable() -> &'static CudaSurfaceAdapterVTable {
        // SAFETY: the returned pointer is `&'static`-shaped per the
        // `const VTABLE` construction in `MonoVTable<D>`.
        unsafe { &*host_cuda_surface_adapter_vtable::<D>() }
    }

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_msg(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            vtable().layout_version,
            CUDA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION
        );
        assert_eq!(vtable()._reserved_padding, 0);
    }

    #[test]
    fn clone_handle_returns_null_on_null_input() {
        unsafe {
            let out = (vtable().clone_handle)(core::ptr::null());
            assert!(out.is_null());
        }
    }

    #[test]
    fn drop_handle_null_is_no_op() {
        // Documented contract — must not crash on null.
        unsafe {
            (vtable().drop_handle)(core::ptr::null());
        }
    }

    #[test]
    fn register_host_surface_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (vtable().register_host_surface)(
                core::ptr::null(),
                42,
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("register_host_surface: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn register_host_image_surface_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (vtable().register_host_image_surface)(
                core::ptr::null(),
                42,
                core::ptr::null(),
                core::ptr::null(),
                core::ptr::null(),
                0,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("register_host_image_surface: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn unregister_host_surface_null_handle_returns_zero_was_present() {
        let mut was_present: u32 = 0xFFFF_FFFF;
        unsafe {
            (vtable().unregister_host_surface)(core::ptr::null(), 42, &mut was_present);
        }
        assert_eq!(was_present, 0);
    }

    #[test]
    fn registered_count_null_handle_returns_zero() {
        let n = unsafe { (vtable().registered_count)(core::ptr::null()) };
        assert_eq!(n, 0);
    }

    #[test]
    fn acquire_read_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: CudaBufferViewRepr = unsafe { core::mem::zeroed() };
        let rc = unsafe {
            (vtable().acquire_read)(
                core::ptr::null(),
                core::ptr::null(),
                &mut view,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("acquire_read: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn acquire_write_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: CudaBufferViewRepr = unsafe { core::mem::zeroed() };
        let rc = unsafe {
            (vtable().acquire_write)(
                core::ptr::null(),
                core::ptr::null(),
                &mut view,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("acquire_write: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn try_acquire_read_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: CudaBufferViewRepr = unsafe { core::mem::zeroed() };
        let mut acquired: u32 = 99;
        let rc = unsafe {
            (vtable().try_acquire_read)(
                core::ptr::null(),
                core::ptr::null(),
                &mut view,
                &mut acquired,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert_eq!(acquired, 0);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("try_acquire_read: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn try_acquire_write_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: CudaBufferViewRepr = unsafe { core::mem::zeroed() };
        let mut acquired: u32 = 99;
        let rc = unsafe {
            (vtable().try_acquire_write)(
                core::ptr::null(),
                core::ptr::null(),
                &mut view,
                &mut acquired,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert_eq!(acquired, 0);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("try_acquire_write: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn acquire_texture_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: CudaImageViewRepr = unsafe { core::mem::zeroed() };
        let rc = unsafe {
            (vtable().acquire_texture)(
                core::ptr::null(),
                core::ptr::null(),
                &mut view,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("acquire_texture: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn acquire_surface_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: CudaImageViewRepr = unsafe { core::mem::zeroed() };
        let rc = unsafe {
            (vtable().acquire_surface)(
                core::ptr::null(),
                core::ptr::null(),
                &mut view,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("acquire_surface: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn try_acquire_texture_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: CudaImageViewRepr = unsafe { core::mem::zeroed() };
        let mut acquired: u32 = 99;
        let rc = unsafe {
            (vtable().try_acquire_texture)(
                core::ptr::null(),
                core::ptr::null(),
                &mut view,
                &mut acquired,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert_eq!(acquired, 0);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("try_acquire_texture: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn try_acquire_surface_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: CudaImageViewRepr = unsafe { core::mem::zeroed() };
        let mut acquired: u32 = 99;
        let rc = unsafe {
            (vtable().try_acquire_surface)(
                core::ptr::null(),
                core::ptr::null(),
                &mut view,
                &mut acquired,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert_eq!(acquired, 0);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("try_acquire_surface: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn end_read_access_null_handle_is_no_op() {
        unsafe { (vtable().end_read_access)(core::ptr::null(), 42) };
    }

    #[test]
    fn end_write_access_null_handle_is_no_op() {
        unsafe { (vtable().end_write_access)(core::ptr::null(), 42) };
    }

    // ---------------------------------------------------------------
    // Cross-crate layout-equivalence tests — lock the source-crate
    // `#[repr(C)]` types whose layout the `streamlib-adapter-cuda-abi`
    // reprs mirror. The abi crate is dep-free by design and can't
    // import the source types itself.
    // ---------------------------------------------------------------

    /// `TextureFormatRepr` (defined in `streamlib-adapter-cuda-abi`)
    /// MUST mirror `streamlib_consumer_rhi::TextureFormat`'s
    /// `#[repr(u32)]` representation byte-for-byte. Locked here
    /// because this crate has both in scope.
    #[test]
    fn texture_format_repr_matches_source_layout() {
        use core::mem::{align_of, size_of};
        use streamlib_consumer_rhi::TextureFormat;
        assert_eq!(size_of::<TextureFormatRepr>(), size_of::<TextureFormat>());
        assert_eq!(align_of::<TextureFormatRepr>(), align_of::<TextureFormat>());

        // The CUDA-mappable subset's discriminant values must match
        // the wire encoding so the cdylib can reconstruct the typed
        // enum from `TextureFormatRepr(raw).0`.
        assert_eq!(TextureFormat::Rgba8Unorm as u32, 0);
        assert_eq!(TextureFormat::Rgba16Float as u32, 4);
        assert_eq!(TextureFormat::Rgba32Float as u32, 5);
    }
}
