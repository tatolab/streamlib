// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side wiring of
//! [`streamlib_adapter_skia_abi::SkiaSurfaceAdapterVTable`] and
//! [`streamlib_adapter_skia_abi::SkiaGlSurfaceAdapterVTable`].
//!
//! Hosts that want to expose Skia surface adapters to a cdylib
//! plugin do this:
//!
//! 1. Construct an `Arc<SkiaSurfaceAdapter<D>>` or
//!    `Arc<SkiaGlSurfaceAdapter>` on the host side (compose on the
//!    inner Vulkan / OpenGL adapter — same as today).
//! 2. Hand the cdylib a `(handle, vtable)` β-shape pair where:
//!    - `handle = Arc::into_raw(arc.clone())`
//!    - `vtable = host_skia_surface_adapter_vtable::<D>()` (Vulkan
//!      flavor) or `host_skia_gl_surface_adapter_vtable()` (GL
//!      flavor)
//! 3. The cdylib invokes the vtable methods exactly as if it held
//!    a Rust `&SkiaSurfaceAdapter<D>` — every method dispatches
//!    through host-compiled code, so layout drift between
//!    rustc-minor versions and divergent dep graphs is contained
//!    inside the host DSO.
//!
//! The Vulkan-flavor wiring is generic over
//! `D: VulkanRhiDevice + 'static` so the same vtable works whether
//! the host is exposing a `SkiaSurfaceAdapter<HostVulkanDevice>`
//! (canonical host-side path) or a
//! `SkiaSurfaceAdapter<ConsumerVulkanDevice>` (cdylib-internal
//! subprocess adapter — same shape, different device flavor). Each
//! monomorphization materializes its own `static` vtable; cdylib
//! code never sees the type parameter — only the
//! `*const SkiaSurfaceAdapterVTable` pointer.
//!
//! Panic guards inside each fn body mirror the
//! `streamlib-plugin-abi` `run_host_extern_c` shape: any panic in
//! host code is caught at the FFI boundary and converted to a
//! clean error return instead of corrupting the cdylib's stack.
//! Tier-1 null-handle tests next to this module verify the guards
//! fire correctly without an actual `Arc<SkiaSurfaceAdapter>`.
//!
//! # Host-side-only constraint
//!
//! Per [`docs/architecture/subprocess-rhi-parity.md`], Skia is
//! host-side only — cdylibs do not depend on
//! `streamlib-adapter-skia` in their Cargo dep graph; subprocess
//! customers reach Skia surfaces through the wrapped Vulkan /
//! OpenGL adapter's cdylib path. The vtables here cover the
//! `SurfaceAdapter` trait scoping contract (acquire / release on a
//! `StreamlibSurface`) only — view payload is the minimal
//! [`SkiaViewRepr`] carrying `surface_id` + dimensions, never the
//! Skia-typed `&skia_safe::Surface` / `&skia_safe::Image`. Skia's
//! `RCHandle<…>` types are tied to the host's `skia-safe` version
//! and single-thread-affine `GrDirectContext`; projecting them
//! through an extern "C" boundary would defeat the purpose of this
//! ABI. Future cross-DSO Skia draw support is its own design
//! problem (e.g. the msgpack display-list pattern recorded on
//! issue #889's body, a per-method canvas vtable, etc.).

#![cfg(target_os = "linux")]

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::Arc;

use streamlib_adapter_abi::{
    AdapterError, StreamlibSurface, SurfaceAdapter,
};
use streamlib_adapter_skia_abi::{
    SkiaGlSurfaceAdapterVTable, SkiaSurfaceAdapterVTable, SkiaViewRepr,
    SKIA_GL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
    SKIA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
};
use streamlib_consumer_rhi::VulkanRhiDevice;

use crate::adapter::SkiaSurfaceAdapter;
use crate::gl_adapter::SkiaGlSurfaceAdapter;

// =============================================================================
// Public host-side entry points
// =============================================================================

