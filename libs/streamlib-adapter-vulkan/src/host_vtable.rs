// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side wiring of [`streamlib_adapter_vulkan_abi::VulkanSurfaceAdapterVTable`].
//!
//! Hosts that want to expose a `VulkanSurfaceAdapter<D>` to a
//! cdylib plugin do this:
//!
//! 1. Construct an `Arc<VulkanSurfaceAdapter<D>>` on the host side
//!    (allocate textures, register surfaces, install setup hook —
//!    same as today).
//! 2. Hand the cdylib a `(handle, vtable)` β-shape pair where:
//!    - `handle = Arc::into_raw(arc.clone())`
//!    - `vtable = host_vulkan_surface_adapter_vtable::<D>()`
//! 3. The cdylib invokes the vtable methods exactly as if it held
//!    a Rust `&VulkanSurfaceAdapter<D>` — every method dispatches
//!    through host-compiled code, so layout drift between
//!    rustc-minor versions and divergent dep graphs is contained
//!    inside the host DSO.
//!
//! Generic over `D: VulkanRhiDevice + 'static` so the same wiring
//! works whether the host is exposing a
//! `VulkanSurfaceAdapter<HostVulkanDevice>` (canonical host-side
//! adapter) or a `VulkanSurfaceAdapter<ConsumerVulkanDevice>`
//! (cdylib-internal subprocess adapter). Each monomorphization
//! materializes its own `static` vtable; cdylib code never sees
//! the type parameter — only the `*const VulkanSurfaceAdapterVTable`
//! pointer.
//!
//! Panic guards inside each fn body mirror the
//! `streamlib-plugin-abi` `run_host_extern_c` shape: any panic in
//! host code is caught at the FFI boundary and converted to a
//! clean error return instead of corrupting the cdylib's stack.
//! Tier-1 null-handle tests next to this module verify the guards
//! fire correctly without an actual `Arc<VulkanSurfaceAdapter>`.

#![cfg(target_os = "linux")]

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::Arc;

use streamlib_adapter_abi::{StreamlibSurface, SurfaceAdapter, VkImageInfo};
use streamlib_adapter_vulkan_abi::{
    RawVulkanHandlesRepr, VkImageInfoRepr, VkImageLayoutValueRepr,
    VulkanSurfaceAdapterVTable, VulkanViewRepr,
    VULKAN_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
};
use streamlib_consumer_rhi::{DevicePrivilege, VulkanLayout, VulkanRhiDevice};

use crate::adapter::VulkanSurfaceAdapter;
use crate::raw_handles::raw_handles;

/// Returns a `&'static VulkanSurfaceAdapterVTable` whose method
/// slots dispatch against an `Arc<VulkanSurfaceAdapter<D>>`-shaped
/// handle.
///
/// The vtable is `const`-initialized per `D` monomorphization;
/// every call for the same `D` returns the same pointer. Multiple
/// `D`s coexist in the same host process with their own vtables.
pub fn host_vulkan_surface_adapter_vtable<D: VulkanRhiDevice + 'static>(
) -> *const VulkanSurfaceAdapterVTable {
    &MonoVTable::<D>::VTABLE
}

/// Type-keyed monomorphizer. The `const VTABLE` is materialized at
/// codegen time for each `D` that calls
/// [`host_vulkan_surface_adapter_vtable`] elsewhere in the binary;
/// the fn-pointer slots resolve to the matching monomorphizations
/// of the free fns below.
struct MonoVTable<D: VulkanRhiDevice + 'static>(PhantomData<D>);

impl<D: VulkanRhiDevice + 'static> MonoVTable<D> {
    const VTABLE: VulkanSurfaceAdapterVTable = VulkanSurfaceAdapterVTable {
        layout_version: VULKAN_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        clone_handle: host_clone_handle::<D>,
        drop_handle: host_drop_handle::<D>,
        register_host_surface: host_register_host_surface::<D>,
        unregister_host_surface: host_unregister_host_surface::<D>,
        registered_count: host_registered_count::<D>,
        acquire_read: host_acquire_read::<D>,
        acquire_write: host_acquire_write::<D>,
        try_acquire_read: host_try_acquire_read::<D>,
        try_acquire_write: host_try_acquire_write::<D>,
        end_read_access: host_end_read_access::<D>,
        end_write_access: host_end_write_access::<D>,
        release_to_foreign: host_release_to_foreign::<D>,
        surface_image_info: host_surface_image_info::<D>,
        raw_handles: host_raw_handles::<D>,
    };
}

