// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side wiring of
//! [`streamlib_adapter_cpu_readback_abi::CpuReadbackSurfaceAdapterVTable`].
//!
//! Hosts that want to expose a `CpuReadbackSurfaceAdapter<D>` to a
//! cdylib plugin do this:
//!
//! 1. Construct an `Arc<CpuReadbackSurfaceAdapter<D>>` on the host
//!    side (wire the in-process trigger, allocate the per-plane
//!    staging buffers + timeline, register surfaces — same as
//!    today).
//! 2. Hand the cdylib a `(handle, vtable)` β-shape pair where:
//!    - `handle = Arc::into_raw(arc.clone())`
//!    - `vtable = host_cpu_readback_surface_adapter_vtable::<D>()`
//! 3. The cdylib invokes the vtable methods exactly as if it held
//!    a Rust `&CpuReadbackSurfaceAdapter<D>` — every method
//!    dispatches through host-compiled code, so layout drift
//!    between rustc-minor versions and divergent dep graphs is
//!    contained inside the host DSO.
//!
//! Generic over `D: VulkanRhiDevice + 'static` so the same wiring
//! works whether the host is exposing a host-flavor or
//! consumer-flavor adapter. Each monomorphization materializes its
//! own `static` vtable; cdylib code never sees the type parameter
//! — only the `*const CpuReadbackSurfaceAdapterVTable` pointer.
//!
//! # Scope fence — escalate-IPC bridge is out of scope here
//!
//! The cpu-readback adapter is special: per-acquire GPU work runs
//! on the host via a thin `run_cpu_readback_copy(surface_id)`
//! escalate-IPC trigger (see
//! `docs/architecture/adapter-runtime-integration.md`). That
//! trigger is **already cross-process** through escalate IPC + the
//! host's `CpuReadbackBridge` trait on `GpuContext`. This vtable
//! wires the parallel "cdylib holds the adapter, talks to its own
//! trigger" surface, NOT the host-side bridge. The bridge stays
//! where it lives; vtable callers don't see it.
//!
//! Panic guards inside each fn body mirror the
//! `streamlib-plugin-abi` `run_host_extern_c` shape: any panic in
//! host code is caught at the FFI boundary and converted to a
//! clean error return instead of corrupting the cdylib's stack.
//! Tier-1 null-handle tests next to this module verify the guards
//! fire correctly without an actual
//! `Arc<CpuReadbackSurfaceAdapter>` or live device.

#![cfg(target_os = "linux")]

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::Arc;

use streamlib_adapter_abi::{
    ReadGuard, StreamlibSurface, SurfaceAdapter, SurfaceFormat, WriteGuard,
};
use streamlib_adapter_cpu_readback_abi::{
    CpuReadbackPlaneRepr, CpuReadbackSurfaceAdapterVTable, CpuReadbackViewRepr,
    HostSurfaceRegistrationRepr, CPU_READBACK_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION, MAX_PLANES,
};
use streamlib_consumer_rhi::{DevicePrivilege, VulkanLayout, VulkanRhiDevice};

use crate::adapter::CpuReadbackSurfaceAdapter;
use crate::state::HostSurfaceRegistration;
use crate::view::{CpuReadbackReadView, CpuReadbackWriteView};

/// Returns a `&'static CpuReadbackSurfaceAdapterVTable` whose
/// method slots dispatch against an
/// `Arc<CpuReadbackSurfaceAdapter<D>>`-shaped handle.
///
/// The vtable is `const`-initialized per `D` monomorphization;
/// every call for the same `D` returns the same pointer. Multiple
/// `D`s coexist in the same host process with their own vtables.
pub fn host_cpu_readback_surface_adapter_vtable<D: VulkanRhiDevice + 'static>(
) -> *const CpuReadbackSurfaceAdapterVTable {
    &MonoVTable::<D>::VTABLE
}

/// Type-keyed monomorphizer. The `const VTABLE` is materialized at
/// codegen time for each `D` that calls
/// [`host_cpu_readback_surface_adapter_vtable`] elsewhere in the
/// binary; the fn-pointer slots resolve to the matching
/// monomorphizations of the free fns below.
struct MonoVTable<D: VulkanRhiDevice + 'static>(PhantomData<D>);