/// Returns a `*const SkiaSurfaceAdapterVTable` whose method slots
/// dispatch against an `Arc<SkiaSurfaceAdapter<D>>`-shaped handle.
///
/// The vtable is `const`-initialized per `D` monomorphization;
/// every call for the same `D` returns the same pointer. Multiple
/// `D`s coexist in the same host process with their own vtables.
pub fn host_skia_surface_adapter_vtable<D: VulkanRhiDevice + 'static>(
) -> *const SkiaSurfaceAdapterVTable {
    &VkMonoVTable::<D>::VTABLE
}

/// Returns a `*const SkiaGlSurfaceAdapterVTable` whose method
/// slots dispatch against an `Arc<SkiaGlSurfaceAdapter>`-shaped
/// handle.
///
/// Non-generic — `SkiaGlSurfaceAdapter` composes on the
/// non-generic `OpenGlSurfaceAdapter`. Returns the same pointer on
/// every call.
pub fn host_skia_gl_surface_adapter_vtable() -> *const SkiaGlSurfaceAdapterVTable {
    &GL_VTABLE
}

// =============================================================================
// Vulkan-flavor monomorphizer
// =============================================================================

/// Type-keyed monomorphizer. The `const VTABLE` is materialized at
/// codegen time for each `D` that calls
/// [`host_skia_surface_adapter_vtable`] elsewhere in the binary;
/// the fn-pointer slots resolve to the matching monomorphizations
/// of the free fns below.
struct VkMonoVTable<D: VulkanRhiDevice + 'static>(PhantomData<D>);

impl<D: VulkanRhiDevice + 'static> VkMonoVTable<D> {
    const VTABLE: SkiaSurfaceAdapterVTable = SkiaSurfaceAdapterVTable {
        layout_version: SKIA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        clone_handle: host_vk_clone_handle::<D>,
        drop_handle: host_vk_drop_handle::<D>,
        registered_count: host_vk_registered_count::<D>,
        acquire_read: host_vk_acquire_read::<D>,
        acquire_write: host_vk_acquire_write::<D>,
        try_acquire_read: host_vk_try_acquire_read::<D>,
        try_acquire_write: host_vk_try_acquire_write::<D>,
        end_read_access: host_vk_end_read_access::<D>,
        end_write_access: host_vk_end_write_access::<D>,
    };
}

// =============================================================================
// OpenGL-flavor static vtable
// =============================================================================

static GL_VTABLE: SkiaGlSurfaceAdapterVTable = SkiaGlSurfaceAdapterVTable {
    layout_version: SKIA_GL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    clone_handle: host_gl_clone_handle,
    drop_handle: host_gl_drop_handle,
    registered_count: host_gl_registered_count,
    acquire_read: host_gl_acquire_read,
    acquire_write: host_gl_acquire_write,
    try_acquire_read: host_gl_try_acquire_read,
    try_acquire_write: host_gl_try_acquire_write,
    end_read_access: host_gl_end_read_access,
    end_write_access: host_gl_end_write_access,
};

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

// =============================================================================
// Vulkan-flavor — handle borrow helper
// =============================================================================

/// Borrow the adapter from a `*const c_void` handle. Returns
/// `None` on null.
///
/// SAFETY: caller asserts the handle is one of: (a) a borrowed
/// pointer produced by `Arc::as_ptr` against a live host-owned
/// `Arc<SkiaSurfaceAdapter<D>>`, or (b) an owned pointer minted by
/// [`host_vk_clone_handle`] still in its valid lifetime. Both are
/// dereferenceable for read while the underlying Arc has at least
/// one strong refcount; the panic guard wrapping every call site
/// converts any UB-shaped mistake into a clean tracing log + the
/// callback's default return.
unsafe fn vk_adapter_borrow<'a, D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
) -> Option<&'a SkiaSurfaceAdapter<D>> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const SkiaSurfaceAdapter<D>) })
}