// =============================================================================
// FFI helpers — error buffer writer + panic guard
// =============================================================================

/// Catch panics at the extern "C" boundary so a host-side panic
/// doesn't unwind into cdylib code. On panic the body returns
/// `default_on_panic`; the panic message is logged via `tracing`
/// for post-mortem visibility.
#[inline]
fn run_host_extern_c<F, T>(callback_name: &'static str, body: F, default_on_panic: T) -> T
where
    F: FnOnce() -> T,
{
    use std::panic::{catch_unwind, AssertUnwindSafe};
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            tracing::error!(
                target: "streamlib_adapter_vulkan::ffi",
                callback = callback_name,
                panic = %msg,
                "host extern \"C\" callback panicked; FFI boundary converted panic to default return"
            );
            default_on_panic
        }
    }
}

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

// =============================================================================
// Payload helpers — VkImageInfo / VulkanReadView / VulkanWriteView → repr
// =============================================================================

fn vk_image_info_to_repr(info: VkImageInfo) -> VkImageInfoRepr {
    VkImageInfoRepr {
        format: info.format,
        tiling: info.tiling,
        usage_flags: info.usage_flags,
        sample_count: info.sample_count,
        level_count: info.level_count,
        queue_family: info.queue_family,
        memory_handle: info.memory_handle,
        memory_offset: info.memory_offset,
        memory_size: info.memory_size,
        memory_property_flags: info.memory_property_flags,
        protected: info.protected,
        ycbcr_conversion: info.ycbcr_conversion,
        _reserved: info._reserved,
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
) -> Option<&'a VulkanSurfaceAdapter<D>> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const VulkanSurfaceAdapter<D>) })
}

// =============================================================================
// Handle lifetime (mirrors GpuContextLimitedAccessVTable::clone_handle / drop_handle)
// =============================================================================

unsafe extern "C" fn host_clone_handle<D: VulkanRhiDevice + 'static>(
    borrowed_handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "vulkan_surface_adapter::clone_handle",
        || {
            if borrowed_handle.is_null() {
                return core::ptr::null();
            }
            // SAFETY: handle is Arc::into_raw(Arc<VulkanSurfaceAdapter<D>>)-shaped.
            unsafe {
                Arc::increment_strong_count(borrowed_handle as *const VulkanSurfaceAdapter<D>);
            }
            borrowed_handle
        },
        core::ptr::null(),
    )
}

