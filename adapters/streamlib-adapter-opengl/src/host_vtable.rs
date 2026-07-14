// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side wiring of [`streamlib_adapter_opengl_abi::OpenGlSurfaceAdapterVTable`].
//!
//! Hosts that want to expose an `OpenGlSurfaceAdapter` to a cdylib
//! plugin do this:
//!
//! 1. Construct an `Arc<OpenGlSurfaceAdapter>` on the host side
//!    (allocate the EglRuntime, register surfaces, install setup
//!    hook — same as today).
//! 2. Hand the cdylib a `(handle, vtable)` PluginAbiObject pair where:
//!    - `handle = Arc::into_raw(arc.clone())`
//!    - `vtable = host_opengl_surface_adapter_vtable()`
//! 3. The cdylib invokes the vtable methods exactly as if it held
//!    a Rust `&OpenGlSurfaceAdapter` — every method dispatches
//!    through host-compiled code, so layout drift between
//!    rustc-minor versions and divergent dep graphs is contained
//!    inside the host plugin.
//!
//! `OpenGlSurfaceAdapter` is not generic over a `DevicePrivilege`
//! type the way `VulkanSurfaceAdapter<D>` is — the OpenGL adapter
//! holds an `Arc<EglRuntime>` directly. The host wiring therefore
//! materializes a single `static VTABLE` rather than the
//! per-`D`-monomorphization shape used by the Vulkan sibling.
//!
//! Panic guards inside each fn body mirror the
//! `streamlib-plugin-abi` `run_host_extern_c` shape: any panic in
//! host code is caught at the plugin ABI and converted to a
//! clean error return instead of corrupting the cdylib's stack.
//! Tier-1 null-handle tests next to this module verify the guards
//! fire correctly without an actual `Arc<OpenGlSurfaceAdapter>`.

#![cfg(target_os = "linux")]

use std::ffi::c_void;
use std::sync::Arc;

use streamlib_adapter_abi::{StreamlibSurface, SurfaceAdapter};
use streamlib_adapter_opengl_abi::{
    HostSurfaceRegistrationRepr, OPENGL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
    OpenGlSurfaceAdapterVTable, OpenGlViewRepr,
};

use crate::adapter::OpenGlSurfaceAdapter;
use crate::state::HostSurfaceRegistration;

/// Returns the static `&'static OpenGlSurfaceAdapterVTable` whose
/// method slots dispatch against an `Arc<OpenGlSurfaceAdapter>`-shaped
/// handle.
pub fn host_opengl_surface_adapter_vtable() -> *const OpenGlSurfaceAdapterVTable {
    &HOST_VTABLE
}

static HOST_VTABLE: OpenGlSurfaceAdapterVTable = OpenGlSurfaceAdapterVTable {
    layout_version: OPENGL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION,
    _reserved_padding: 0,
    clone_handle: host_clone_handle,
    drop_handle: host_drop_handle,
    register_host_surface: host_register_host_surface,
    register_external_oes_host_surface: host_register_external_oes_host_surface,
    unregister_host_surface: host_unregister_host_surface,
    registered_count: host_registered_count,
    acquire_read: host_acquire_read,
    acquire_write: host_acquire_write,
    try_acquire_read: host_try_acquire_read,
    try_acquire_write: host_try_acquire_write,
    end_read_access: host_end_read_access,
    end_write_access: host_end_write_access,
};

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
unsafe fn adapter_borrow<'a>(handle: *const c_void) -> Option<&'a OpenGlSurfaceAdapter> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &*(handle as *const OpenGlSurfaceAdapter) })
}

// =============================================================================
// Handle lifetime (mirrors GpuContextLimitedAccessVTable::clone_handle / drop_handle)
// =============================================================================

unsafe extern "C" fn host_clone_handle(borrowed_handle: *const c_void) -> *const c_void {
    run_host_extern_c(
        "opengl_surface_adapter::clone_handle",
        || {
            if borrowed_handle.is_null() {
                return core::ptr::null();
            }
            // SAFETY: handle is Arc::into_raw(Arc<OpenGlSurfaceAdapter>)-shaped.
            unsafe {
                Arc::increment_strong_count(borrowed_handle as *const OpenGlSurfaceAdapter);
            }
            borrowed_handle
        },
        core::ptr::null(),
    )
}

unsafe extern "C" fn host_drop_handle(owned_handle: *const c_void) {
    run_host_extern_c(
        "opengl_surface_adapter::drop_handle",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: handle is Arc::into_raw(Arc<OpenGlSurfaceAdapter>)-shaped
            // with at least one host-side refcount remaining.
            unsafe {
                Arc::decrement_strong_count(owned_handle as *const OpenGlSurfaceAdapter);
            }
        },
        (),
    )
}

// =============================================================================
// Registry management
// =============================================================================