// =============================================================================
// Vulkan-flavor — clone_handle / drop_handle
// =============================================================================

unsafe extern "C" fn host_vk_clone_handle<D: VulkanRhiDevice + 'static>(
    borrowed_handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "skia_surface_adapter::clone_handle",
        || {
            if borrowed_handle.is_null() {
                return core::ptr::null();
            }
            // SAFETY: handle is Arc::into_raw(Arc<SkiaSurfaceAdapter<D>>)-shaped.
            unsafe {
                Arc::increment_strong_count(borrowed_handle as *const SkiaSurfaceAdapter<D>);
            }
            borrowed_handle
        },
        core::ptr::null(),
    )
}

unsafe extern "C" fn host_vk_drop_handle<D: VulkanRhiDevice + 'static>(
    owned_handle: *const c_void,
) {
    run_host_extern_c(
        "skia_surface_adapter::drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: handle is Arc::into_raw(Arc<SkiaSurfaceAdapter<D>>)-shaped
            // with at least one host-side refcount remaining.
            unsafe {
                Arc::decrement_strong_count(owned_handle as *const SkiaSurfaceAdapter<D>);
            }
        },
        (),
    )
}

// =============================================================================
// Vulkan-flavor — registered_count
// =============================================================================

unsafe extern "C" fn host_vk_registered_count<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
) -> usize {
    run_host_extern_c(
        "skia_surface_adapter::registered_count",
        || {
            let adapter = match unsafe { vk_adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => return 0usize,
            };
            // Skia adapter has no registry of its own — project
            // through the inner Vulkan adapter.
            adapter.inner().registered_count()
        },
        0usize,
    )
}

// =============================================================================
// Vulkan-flavor — SurfaceAdapter trait methods
// =============================================================================