impl<D: VulkanRhiDevice + 'static> MonoVTable<D> {
    const VTABLE: CpuReadbackSurfaceAdapterVTable = CpuReadbackSurfaceAdapterVTable {
        layout_version: CPU_READBACK_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
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
        run_bridge_copy_image_to_buffer: host_run_bridge_copy_image_to_buffer::<D>,
        run_bridge_copy_buffer_to_image: host_run_bridge_copy_buffer_to_image::<D>,
    };
}

// =============================================================================
// FFI helpers — error buffer writer + panic guard
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
) -> Option<&'a CpuReadbackSurfaceAdapter<D>> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const CpuReadbackSurfaceAdapter<D>) })
}

/// Map a `SurfaceFormat` `#[repr(u32)]` value back to the enum.
/// Returns `None` for unknown discriminants; the caller surfaces a
/// validation error.
fn surface_format_from_raw(raw: u32) -> Option<SurfaceFormat> {
    match raw {
        0 => Some(SurfaceFormat::Bgra8),
        1 => Some(SurfaceFormat::Rgba8),
        2 => Some(SurfaceFormat::Nv12),
        _ => None,
    }
}

// =============================================================================
// View-projection helpers — CpuReadbackReadView / WriteView → repr
// =============================================================================

fn read_view_to_repr<'g>(view: &CpuReadbackReadView<'g>) -> CpuReadbackViewRepr {
    let mut planes = [CpuReadbackPlaneRepr::zeroed(); MAX_PLANES];
    let plane_count = view.plane_count().min(MAX_PLANES as u32);
    for (i, p) in view.planes().iter().take(plane_count as usize).enumerate() {
        let bytes = p.bytes();
        planes[i] = CpuReadbackPlaneRepr {
            mapped_ptr: bytes.as_ptr() as u64,
            byte_size: bytes.len() as u64,
            width: p.width(),
            height: p.height(),
            bytes_per_pixel: p.bytes_per_pixel(),
            _padding: 0,
        };
    }
    CpuReadbackViewRepr {
        format_raw: view.format() as u32,
        width: view.width(),
        height: view.height(),
        plane_count,
        planes,
    }
}

fn write_view_to_repr<'g>(view: &CpuReadbackWriteView<'g>) -> CpuReadbackViewRepr {
    let mut planes = [CpuReadbackPlaneRepr::zeroed(); MAX_PLANES];
    let plane_count = view.plane_count().min(MAX_PLANES as u32);
    for (i, p) in view.planes().iter().take(plane_count as usize).enumerate() {
        // bytes() is immutable here; mapped_ptr is the same address
        // regardless of borrow flavor — the host's WriteView keeps
        // the mut borrow alive via the leaked guard, so the cdylib
        // can safely write through the same pointer for the
        // duration of the acquire scope.
        let bytes = p.bytes();
        planes[i] = CpuReadbackPlaneRepr {
            mapped_ptr: bytes.as_ptr() as u64,
            byte_size: bytes.len() as u64,
            width: p.width(),
            height: p.height(),
            bytes_per_pixel: p.bytes_per_pixel(),
            _padding: 0,
        };
    }
    CpuReadbackViewRepr {
        format_raw: view.format() as u32,
        width: view.width(),
        height: view.height(),
        plane_count,
        planes,
    }
}

fn read_guard_to_repr<D: VulkanRhiDevice + 'static>(
    guard: ReadGuard<'_, CpuReadbackSurfaceAdapter<D>>,
) -> CpuReadbackViewRepr {
    let view_repr = read_view_to_repr(guard.view());
    // SAFETY: we deliberately leak the guard's `end_read_access`
    // signal — the cdylib will fire `end_read_access` itself from
    // its own ReadGuard::drop via the vtable. Calling Drop here
    // would double-decrement the read_holders counter.
    core::mem::forget(guard);
    view_repr
}

fn write_guard_to_repr<D: VulkanRhiDevice + 'static>(
    guard: WriteGuard<'_, CpuReadbackSurfaceAdapter<D>>,
) -> CpuReadbackViewRepr {
    let view_repr = write_view_to_repr(guard.view());
    // SAFETY: same rationale as `read_guard_to_repr` — the cdylib
    // fires `end_write_access` itself via the vtable; running
    // Drop here would issue the post-write flush twice.
    core::mem::forget(guard);
    view_repr
}

// =============================================================================
// Handle lifetime
// =============================================================================