fn registration_from_repr(repr: &HostSurfaceRegistrationRepr) -> HostSurfaceRegistration {
    HostSurfaceRegistration {
        dma_buf_fd: repr.dma_buf_fd,
        width: repr.width,
        height: repr.height,
        drm_fourcc: repr.drm_fourcc,
        drm_format_modifier: repr.drm_format_modifier,
        plane_offset: repr.plane_offset,
        plane_stride: repr.plane_stride,
    }
}

unsafe extern "C" fn host_register_host_surface(
    handle: *const c_void,
    surface_id: u64,
    registration: *const HostSurfaceRegistrationRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "opengl_surface_adapter::register_host_surface",
        || {
            let adapter = match unsafe { adapter_borrow(handle) } {
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
            if registration.is_null() {
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
            // SAFETY: caller asserts the pointer is a borrowed
            // HostSurfaceRegistrationRepr from its own stack/heap,
            // valid for the duration of the call.
            let repr = unsafe { &*registration };
            let reg = registration_from_repr(repr);
            match adapter.register_host_surface(surface_id, reg) {
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

unsafe extern "C" fn host_register_external_oes_host_surface(
    handle: *const c_void,
    surface_id: u64,
    registration: *const HostSurfaceRegistrationRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "opengl_surface_adapter::register_external_oes_host_surface",
        || {
            let adapter = match unsafe { adapter_borrow(handle) } {
                Some(a) => a,
                None => {
                    unsafe {
                        write_err(
                            "register_external_oes_host_surface: null adapter handle",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                    }
                    return 1;
                }
            };
            if registration.is_null() {
                unsafe {
                    write_err(
                        "register_external_oes_host_surface: null registration pointer",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                }
                return 1;
            }
            // SAFETY: caller asserts the pointer is a borrowed
            // HostSurfaceRegistrationRepr from its own stack/heap,
            // valid for the duration of the call.
            let repr = unsafe { &*registration };
            let reg = registration_from_repr(repr);
            match adapter.register_external_oes_host_surface(surface_id, reg) {
                Ok(()) => 0,
                Err(e) => {
                    let msg = format!("register_external_oes_host_surface: {e}");
                    unsafe { write_err(&msg, err_buf, err_buf_cap, err_len) };
                    1
                }
            }
        },
        1,
    )
}

unsafe extern "C" fn host_unregister_host_surface(
    handle: *const c_void,
    surface_id: u64,
    out_was_present: *mut u32,
) {
    run_host_extern_c(
        "opengl_surface_adapter::unregister_host_surface",
        || {
            let adapter = match unsafe { adapter_borrow(handle) } {
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

unsafe extern "C" fn host_registered_count(handle: *const c_void) -> usize {
    run_host_extern_c(
        "opengl_surface_adapter::registered_count",
        || {
            let adapter = match unsafe { adapter_borrow(handle) } {
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
unsafe fn run_acquire<F>(
    callback_name: &'static str,
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut OpenGlViewRepr,
    out_acquired: Option<*mut u32>,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
    body: F,
) -> i32
where
    F: FnOnce(&OpenGlSurfaceAdapter, &StreamlibSurface) -> Result<Option<OpenGlViewRepr>, String>,
{
    run_host_extern_c(
        callback_name,
        || {
            if let Some(p) = out_acquired {
                if !p.is_null() {
                    unsafe { *p = 0 };
                }
            }
            let adapter = match unsafe { adapter_borrow(handle) } {
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
            let surface: &StreamlibSurface = unsafe { &*(surface_ptr as *const StreamlibSurface) };
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

fn read_guard_to_repr(
    guard: streamlib_adapter_abi::ReadGuard<'_, OpenGlSurfaceAdapter>,
) -> OpenGlViewRepr {
    let view = guard.view();
    let view_repr = OpenGlViewRepr {
        gl_texture_id: view.gl_texture_id(),
        target: view.target(),
    };
    // SAFETY: we deliberately leak the guard's `end_read_access`
    // signal — the cdylib will fire `end_read_access` itself from
    // its own ReadGuard::drop via the vtable. Calling Drop here
    // would double-signal.
    core::mem::forget(guard);
    view_repr
}

fn write_guard_to_repr(
    guard: streamlib_adapter_abi::WriteGuard<'_, OpenGlSurfaceAdapter>,
) -> OpenGlViewRepr {
    let view = guard.view();
    // OpenGlWriteView's target is always GL_TEXTURE_2D by construction
    // (write acquires are rejected for GL_TEXTURE_EXTERNAL_OES surfaces
    // inside the adapter's try_begin_write path). We surface it
    // explicitly via the view's accessor so the contract is read
    // off the live view, not hardcoded here.
    let view_repr = OpenGlViewRepr {
        gl_texture_id: view.gl_texture_id(),
        target: view.target(),
    };
    core::mem::forget(guard);
    view_repr
}

unsafe extern "C" fn host_acquire_read(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut OpenGlViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire(
            "opengl_surface_adapter::acquire_read",
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

unsafe extern "C" fn host_acquire_write(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut OpenGlViewRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire(
            "opengl_surface_adapter::acquire_write",
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

unsafe extern "C" fn host_try_acquire_read(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut OpenGlViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire(
            "opengl_surface_adapter::try_acquire_read",
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

unsafe extern "C" fn host_try_acquire_write(
    handle: *const c_void,
    surface_ptr: *const c_void,
    out_view: *mut OpenGlViewRepr,
    out_acquired: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    unsafe {
        run_acquire(
            "opengl_surface_adapter::try_acquire_write",
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

unsafe extern "C" fn host_end_read_access(handle: *const c_void, surface_id: u64) {
    run_host_extern_c(
        "opengl_surface_adapter::end_read_access",
        || {
            let adapter = match unsafe { adapter_borrow(handle) } {
                Some(a) => a,
                None => return,
            };
            adapter.end_read_access(surface_id);
        },
        (),
    )
}

unsafe extern "C" fn host_end_write_access(handle: *const c_void, surface_id: u64) {
    run_host_extern_c(
        "opengl_surface_adapter::end_write_access",
        || {
            let adapter = match unsafe { adapter_borrow(handle) } {
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
// success-path tests require an actual `Arc<OpenGlSurfaceAdapter>`
// + a live `EglRuntime` and live in the existing
// `streamlib-adapter-opengl/tests/` integration tests as those
// scenarios get wired through this vtable in follow-up issues.

#[cfg(test)]
mod tier1_null_handle_tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};
    use streamlib_adapter_opengl_abi::HostSurfaceRegistrationRepr;

    /// `HostSurfaceRegistrationRepr` (defined in
    /// `streamlib-adapter-opengl-abi`) MUST mirror
    /// `streamlib_adapter_opengl::HostSurfaceRegistration` byte-for-
    /// byte. This crate has both in scope so it's the natural place
    /// to lock the contract; the abi crate is dep-free by design
    /// and can't import the source type itself.
    #[test]
    fn host_surface_registration_repr_matches_source_layout() {
        assert_eq!(
            size_of::<HostSurfaceRegistrationRepr>(),
            size_of::<HostSurfaceRegistration>()
        );
        assert_eq!(
            align_of::<HostSurfaceRegistrationRepr>(),
            align_of::<HostSurfaceRegistration>()
        );
        assert_eq!(
            offset_of!(HostSurfaceRegistrationRepr, dma_buf_fd),
            offset_of!(HostSurfaceRegistration, dma_buf_fd)
        );
        assert_eq!(
            offset_of!(HostSurfaceRegistrationRepr, width),
            offset_of!(HostSurfaceRegistration, width)
        );
        assert_eq!(
            offset_of!(HostSurfaceRegistrationRepr, height),
            offset_of!(HostSurfaceRegistration, height)
        );
        assert_eq!(
            offset_of!(HostSurfaceRegistrationRepr, drm_fourcc),
            offset_of!(HostSurfaceRegistration, drm_fourcc)
        );
        assert_eq!(
            offset_of!(HostSurfaceRegistrationRepr, drm_format_modifier),
            offset_of!(HostSurfaceRegistration, drm_format_modifier)
        );
        assert_eq!(
            offset_of!(HostSurfaceRegistrationRepr, plane_offset),
            offset_of!(HostSurfaceRegistration, plane_offset)
        );
        assert_eq!(
            offset_of!(HostSurfaceRegistrationRepr, plane_stride),
            offset_of!(HostSurfaceRegistration, plane_stride)
        );
    }

    fn vtable() -> &'static OpenGlSurfaceAdapterVTable {
        // SAFETY: the returned pointer is `&'static`-shaped per the
        // `static HOST_VTABLE` construction above.
        unsafe { &*host_opengl_surface_adapter_vtable() }
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
            OPENGL_SURFACE_ADAPTER_VTABLE_LAYOUT_VERSION
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
    fn register_external_oes_host_surface_returns_error_on_null_handle() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (vtable().register_external_oes_host_surface)(
                core::ptr::null(),
                42,
                core::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        let msg = err_msg(&buf, len);
        assert!(
            msg.contains("register_external_oes_host_surface: null adapter handle"),
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
        let mut view: OpenGlViewRepr = unsafe { core::mem::zeroed() };
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
        let mut view: OpenGlViewRepr = unsafe { core::mem::zeroed() };
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
        let mut view: OpenGlViewRepr = unsafe { core::mem::zeroed() };
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
        let mut view: OpenGlViewRepr = unsafe { core::mem::zeroed() };
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
}