/// Common shape for `acquire_read` / `acquire_write` /
/// `try_acquire_*`: borrow the adapter, validate the surface
/// pointer, call the inner method, write the view + status.
///
/// The closure receives the adapter + a `&StreamlibSurface` and
/// returns either:
///   - `Ok(Some(SkiaViewRepr))` → status 0, `*out_acquired = 1`,
///                                view written
///   - `Ok(None)`               → status 0, `*out_acquired = 0`
///                                (try_* only; blocking variants
///                                never produce Ok(None))
///   - `Err(msg)`               → status 1, error message written
///
/// Skia's view types are not cross-DSO-safe (see module docs), so
/// the returned view payload only carries `surface_id` + width +
/// height — the host's actual `WriteGuard` is dropped at the end
/// of the host-side acquire callback (the scope is the callback
/// itself; this is intentional, see end_*_access for the rationale).
///
/// NOTE on guard lifecycle: Skia's view types own
/// `ManuallyDrop<WriteGuard>` and run flush+timeline-signal in
/// their own `Drop`. Returning the WriteGuard to a cdylib would
/// require crossing `skia_safe::Surface` through extern "C", which
/// is unsound. Today the host-side acquire callback completes the
/// full scope synchronously: the cdylib receives only the
/// `SkiaViewRepr` identity payload and the surface is released by
/// the time the call returns. A future shape (Option C msgpack
/// display-list) would buffer the cdylib's draw commands and
/// replay them under a real host-side scope.
unsafe fn run_vk_acquire<D, F>(
    callback_name: &'static str,
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut SkiaViewRepr,
    out_acquired: Option<*mut u32>,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
    body: F,
) -> i32
where
    D: VulkanRhiDevice + 'static,
    F: FnOnce(
        &SkiaSurfaceAdapter<D>,
        &StreamlibSurface,
    ) -> Result<Option<SkiaViewRepr>, String>,
{
    run_host_extern_c(
        callback_name,
        || {
            if let Some(p) = out_acquired {
                if !p.is_null() {
                    unsafe { *p = 0 };
                }
            }
            let adapter = match unsafe { vk_adapter_borrow::<D>(handle) } {
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
            // StreamlibSurface valid for the call duration.
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

fn skia_view_repr_from_surface(surface_id: u64, surface: &StreamlibSurface) -> SkiaViewRepr {
    SkiaViewRepr {
        surface_id,
        width: surface.width,
        height: surface.height,
        _reserved: [0u8; 16],
    }
}

/// Run a Skia VK acquire-write scope and drop the WriteGuard
/// before returning. Skia's drop hook fires flush + inner-guard
/// release inside the closure body so the surface is fully
/// released by the time the cdylib receives the
/// [`SkiaViewRepr`] response. The closure's actual return value
/// is just the view's identity payload — no Skia types cross the
/// boundary.
fn run_skia_vk_write_scope<D: VulkanRhiDevice + 'static>(
    adapter: &SkiaSurfaceAdapter<D>,
    surface: &StreamlibSurface,
) -> Result<SkiaViewRepr, AdapterError> {
    let guard = adapter.acquire_write(surface)?;
    let surface_id = guard.surface_id();
    // Drop the guard explicitly so the flush + inner release runs
    // synchronously inside the FFI scope. The cdylib customer's
    // SurfaceAdapter trait contract guarantees that on `Ok` return
    // the surface has been written (and synchronized) — same as a
    // synchronous Rust-side `acquire_write` followed by drop.
    drop(guard);
    Ok(skia_view_repr_from_surface(u64::from(surface_id), surface))
}

fn run_skia_vk_read_scope<D: VulkanRhiDevice + 'static>(
    adapter: &SkiaSurfaceAdapter<D>,
    surface: &StreamlibSurface,
) -> Result<SkiaViewRepr, AdapterError> {
    let guard = adapter.acquire_read(surface)?;
    let surface_id = guard.surface_id();
    drop(guard);
    Ok(skia_view_repr_from_surface(u64::from(surface_id), surface))
}

unsafe extern "C" fn host_vk_acquire_read<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut SkiaViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_vk_acquire::<D, _>(
            "skia_surface_adapter::acquire_read",
            handle,
            surface_ptr,
            out_view,
            None,
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match run_skia_vk_read_scope(adapter, surface) {
                Ok(view) => Ok(Some(view)),
                Err(e) => Err(format!("acquire_read: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_vk_acquire_write<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut SkiaViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_vk_acquire::<D, _>(
            "skia_surface_adapter::acquire_write",
            handle,
            surface_ptr,
            out_view,
            None,
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match run_skia_vk_write_scope(adapter, surface) {
                Ok(view) => Ok(Some(view)),
                Err(e) => Err(format!("acquire_write: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_vk_try_acquire_read<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut SkiaViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_vk_acquire::<D, _>(
            "skia_surface_adapter::try_acquire_read",
            handle,
            surface_ptr,
            out_view,
            Some(out_acquired),
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.try_acquire_read(surface) {
                Ok(Some(guard)) => {
                    let surface_id = guard.surface_id();
                    drop(guard);
                    Ok(Some(skia_view_repr_from_surface(
                        u64::from(surface_id),
                        surface,
                    )))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(format!("try_acquire_read: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_vk_try_acquire_write<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut SkiaViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_vk_acquire::<D, _>(
            "skia_surface_adapter::try_acquire_write",
            handle,
            surface_ptr,
            out_view,
            Some(out_acquired),
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.try_acquire_write(surface) {
                Ok(Some(guard)) => {
                    let surface_id = guard.surface_id();
                    drop(guard);
                    Ok(Some(skia_view_repr_from_surface(
                        u64::from(surface_id),
                        surface,
                    )))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(format!("try_acquire_write: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_vk_end_read_access<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
) {
    run_host_extern_c(
        "skia_surface_adapter::end_read_access",
        || {
            let adapter = match unsafe { vk_adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => return,
            };
            // SkiaSurfaceAdapter::end_read_access is a host-side
            // no-op by design (the view's drop hook is where the
            // flush + inner-guard-drop happens). The release
            // already happened inside the host-side acquire
            // closure above; this slot exists for trunk-pattern
            // symmetry / observability if a future host wires
            // additional teardown logic here.
            adapter.end_read_access(surface_id);
        },
        (),
    )
}

unsafe extern "C" fn host_vk_end_write_access<D: VulkanRhiDevice + 'static>(
    handle: *const c_void,
    surface_id: u64,
) {
    run_host_extern_c(
        "skia_surface_adapter::end_write_access",
        || {
            let adapter = match unsafe { vk_adapter_borrow::<D>(handle) } {
                Some(a) => a,
                None => return,
            };
            adapter.end_write_access(surface_id);
        },
        (),
    )
}

// =============================================================================
// OpenGL-flavor — handle borrow helper
// =============================================================================

unsafe fn gl_adapter_borrow<'a>(handle: *const c_void) -> Option<&'a SkiaGlSurfaceAdapter> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const SkiaGlSurfaceAdapter) })
}

// =============================================================================
// OpenGL-flavor — clone_handle / drop_handle
// =============================================================================

unsafe extern "C" fn host_gl_clone_handle(borrowed_handle: *const c_void) -> *const c_void {
    run_host_extern_c(
        "skia_gl_surface_adapter::clone_handle",
        || {
            if borrowed_handle.is_null() {
                return core::ptr::null();
            }
            unsafe {
                Arc::increment_strong_count(borrowed_handle as *const SkiaGlSurfaceAdapter);
            }
            borrowed_handle
        },
        core::ptr::null(),
    )
}

unsafe extern "C" fn host_gl_drop_handle(owned_handle: *const c_void) {
    run_host_extern_c(
        "skia_gl_surface_adapter::drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            unsafe {
                Arc::decrement_strong_count(owned_handle as *const SkiaGlSurfaceAdapter);
            }
        },
        (),
    )
}

// =============================================================================
// OpenGL-flavor — registered_count
// =============================================================================

unsafe extern "C" fn host_gl_registered_count(handle: *const c_void) -> usize {
    run_host_extern_c(
        "skia_gl_surface_adapter::registered_count",
        || {
            let adapter = match unsafe { gl_adapter_borrow(handle) } {
                Some(a) => a,
                None => return 0usize,
            };
            adapter.inner().registered_count()
        },
        0usize,
    )
}

// =============================================================================
// OpenGL-flavor — SurfaceAdapter trait methods
// =============================================================================

unsafe fn run_gl_acquire<F>(
    callback_name: &'static str,
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut SkiaViewRepr,
    out_acquired: Option<*mut u32>,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
    body: F,
) -> i32
where
    F: FnOnce(
        &SkiaGlSurfaceAdapter,
        &StreamlibSurface,
    ) -> Result<Option<SkiaViewRepr>, String>,
{
    run_host_extern_c(
        callback_name,
        || {
            if let Some(p) = out_acquired {
                if !p.is_null() {
                    unsafe { *p = 0 };
                }
            }
            let adapter = match unsafe { gl_adapter_borrow(handle) } {
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

fn run_skia_gl_read_scope(
    adapter: &SkiaGlSurfaceAdapter,
    surface: &StreamlibSurface,
) -> Result<SkiaViewRepr, AdapterError> {
    let guard = adapter.acquire_read(surface)?;
    let surface_id = guard.surface_id();
    drop(guard);
    Ok(skia_view_repr_from_surface(u64::from(surface_id), surface))
}

fn run_skia_gl_write_scope(
    adapter: &SkiaGlSurfaceAdapter,
    surface: &StreamlibSurface,
) -> Result<SkiaViewRepr, AdapterError> {
    let guard = adapter.acquire_write(surface)?;
    let surface_id = guard.surface_id();
    drop(guard);
    Ok(skia_view_repr_from_surface(u64::from(surface_id), surface))
}

unsafe extern "C" fn host_gl_acquire_read(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut SkiaViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_gl_acquire(
            "skia_gl_surface_adapter::acquire_read",
            handle,
            surface_ptr,
            out_view,
            None,
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match run_skia_gl_read_scope(adapter, surface) {
                Ok(view) => Ok(Some(view)),
                Err(e) => Err(format!("acquire_read: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_gl_acquire_write(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut SkiaViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_gl_acquire(
            "skia_gl_surface_adapter::acquire_write",
            handle,
            surface_ptr,
            out_view,
            None,
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match run_skia_gl_write_scope(adapter, surface) {
                Ok(view) => Ok(Some(view)),
                Err(e) => Err(format!("acquire_write: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_gl_try_acquire_read(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut SkiaViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_gl_acquire(
            "skia_gl_surface_adapter::try_acquire_read",
            handle,
            surface_ptr,
            out_view,
            Some(out_acquired),
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.try_acquire_read(surface) {
                Ok(Some(guard)) => {
                    let surface_id = guard.surface_id();
                    drop(guard);
                    Ok(Some(skia_view_repr_from_surface(
                        u64::from(surface_id),
                        surface,
                    )))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(format!("try_acquire_read: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_gl_try_acquire_write(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut SkiaViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_gl_acquire(
            "skia_gl_surface_adapter::try_acquire_write",
            handle,
            surface_ptr,
            out_view,
            Some(out_acquired),
            err_buf,
            err_buf_cap,
            err_len,
            |adapter, surface| match adapter.try_acquire_write(surface) {
                Ok(Some(guard)) => {
                    let surface_id = guard.surface_id();
                    drop(guard);
                    Ok(Some(skia_view_repr_from_surface(
                        u64::from(surface_id),
                        surface,
                    )))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(format!("try_acquire_write: {e}")),
            },
        )
    }
}

unsafe extern "C" fn host_gl_end_read_access(handle: *const c_void, surface_id: u64) {
    run_host_extern_c(
        "skia_gl_surface_adapter::end_read_access",
        || {
            let adapter = match unsafe { gl_adapter_borrow(handle) } {
                Some(a) => a,
                None => return,
            };
            adapter.end_read_access(surface_id);
        },
        (),
    )
}

unsafe extern "C" fn host_gl_end_write_access(handle: *const c_void, surface_id: u64) {
    run_host_extern_c(
        "skia_gl_surface_adapter::end_write_access",
        || {
            let adapter = match unsafe { gl_adapter_borrow(handle) } {
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
// success-path tests require an actual `Arc<SkiaSurfaceAdapter>` /
// `Arc<SkiaGlSurfaceAdapter>` and live in the existing
// `streamlib-adapter-skia/tests/` integration tests as those
// scenarios get wired through the vtables in follow-up issues.

#[cfg(test)]
mod tier1_null_handle_tests {
    use super::*;
    use streamlib_consumer_rhi::ConsumerVulkanDevice;

    // Pick a concrete D to materialize the monomorphized Vulkan
    // vtable. The null-handle tests don't care which D — they
    // exercise the panic guards before any device-shape work fires.
    type D = ConsumerVulkanDevice;

    fn vk_vtable() -> &'static SkiaSurfaceAdapterVTable {
        // SAFETY: the returned pointer is `&'static`-shaped per the
        // `const VTABLE` construction in `VkMonoVTable<D>`.
        unsafe { &*host_skia_surface_adapter_vtable::<D>() }
    }

    fn gl_vtable() -> &'static SkiaGlSurfaceAdapterVTable {
        unsafe { &*host_skia_gl_surface_adapter_vtable() }
    }

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_msg(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    // -----------------------------------------------------------------
    // Layout version sanity
    // -----------------------------------------------------------------

    #[test]
    fn vk_layout_version_matches_constant() {
        assert_eq!(
            vk_vtable().layout_version,
            SKIA_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION
        );
        assert_eq!(vk_vtable()._reserved_padding, 0);
    }

    #[test]
    fn gl_layout_version_matches_constant() {
        assert_eq!(
            gl_vtable().layout_version,
            SKIA_GL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION
        );
        assert_eq!(gl_vtable()._reserved_padding, 0);
    }

    // -----------------------------------------------------------------
    // Vulkan-flavor null-handle tests
    // -----------------------------------------------------------------

    #[test]
    fn vk_clone_handle_returns_null_on_null_input() {
        unsafe {
            let out = (vk_vtable().clone_handle)(core::ptr::null());
            assert!(out.is_null());
        }
    }

    #[test]
    fn vk_drop_handle_null_is_no_op() {
        unsafe { (vk_vtable().drop_handle)(core::ptr::null()) };
    }

    #[test]
    fn vk_registered_count_null_handle_returns_zero() {
        let n = unsafe { (vk_vtable().registered_count)(core::ptr::null()) };
        assert_eq!(n, 0);
    }

    #[test]
    fn vk_acquire_read_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: SkiaViewRepr = SkiaViewRepr::default();
        let rc = unsafe {
            (vk_vtable().acquire_read)(
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
    fn vk_acquire_write_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: SkiaViewRepr = SkiaViewRepr::default();
        let rc = unsafe {
            (vk_vtable().acquire_write)(
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
    fn vk_try_acquire_read_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: SkiaViewRepr = SkiaViewRepr::default();
        let mut acquired: u32 = 99;
        let rc = unsafe {
            (vk_vtable().try_acquire_read)(
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
    fn vk_try_acquire_write_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: SkiaViewRepr = SkiaViewRepr::default();
        let mut acquired: u32 = 99;
        let rc = unsafe {
            (vk_vtable().try_acquire_write)(
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
    fn vk_end_read_access_null_handle_is_no_op() {
        unsafe { (vk_vtable().end_read_access)(core::ptr::null(), 42) };
    }

    #[test]
    fn vk_end_write_access_null_handle_is_no_op() {
        unsafe { (vk_vtable().end_write_access)(core::ptr::null(), 42) };
    }

    // -----------------------------------------------------------------
    // OpenGL-flavor null-handle tests
    // -----------------------------------------------------------------

    #[test]
    fn gl_clone_handle_returns_null_on_null_input() {
        unsafe {
            let out = (gl_vtable().clone_handle)(core::ptr::null());
            assert!(out.is_null());
        }
    }

    #[test]
    fn gl_drop_handle_null_is_no_op() {
        unsafe { (gl_vtable().drop_handle)(core::ptr::null()) };
    }

    #[test]
    fn gl_registered_count_null_handle_returns_zero() {
        let n = unsafe { (gl_vtable().registered_count)(core::ptr::null()) };
        assert_eq!(n, 0);
    }

    #[test]
    fn gl_acquire_read_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: SkiaViewRepr = SkiaViewRepr::default();
        let rc = unsafe {
            (gl_vtable().acquire_read)(
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
    fn gl_acquire_write_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: SkiaViewRepr = SkiaViewRepr::default();
        let rc = unsafe {
            (gl_vtable().acquire_write)(
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
    fn gl_try_acquire_read_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: SkiaViewRepr = SkiaViewRepr::default();
        let mut acquired: u32 = 99;
        let rc = unsafe {
            (gl_vtable().try_acquire_read)(
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
    fn gl_try_acquire_write_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let mut view: SkiaViewRepr = SkiaViewRepr::default();
        let mut acquired: u32 = 99;
        let rc = unsafe {
            (gl_vtable().try_acquire_write)(
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
    fn gl_end_read_access_null_handle_is_no_op() {
        unsafe { (gl_vtable().end_read_access)(core::ptr::null(), 42) };
    }

    #[test]
    fn gl_end_write_access_null_handle_is_no_op() {
        unsafe { (gl_vtable().end_write_access)(core::ptr::null(), 42) };
    }
}