unsafe extern "C" fn host_drop_handle<D: VulkanRhiDevice + 'static>(owned_handle: *const c_void) {
    run_host_extern_c(
        "vulkan_surface_adapter::drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: handle is Arc::into_raw(Arc<VulkanSurfaceAdapter<D>>)-shaped
            // with at least one host-side refcount remaining.
            unsafe {
                Arc::decrement_strong_count(owned_handle as *const VulkanSurfaceAdapter<D>);
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
    texture_handle: *const c_void,
    timeline_handle: *const c_void,
    initial_layout_raw: i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "vulkan_surface_adapter::register_host_surface",
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
            if texture_handle.is_null() || timeline_handle.is_null() {
                unsafe {
                    write_err(
                        "register_host_surface: null texture or timeline handle",
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
            // a clone in the registry; the caller's Arcs remain
            // owned by the caller.
            let texture: Arc<<D::Privilege as DevicePrivilege>::Texture> = unsafe {
                Arc::increment_strong_count(
                    texture_handle as *const <D::Privilege as DevicePrivilege>::Texture,
                );
                Arc::from_raw(texture_handle as *const <D::Privilege as DevicePrivilege>::Texture)
            };
            let timeline: Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore> = unsafe {
                Arc::increment_strong_count(
                    timeline_handle as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore,
                );
                Arc::from_raw(
                    timeline_handle
                        as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore,
                )
            };
            let registration = crate::state::HostSurfaceRegistration {
                texture,
                timeline,
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

unsafe extern "C" fn host_unregister_host_surface<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
    out_was_present: *mut u32,
) {
    run_host_extern_c(
        "vulkan_surface_adapter::unregister_host_surface",
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
        "vulkan_surface_adapter::registered_count",
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
// SurfaceAdapter trait methods
// =============================================================================

/// Common shape for `acquire_read` / `acquire_write` /
/// `try_acquire_*`: borrow the adapter, validate the surface
/// pointer, call the inner method, write the view + status. The
/// closure receives the adapter + a `&StreamlibSurface` and
/// returns either:
///   - `Ok(Some(view))`  → status 0, `*out_acquired = 1`, view written
///   - `Ok(None)`        → status 0, `*out_acquired = 0` (try_* only;
///                          blocking variants reject via the caller)
///   - `Err(msg)`        → status 1, error message written
unsafe fn run_acquire<D, F>(
    callback_name: &'static str,
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut VulkanViewRepr,
    out_acquired: Option<*mut u32>,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
    body: F,
) -> i32
where
    D: VulkanRhiDevice + 'static,
    F: FnOnce(
        &VulkanSurfaceAdapter<D>,
        &StreamlibSurface,
    ) -> Result<Option<VulkanViewRepr>, String>,
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

fn read_guard_to_repr<D: VulkanRhiDevice + 'static>(
    guard: streamlib_adapter_abi::ReadGuard<'_, VulkanSurfaceAdapter<D>>,
) -> VulkanViewRepr {
    let view = guard.view();
    // SAFETY: we deliberately leak the guard's `end_read_access`
    // signal — the cdylib will fire `end_read_access` itself from
    // its own ReadGuard::drop via the vtable. Calling Drop here
    // would double-signal.
    let view_repr = VulkanViewRepr {
        vk_image: streamlib_adapter_abi::VulkanWritable::vk_image(view).0,
        vk_image_layout: VkImageLayoutValueRepr(
            streamlib_adapter_abi::VulkanWritable::vk_image_layout(view).0,
        ),
        _padding: 0,
        info: vk_image_info_to_repr(streamlib_adapter_abi::VulkanImageInfoExt::vk_image_info(
            view,
        )),
    };
    core::mem::forget(guard);
    view_repr
}

fn write_guard_to_repr<D: VulkanRhiDevice + 'static>(
    guard: streamlib_adapter_abi::WriteGuard<'_, VulkanSurfaceAdapter<D>>,
) -> VulkanViewRepr {
    let view = guard.view();
    let view_repr = VulkanViewRepr {
        vk_image: streamlib_adapter_abi::VulkanWritable::vk_image(view).0,
        vk_image_layout: VkImageLayoutValueRepr(
            streamlib_adapter_abi::VulkanWritable::vk_image_layout(view).0,
        ),
        _padding: 0,
        info: vk_image_info_to_repr(streamlib_adapter_abi::VulkanImageInfoExt::vk_image_info(
            view,
        )),
    };
    core::mem::forget(guard);
    view_repr
}

unsafe extern "C" fn host_acquire_read<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut VulkanViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire::<D, _>(
            "vulkan_surface_adapter::acquire_read",
            handle,
            surface_ptr,
            out_view,
            None,
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.acquire_read(surface) {
                Ok(guard) => Ok(Some(read_guard_to_repr(guard))),
                Err(e) => Err(format!("acquire_read: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_acquire_write<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut VulkanViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire::<D, _>(
            "vulkan_surface_adapter::acquire_write",
            handle,
            surface_ptr,
            out_view,
            None,
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.acquire_write(surface) {
                Ok(guard) => Ok(Some(write_guard_to_repr(guard))),
                Err(e) => Err(format!("acquire_write: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_try_acquire_read<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut VulkanViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire::<D, _>(
            "vulkan_surface_adapter::try_acquire_read",
            handle,
            surface_ptr,
            out_view,
            Some(out_acquired),
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.try_acquire_read(surface) {
                Ok(Some(guard)) => Ok(Some(read_guard_to_repr(guard))),
                Ok(None) => Ok(None),
                Err(e) => Err(format!("try_acquire_read: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_try_acquire_write<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut VulkanViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire::<D, _>(
            "vulkan_surface_adapter::try_acquire_write",
            handle,
            surface_ptr,
            out_view,
            Some(out_acquired),
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.try_acquire_write(surface) {
                Ok(Some(guard)) => Ok(Some(write_guard_to_repr(guard))),
                Ok(None) => Ok(None),
                Err(e) => Err(format!("try_acquire_write: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_end_read_access<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
) {
    run_host_extern_c(
        "vulkan_surface_adapter::end_read_access",
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
        "vulkan_surface_adapter::end_write_access",
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
// Cross-process publishing
// =============================================================================

unsafe extern "C" fn host_release_to_foreign<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
    post_release_layout_raw: i32,
    out_resulting_layout_raw: *mut i32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "vulkan_surface_adapter::release_to_foreign",
        || {
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => {
                    unsafe {
                        write_err(
                            "release_to_foreign: null adapter handle",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                    }
                    return 1;
                }
            };
            match adapter.release_to_foreign(surface_id, VulkanLayout(post_release_layout_raw)) {
                Ok(layout) => {
                    if !out_resulting_layout_raw.is_null() {
                        unsafe { *out_resulting_layout_raw = layout.0 };
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("release_to_foreign: {e}");
                    unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                    1
                }
            }
        },
        1,
    )
}

// =============================================================================
// surface_image_info
// =============================================================================

unsafe extern "C" fn host_surface_image_info<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
    out_info: *mut VkImageInfoRepr,
    out_found: *mut u32,
) {
    run_host_extern_c(
        "vulkan_surface_adapter::surface_image_info",
        || {
            if !out_found.is_null() {
                unsafe { *out_found = 0 };
            }
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => return,
            };
            if let Some(info) = adapter.surface_image_info(surface_id) {
                if !out_info.is_null() {
                    unsafe { *out_info = vk_image_info_to_repr(info) };
                }
                if !out_found.is_null() {
                    unsafe { *out_found = 1 };
                }
            }
        },
        (),
    )
}

// =============================================================================
// raw_handles
// =============================================================================

unsafe extern "C" fn host_raw_handles<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    out_handles: *mut RawVulkanHandlesRepr,
) {
    run_host_extern_c(
        "vulkan_surface_adapter::raw_handles",
        || {
            if out_handles.is_null() {
                return;
            }
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => {
                    // Zero the slot so the caller sees the
                    // "no handles available" sentinel rather than
                    // garbage.
                    unsafe {
                        *out_handles = RawVulkanHandlesRepr {
                            vk_instance: 0,
                            vk_physical_device: 0,
                            vk_device: 0,
                            vk_queue: 0,
                            vk_queue_family_index: 0,
                            api_version: 0,
                        };
                    }
                    return;
                }
            };
            let raw = raw_handles(adapter.device().as_ref());
            unsafe {
                *out_handles = RawVulkanHandlesRepr {
                    vk_instance: raw.vk_instance,
                    vk_physical_device: raw.vk_physical_device,
                    vk_device: raw.vk_device,
                    vk_queue: raw.vk_queue,
                    vk_queue_family_index: raw.vk_queue_family_index,
                    api_version: raw.api_version,
                };
            }
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
// success-path tests require an actual `Arc<VulkanSurfaceAdapter>`
// + a live `HostVulkanDevice` and live in the existing
// `streamlib-adapter-vulkan/tests/` integration tests as those
// scenarios get wired through this vtable in follow-up issues.

#[cfg(test)]
mod tier1_null_handle_tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};
    use streamlib_consumer_rhi::ConsumerVulkanDevice;

    // Pick a concrete D to materialize the monomorphized vtable.
    // The null-handle tests don't care which D — they exercise the
    // panic guards before any device-shape work fires.
    type D = ConsumerVulkanDevice;

    /// `VkImageInfoRepr` (defined in `streamlib-adapter-vulkan-abi`)
    /// MUST mirror `streamlib_adapter_abi::VkImageInfo` byte-for-byte.
    /// This crate has both in scope so it's the natural place to
    /// lock the contract; the abi crate is dep-free by design and
    /// can't import the source type itself.
    #[test]
    fn vk_image_info_repr_matches_adapter_abi_layout() {
        assert_eq!(size_of::<VkImageInfoRepr>(), size_of::<VkImageInfo>());
        assert_eq!(align_of::<VkImageInfoRepr>(), align_of::<VkImageInfo>());
        assert_eq!(
            offset_of!(VkImageInfoRepr, format),
            offset_of!(VkImageInfo, format)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, tiling),
            offset_of!(VkImageInfo, tiling)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, usage_flags),
            offset_of!(VkImageInfo, usage_flags)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, sample_count),
            offset_of!(VkImageInfo, sample_count)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, level_count),
            offset_of!(VkImageInfo, level_count)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, queue_family),
            offset_of!(VkImageInfo, queue_family)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, memory_handle),
            offset_of!(VkImageInfo, memory_handle)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, memory_offset),
            offset_of!(VkImageInfo, memory_offset)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, memory_size),
            offset_of!(VkImageInfo, memory_size)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, memory_property_flags),
            offset_of!(VkImageInfo, memory_property_flags)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, protected),
            offset_of!(VkImageInfo, protected)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, ycbcr_conversion),
            offset_of!(VkImageInfo, ycbcr_conversion)
        );
        assert_eq!(
            offset_of!(VkImageInfoRepr, _reserved),
            offset_of!(VkImageInfo, _reserved)
        );
    }

    /// `RawVulkanHandlesRepr` MUST mirror `RawVulkanHandles` from
    /// this crate byte-for-byte. The host's `raw_handles` slot
    /// projects between them via a field-by-field copy; this test
    /// locks the source layout so a future field reorder doesn't
    /// silently desync the wire format.
    #[test]
    fn raw_vulkan_handles_repr_matches_source_layout() {
        use crate::raw_handles::RawVulkanHandles;
        assert_eq!(
            size_of::<RawVulkanHandlesRepr>(),
            size_of::<RawVulkanHandles>()
        );
        assert_eq!(
            align_of::<RawVulkanHandlesRepr>(),
            align_of::<RawVulkanHandles>()
        );
    }

    fn vtable() -> &'static VulkanSurfaceAdapterVTable {
        // SAFETY: the returned pointer is `&'static`-shaped per the
        // `const VTABLE` construction in `MonoVTable<D>`.
        unsafe { &*host_vulkan_surface_adapter_vtable::<D>() }
    }

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_msg(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(vtable().layout_version, VULKAN_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION);
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
        let mut view: VulkanViewRepr = unsafe { core::mem::zeroed() };
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
        assert!(msg.contains("acquire_read: null adapter handle"), "got: {msg}");
    }

    #[test]
    fn acquire_write_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: VulkanViewRepr = unsafe { core::mem::zeroed() };
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
        let mut view: VulkanViewRepr = unsafe { core::mem::zeroed() };
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
        let mut view: VulkanViewRepr = unsafe { core::mem::zeroed() };
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
    fn end_read_access_null_handle_is_no_op() {
        unsafe { (vtable().end_read_access)(core::ptr::null(), 42) };
    }

    #[test]
    fn end_write_access_null_handle_is_no_op() {
        unsafe { (vtable().end_write_access)(core::ptr::null(), 42) };
    }

    #[test]
    fn release_to_foreign_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut layout: i32 = 0;
        let rc = unsafe {
            (vtable().release_to_foreign)(
                core::ptr::null(),
                42,
                0,
                &mut layout,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("release_to_foreign: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn surface_image_info_null_handle_sets_out_found_zero() {
        let mut info: VkImageInfoRepr = unsafe { core::mem::zeroed() };
        let mut found: u32 = 99;
        unsafe {
            (vtable().surface_image_info)(core::ptr::null(), 42, &mut info, &mut found);
        }
        assert_eq!(found, 0);
    }

    #[test]
    fn raw_handles_null_handle_writes_zeroed_struct() {
        let mut handles: RawVulkanHandlesRepr = RawVulkanHandlesRepr {
            vk_instance: 0xDEAD,
            vk_physical_device: 0xBEEF,
            vk_device: 0xCAFE,
            vk_queue: 0xBABE,
            vk_queue_family_index: 0xFF,
            api_version: 0xFF,
        };
        unsafe { (vtable().raw_handles)(core::ptr::null(), &mut handles) };
        assert_eq!(handles.vk_instance, 0);
        assert_eq!(handles.vk_physical_device, 0);
        assert_eq!(handles.vk_device, 0);
        assert_eq!(handles.vk_queue, 0);
        assert_eq!(handles.vk_queue_family_index, 0);
        assert_eq!(handles.api_version, 0);
    }
}