unsafe extern "C" fn host_clone_handle<D: VulkanRhiDevice + 'static>(
    borrowed_handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "cpu_readback_surface_adapter::clone_handle",
        || {
            if borrowed_handle.is_null() {
                return core::ptr::null();
            }
            // SAFETY: handle is
            // Arc::into_raw(Arc<CpuReadbackSurfaceAdapter<D>>)-shaped.
            unsafe {
                Arc::increment_strong_count(
                    borrowed_handle as *const CpuReadbackSurfaceAdapter<D>,
                );
            }
            borrowed_handle
        },
        core::ptr::null(),
    )
}

unsafe extern "C" fn host_drop_handle<D: VulkanRhiDevice + 'static>(owned_handle: *const c_void) {
    run_host_extern_c(
        "cpu_readback_surface_adapter::drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: handle is
            // Arc::into_raw(Arc<CpuReadbackSurfaceAdapter<D>>)-shaped
            // with at least one host-side refcount remaining.
            unsafe {
                Arc::decrement_strong_count(
                    owned_handle as *const CpuReadbackSurfaceAdapter<D>,
                );
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
    registration_ptr: *const HostSurfaceRegistrationRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "cpu_readback_surface_adapter::register_host_surface",
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
            if registration_ptr.is_null() {
                unsafe {
                    write_err(
                        "register_host_surface: null registration pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                }
                return 1;
            }
            // SAFETY: caller asserts the pointer is borrowed from
            // their stack for the duration of this call.
            let r: &HostSurfaceRegistrationRepr = unsafe { &*registration_ptr };
            let format = match surface_format_from_raw(r.format_raw) {
                Some(f) => f,
                None => {
                    let msg = format!(
                        "register_host_surface: unknown SurfaceFormat enumerant {}",
                        r.format_raw
                    );
                    unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                    return 1;
                }
            };
            let plane_count = r.plane_count as usize;
            if plane_count == 0 || plane_count > MAX_PLANES {
                let msg = format!(
                    "register_host_surface: plane_count {plane_count} out of [1, {MAX_PLANES}]"
                );
                unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                return 1;
            }
            if r.produce_done_handle == 0 || r.consume_done_handle == 0 {
                unsafe {
                    write_err(
                        "register_host_surface: null produce_done or consume_done handle",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                }
                return 1;
            }
            // SAFETY: caller asserts each non-zero handle is
            // Arc::into_raw-shaped against the correct privilege
            // family for D. The host bumps refcount and stashes
            // clones in the registry; caller's Arcs remain owned
            // by the caller.
            let texture: Option<Arc<<D::Privilege as DevicePrivilege>::Texture>> =
                if r.texture_handle == 0 {
                    None
                } else {
                    let ptr = r.texture_handle
                        as *const <D::Privilege as DevicePrivilege>::Texture;
                    unsafe {
                        Arc::increment_strong_count(ptr);
                        Some(Arc::from_raw(ptr))
                    }
                };
            let produce_done: Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore> = unsafe {
                let ptr = r.produce_done_handle
                    as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore;
                Arc::increment_strong_count(ptr);
                Arc::from_raw(ptr)
            };
            let consume_done: Arc<<D::Privilege as DevicePrivilege>::TimelineSemaphore> = unsafe {
                let ptr = r.consume_done_handle
                    as *const <D::Privilege as DevicePrivilege>::TimelineSemaphore;
                Arc::increment_strong_count(ptr);
                Arc::from_raw(ptr)
            };
            let mut staging_planes: Vec<Arc<<D::Privilege as DevicePrivilege>::Buffer>> =
                Vec::with_capacity(plane_count);
            for i in 0..plane_count {
                let raw = r.staging_handles[i];
                if raw == 0 {
                    let msg = format!(
                        "register_host_surface: null staging handle at plane {i}"
                    );
                    unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                    return 1;
                }
                let ptr = raw as *const <D::Privilege as DevicePrivilege>::Buffer;
                unsafe {
                    Arc::increment_strong_count(ptr);
                    staging_planes.push(Arc::from_raw(ptr));
                }
            }
            let registration: HostSurfaceRegistration<D::Privilege> = HostSurfaceRegistration {
                texture,
                staging_planes,
                produce_done,
                consume_done,
                initial_image_layout: VulkanLayout(r.initial_layout_raw),
                format,
                width: r.width,
                height: r.height,
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
        "cpu_readback_surface_adapter::unregister_host_surface",
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
        "cpu_readback_surface_adapter::registered_count",
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

/// Common shape for `acquire_*` / `try_acquire_*`: borrow the
/// adapter, validate the surface pointer, call the inner method,
/// write the view + status.
unsafe fn run_acquire<D, F>(
    callback_name: &'static str,
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CpuReadbackViewRepr,
    out_acquired: Option<*mut u32>,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
    body: F,
) -> i32
where
    D: VulkanRhiDevice + 'static,
    F: FnOnce(
        &CpuReadbackSurfaceAdapter<D>,
        &StreamlibSurface,
    ) -> Result<Option<CpuReadbackViewRepr>, String>,
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

unsafe extern "C" fn host_acquire_read<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut CpuReadbackViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire::<D, _>(
            "cpu_readback_surface_adapter::acquire_read",
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
    out_view: *mut CpuReadbackViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire::<D, _>(
            "cpu_readback_surface_adapter::acquire_write",
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
    out_view: *mut CpuReadbackViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire::<D, _>(
            "cpu_readback_surface_adapter::try_acquire_read",
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
    out_view: *mut CpuReadbackViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire::<D, _>(
            "cpu_readback_surface_adapter::try_acquire_write",
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
        "cpu_readback_surface_adapter::end_read_access",
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
        "cpu_readback_surface_adapter::end_write_access",
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
// Bridge entries
// =============================================================================

unsafe extern "C" fn host_run_bridge_copy_image_to_buffer<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
    out_signaled_value: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "cpu_readback_surface_adapter::run_bridge_copy_image_to_buffer",
        || {
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => {
                    unsafe {
                        write_err(
                            "run_bridge_copy_image_to_buffer: null adapter handle",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                    }
                    return 1;
                }
            };
            match adapter.run_bridge_copy_image_to_buffer(surface_id) {
                Ok(v) => {
                    if !out_signaled_value.is_null() {
                        unsafe { *out_signaled_value = v };
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("run_bridge_copy_image_to_buffer: {e}");
                    unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_run_bridge_copy_buffer_to_image<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
    out_signaled_value: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "cpu_readback_surface_adapter::run_bridge_copy_buffer_to_image",
        || {
            let adapter = match unsafe { adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => {
                    unsafe {
                        write_err(
                            "run_bridge_copy_buffer_to_image: null adapter handle",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                    }
                    return 1;
                }
            };
            match adapter.run_bridge_copy_buffer_to_image(surface_id) {
                Ok(v) => {
                    if !out_signaled_value.is_null() {
                        unsafe { *out_signaled_value = v };
                    }
                    0
                }
                Err(e) => {
                    let msg = format!("run_bridge_copy_buffer_to_image: {e}");
                    unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                    1
                }
            }
        },
        1,
    )
}

// =============================================================================
// Tier-1 host-side wire-format tests (null-handle guards) +
// cross-crate layout-equivalence tests
// =============================================================================
//
// Each test invokes a vtable slot directly with a null handle and
// asserts the slot's documented null-handle behaviour fires. The
// success-path tests require an actual `Arc<CpuReadbackSurfaceAdapter>`
// + a live device + live staging buffers and timeline; those live
// in the crate's `tests/` integration tests as those scenarios get
// wired through this vtable in follow-up issues.

#[cfg(test)]
mod tier1_null_handle_tests {
    use super::*;
    use std::mem::{align_of, size_of};
    use streamlib_consumer_rhi::ConsumerVulkanDevice;

    // Pick a concrete D to materialize the monomorphized vtable.
    // The null-handle tests don't care which D — they exercise the
    // panic guards before any device-shape work fires.
    type D = ConsumerVulkanDevice;

    /// `SurfaceFormat` is declared `#[repr(u32)]` in
    /// `streamlib-adapter-abi`. The vtable wire format mirrors
    /// that representation in `CpuReadbackViewRepr::format_raw`
    /// and `HostSurfaceRegistrationRepr::format_raw`. Locking the
    /// discriminant values here ensures a stealth re-numbering
    /// in `streamlib-adapter-abi` trips at this layer too.
    #[test]
    fn surface_format_discriminants_match_repr() {
        assert_eq!(SurfaceFormat::Bgra8 as u32, 0);
        assert_eq!(SurfaceFormat::Rgba8 as u32, 1);
        assert_eq!(SurfaceFormat::Nv12 as u32, 2);
    }

    /// `surface_format_from_raw` round-trips the canonical
    /// discriminants and rejects unknown values.
    #[test]
    fn surface_format_round_trip_through_raw() {
        assert_eq!(surface_format_from_raw(0), Some(SurfaceFormat::Bgra8));
        assert_eq!(surface_format_from_raw(1), Some(SurfaceFormat::Rgba8));
        assert_eq!(surface_format_from_raw(2), Some(SurfaceFormat::Nv12));
        assert_eq!(surface_format_from_raw(3), None);
        assert_eq!(surface_format_from_raw(u32::MAX), None);
    }

    /// Cross-crate sanity: the abi crate is dep-light by design
    /// and can't see `streamlib-adapter-abi`. This crate has both
    /// in scope, so we lock the size/align consistency between
    /// `CpuReadbackPlaneRepr` and its conceptual source (the
    /// per-plane view shape) here as a witness — there's no
    /// existing `#[repr(C)]` mirror in `streamlib-adapter-abi` to
    /// compare against, but the size/align is the contract the
    /// layout regression test in the abi crate locks. This test
    /// fails loudly if a future refactor accidentally changes
    /// either side without updating the other.
    #[test]
    fn cpu_readback_plane_repr_size_align_locked() {
        assert_eq!(size_of::<CpuReadbackPlaneRepr>(), 32);
        assert_eq!(align_of::<CpuReadbackPlaneRepr>(), 8);
    }

    /// Cross-crate sanity: `CpuReadbackViewRepr` size/align lock.
    /// 144 bytes = `u32×4 + [CpuReadbackPlaneRepr; 4] @ 16`.
    #[test]
    fn cpu_readback_view_repr_size_align_locked() {
        assert_eq!(size_of::<CpuReadbackViewRepr>(), 144);
        assert_eq!(align_of::<CpuReadbackViewRepr>(), 8);
    }

    /// Cross-crate sanity: `HostSurfaceRegistrationRepr` size/align
    /// lock. 80 bytes (dual-timeline: produce_done + consume_done).
    #[test]
    fn host_surface_registration_repr_size_align_locked() {
        assert_eq!(size_of::<HostSurfaceRegistrationRepr>(), 80);
        assert_eq!(align_of::<HostSurfaceRegistrationRepr>(), 8);
    }

    fn vtable() -> &'static CpuReadbackSurfaceAdapterVTable {
        // SAFETY: the returned pointer is `&'static`-shaped per the
        // `const VTABLE` construction in `MonoVTable<D>`.
        unsafe { &*host_cpu_readback_surface_adapter_vtable::<D>() }
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
            CPU_READBACK_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION
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
        let reg = HostSurfaceRegistrationRepr::zeroed();
        let rc = unsafe {
            (vtable().register_host_surface)(
                core::ptr::null(),
                42,
                &reg,
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

    // NOTE: there's no tier-1 test for the
    // `registration_ptr == null` guard inside `register_host_surface`
    // because exercising it cleanly requires a valid Arc-shaped
    // adapter handle (we'd need to build a fake-but-aligned dummy
    // and risk a misaligned-pointer abort in debug builds, which
    // bypasses the panic guard's `catch_unwind` because misalignment
    // is non-unwinding). The integration test that wires a real
    // `Arc<CpuReadbackSurfaceAdapter>` will cover the
    // registration-pointer guard alongside the success path; in
    // tier-1 the null-handle case (above) is the primary lock.

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
        let mut view: CpuReadbackViewRepr = CpuReadbackViewRepr::zeroed();
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
        let mut view: CpuReadbackViewRepr = CpuReadbackViewRepr::zeroed();
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
        let mut view: CpuReadbackViewRepr = CpuReadbackViewRepr::zeroed();
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
        let mut view: CpuReadbackViewRepr = CpuReadbackViewRepr::zeroed();
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
    fn run_bridge_copy_image_to_buffer_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut signaled: u64 = 0;
        let rc = unsafe {
            (vtable().run_bridge_copy_image_to_buffer)(
                core::ptr::null(),
                42,
                &mut signaled,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("run_bridge_copy_image_to_buffer: null adapter handle"),
            "got: {msg}"
        );
    }

    #[test]
    fn run_bridge_copy_buffer_to_image_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut signaled: u64 = 0;
        let rc = unsafe {
            (vtable().run_bridge_copy_buffer_to_image)(
                core::ptr::null(),
                42,
                &mut signaled,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("run_bridge_copy_buffer_to_image: null adapter handle"),
            "got: {msg}"
        );
    }
}
