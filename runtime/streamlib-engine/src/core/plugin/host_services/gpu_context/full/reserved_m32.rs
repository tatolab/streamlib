// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextFullAccessVTable` v11 host bodies (M32 one-shot slot
//! reservation, #1253) — mixed landed + reserved.
//!
//! The four OPAQUE_FD/CUDA slots (`create_opaque_fd_export_buffer`,
//! `export_storage_buffer_opaque_fd`,
//! `wrap_storage_buffer_as_pixel_buffer`,
//! `copy_texture_to_storage_buffer_and_signal`) carry their real host
//! bodies as of the #1262 fill-in: each validates its out-params +
//! scope token, then dispatches to the resolved `Arc<GpuContext>` via
//! [`with_full_scope_or_err`], all under the `run_host_extern_c` panic
//! net. Linux-only; the non-Linux stubs return a typed
//! "not available on this platform" error.
//!
//! The remaining reserved slots (present target #1258, hardware video
//! #1259, exportable timeline #1260, texture readback #1261) still ship
//! a typed NotYetProvided-style stub: a non-zero return
//! ([`NOT_YET_PROVIDED_RC`]) + a descriptive `write_err` message, never
//! `todo!()` / `unimplemented!()`, never an unguarded unwind across the
//! ABI. Their per-surface fill-in issues replace those bodies against
//! the frozen slots without touching the vtable struct again.
//!
//! The four drop-only slots (`drop_present_target`,
//! `drop_encoder_session`, `drop_decoder_session`, `drop_texture_readback`)
//! are defensive no-ops until minting lands: no create slot yields a
//! real handle yet, so drop is never called with one.

use std::ffi::c_void;

use streamlib_plugin_abi::{
    ColorTraitsRepr, OpaqueFdExportDescriptorRepr, RawWindowHandleRepr,
    VideoDecoderSessionDescriptorRepr, VideoEncoderSessionDescriptorRepr,
};

use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::{NOT_YET_PROVIDED_RC, not_yet_provided, write_err};
#[cfg(target_os = "linux")]
use super::super::scope_token::with_full_scope_or_err;
#[cfg(target_os = "linux")]
use super::super::shared::pixel_format_from_raw;

/// `VkExternalMemoryHandleTypeFlagBits::OPAQUE_FD`. Written into
/// [`OpaqueFdExportDescriptorRepr::handle_type_raw`] so a cdylib CUDA
/// adapter can pass the matching handle type to `cudaImportExternalMemory`.
#[cfg(target_os = "linux")]
const OPAQUE_FD_HANDLE_TYPE_RAW: u32 = 0x0000_0001;

// ============================================================================
// Present target (#1258)
// ============================================================================

/// Borrowed-native-handle shim implementing `raw-window-handle`'s
/// `HasWindowHandle` + `HasDisplayHandle` from the flattened
/// [`RawWindowHandleRepr`], so `VulkanPresentTarget::new` can build a
/// `VkSurfaceKHR` from the caller's window without the SDK ever naming a
/// `vk::*` type. The caller (SDK / winit event loop) owns the native
/// window and guarantees the borrowed window + display pointers remain
/// valid until the minted `PresentTarget` is dropped — a Wayland / Xlib
/// `VkSurfaceKHR` retains the display connection, and `vkDestroySurfaceKHR`
/// (at `PresentTarget` drop, not at this call's return) dereferences it.
#[cfg(target_os = "linux")]
struct RawWindowHandleShim {
    window: raw_window_handle::RawWindowHandle,
    display: raw_window_handle::RawDisplayHandle,
}

#[cfg(target_os = "linux")]
impl raw_window_handle::HasWindowHandle for RawWindowHandleShim {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        // SAFETY: per the struct-level contract the caller keeps the
        // borrowed native window pointer valid until the minted
        // PresentTarget is dropped (vkDestroySurfaceKHR), which outlives
        // this borrow.
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(self.window) })
    }
}

#[cfg(target_os = "linux")]
impl raw_window_handle::HasDisplayHandle for RawWindowHandleShim {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        // SAFETY: per the struct-level contract the caller keeps the
        // borrowed native display pointer valid until the minted
        // PresentTarget is dropped — a Wayland / Xlib VkSurfaceKHR retains
        // the display connection, which vkDestroySurfaceKHR dereferences at
        // PresentTarget drop, well after this borrow ends.
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(self.display) })
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_present_target(
    scope_token: *const c_void,
    window: *const RawWindowHandleRepr,
    width: u32,
    height: u32,
    vsync: u32,
    color: *const ColorTraitsRepr,
    out_present_target: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    use std::num::NonZeroU32;
    use std::os::raw::{c_int, c_ulong};
    use std::ptr::NonNull;

    use raw_window_handle::{
        RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
        XcbDisplayHandle, XcbWindowHandle, XlibDisplayHandle, XlibWindowHandle,
    };

    use super::super::super::shared::wire::write_err;
    use super::super::scope_token::with_full_scope_or_err;

    run_host_extern_c(
        "host_gpu_full_create_present_target",
        || -> i32 {
            if out_present_target.is_null() || window.is_null() {
                write_err(
                    "create_present_target: null window / out_present_target",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: `window` non-null per the guard; read once by ref (POD).
            let repr = unsafe { &*window };
            let shim = match repr.kind {
                0 => {
                    // Xlib: window is an XID (c_ulong); display is optional.
                    let wh = XlibWindowHandle::new(repr.window_or_surface as c_ulong);
                    let display = NonNull::new(repr.display_or_connection as *mut c_void);
                    let dh = XlibDisplayHandle::new(display, repr.screen as c_int);
                    RawWindowHandleShim {
                        window: RawWindowHandle::Xlib(wh),
                        display: RawDisplayHandle::Xlib(dh),
                    }
                }
                1 => {
                    // Xcb: window is a non-zero u32; connection is optional.
                    let Some(xcb_window) = NonZeroU32::new(repr.window_or_surface as u32) else {
                        write_err(
                            "create_present_target: Xcb window handle is zero (invalid)",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    };
                    let wh = XcbWindowHandle::new(xcb_window);
                    let connection = NonNull::new(repr.display_or_connection as *mut c_void);
                    let dh = XcbDisplayHandle::new(connection, repr.screen as c_int);
                    RawWindowHandleShim {
                        window: RawWindowHandle::Xcb(wh),
                        display: RawDisplayHandle::Xcb(dh),
                    }
                }
                2 => {
                    // Wayland: wl_surface* + wl_display* both required.
                    let Some(surface) = NonNull::new(repr.window_or_surface as *mut c_void) else {
                        write_err(
                            "create_present_target: Wayland wl_surface pointer is null (invalid)",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    };
                    let Some(display) = NonNull::new(repr.display_or_connection as *mut c_void)
                    else {
                        write_err(
                            "create_present_target: Wayland wl_display pointer is null (invalid)",
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        return 1;
                    };
                    let wh = WaylandWindowHandle::new(surface);
                    let dh = WaylandDisplayHandle::new(display);
                    RawWindowHandleShim {
                        window: RawWindowHandle::Wayland(wh),
                        display: RawDisplayHandle::Wayland(dh),
                    }
                }
                3 | 4 => {
                    // Win32 (3) / AppKit (4) discriminants reserved from day
                    // one — activation lands only a new dispatch arm here,
                    // never an ABI layout bump. Apple's display path is
                    // CAMetalLayer, outside this surface.
                    return not_yet_provided(
                        "create_present_target (Win32/AppKit reserved)",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                }
                other => {
                    write_err(
                        &format!("create_present_target: unknown window-handle kind {other}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };

            let color_traits =
                super::super::super::present_target::color_traits_from_repr(color);
            let result = with_full_scope_or_err(
                scope_token,
                "create_present_target",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_present_target(&shim, width, height, vsync != 0, color_traits.as_ref()),
            );
            match result {
                Some(Ok(present_target)) => {
                    // SAFETY: `out_present_target` is the caller's
                    // `PresentTarget` slot — a `#[repr(C)]` 32-byte POD
                    // (handle, vtable, methods_vtable, color_format_raw,
                    // padding). Written by value; the cdylib's Drop later
                    // dispatches `drop_present_target` (`Box::from_raw` +
                    // drop host-side).
                    unsafe {
                        std::ptr::write(
                            out_present_target as *mut crate::vulkan::rhi::PresentTarget,
                            present_target,
                        );
                    }
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

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_present_target(
    _gpu_handle: *const c_void,
    _window: *const RawWindowHandleRepr,
    _width: u32,
    _height: u32,
    _vsync: u32,
    _color: *const ColorTraitsRepr,
    _out_present_target: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_present_target",
        || not_yet_provided("create_present_target", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_present_target(
    owned_handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_present_target",
        || {
            if owned_handle.is_null() {
                return;
            }
            // SAFETY: `owned_handle` is
            // `Box::into_raw(Box<PresentTargetInner>)`-shaped
            // (`Box<Mutex<VulkanPresentTarget>>`) from
            // `PresentTarget::from_target`. Reconstruct the Box and let
            // Drop run — every `vkDestroySwapchainKHR` /
            // `vkDestroySurfaceKHR` / semaphore teardown runs host-side.
            unsafe {
                let _ = Box::from_raw(owned_handle as *mut crate::vulkan::rhi::PresentTargetInner);
            }
        },
        (),
    )
}

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_present_target(
    owned_handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_present_target",
        || {
            let _ = owned_handle;
        },
        (),
    )
}

// ============================================================================
// Hardware video encode/decode (#1259)
// ============================================================================

#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_encoder_session(
    _gpu_handle: *const c_void,
    _desc: *const VideoEncoderSessionDescriptorRepr,
    _out_session: *mut *const c_void,
    _out_aligned_width: *mut u32,
    _out_aligned_height: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_encoder_session",
        || not_yet_provided("create_encoder_session", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_encoder_session(
    owned_handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_encoder_session",
        || {
            let _ = owned_handle;
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_decoder_session(
    _gpu_handle: *const c_void,
    _desc: *const VideoDecoderSessionDescriptorRepr,
    _out_session: *mut *const c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_decoder_session",
        || not_yet_provided("create_decoder_session", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_drop_decoder_session(
    owned_handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_full_drop_decoder_session",
        || {
            let _ = owned_handle;
        },
        (),
    )
}

// ============================================================================
// Exportable timeline semaphore (#1260)
// ============================================================================

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_exportable_timeline_semaphore(
    gpu_handle: *const c_void,
    initial_value: u64,
    out_timeline: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_exportable_timeline_semaphore",
        || -> i32 {
            if out_timeline.is_null() {
                write_err(
                    "create_exportable_timeline_semaphore: null out_timeline pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                gpu_handle,
                "create_exportable_timeline_semaphore",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_exportable_timeline_semaphore(initial_value),
            );
            match result {
                Some(Ok(arc)) => {
                    let wire = crate::core::rhi::HostTimelineSemaphore::from_arc(arc);
                    // SAFETY: `out_timeline` checked non-null; the cdylib
                    // provided a 16-byte `MaybeUninit<HostTimelineSemaphore>`
                    // slot. `ptr::write` moves the wire envelope in without
                    // dropping the uninitialized destination.
                    unsafe {
                        std::ptr::write(
                            out_timeline as *mut crate::core::rhi::HostTimelineSemaphore,
                            wire,
                        )
                    };
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
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_exportable_timeline_semaphore(
    _gpu_handle: *const c_void,
    _initial_value: u64,
    _out_timeline: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_exportable_timeline_semaphore",
        || {
            write_err(
                "create_exportable_timeline_semaphore: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

// Texture readback (#1261) is no longer reserved — its real host bodies
// (`host_gpu_full_create_texture_readback` /
// `host_gpu_full_drop_texture_readback`) landed in the sibling
// `texture_readback` module against the frozen v11 slots.

// ============================================================================
// OPAQUE_FD / CUDA buffer surface (#1262)
// ============================================================================

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_opaque_fd_export_buffer(
    scope_token: *const c_void,
    byte_size: u64,
    device_local: u8,
    out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_opaque_fd_export_buffer",
        || -> i32 {
            if out_buffer.is_null() {
                write_err(
                    "create_opaque_fd_export_buffer: null out_buffer pointer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if byte_size == 0 {
                write_err(
                    "create_opaque_fd_export_buffer: byte_size must be > 0",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let result = with_full_scope_or_err(
                scope_token,
                "create_opaque_fd_export_buffer",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.create_opaque_fd_export_buffer(byte_size, device_local != 0),
            );
            match result {
                Some(Ok(buf)) => {
                    // SAFETY: host wrote the 32-byte `StorageBuffer`
                    // PluginAbiObject into the caller's slot; its cached
                    // `byte_size_cached` is populated by
                    // `from_host_vulkan_buffer` (never a zeroed borrow).
                    unsafe {
                        std::ptr::write(out_buffer as *mut crate::core::rhi::StorageBuffer, buf);
                    }
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

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_create_opaque_fd_export_buffer(
    _scope_token: *const c_void,
    _byte_size: u64,
    _device_local: u8,
    _out_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_create_opaque_fd_export_buffer",
        || write_err_non_linux("create_opaque_fd_export_buffer", err_buf, err_buf_cap, err_len),
        1,
    )
}

#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_export_storage_buffer_opaque_fd(
    scope_token: *const c_void,
    buffer: *const c_void,
    out_descriptor: *mut OpaqueFdExportDescriptorRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_export_storage_buffer_opaque_fd",
        || -> i32 {
            // FD-failure convention: write `fd = -1` up-front so every
            // error path leaves the sentinel and a caller never reads a
            // stale live fd from the descriptor (double-close guard).
            if !out_descriptor.is_null() {
                // SAFETY: caller-provided out-pointer, writable for the
                // descriptor.
                unsafe { (*out_descriptor).fd = -1 };
            }
            if out_descriptor.is_null() || buffer.is_null() {
                write_err(
                    "export_storage_buffer_opaque_fd: null buffer / out_descriptor",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: `buffer` is a borrowed `*const StorageBuffer` from
            // the cdylib, valid for the call.
            let sb = unsafe { &*(buffer as *const crate::core::rhi::StorageBuffer) };
            let result = with_full_scope_or_err(
                scope_token,
                "export_storage_buffer_opaque_fd",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| gpu.export_storage_buffer_opaque_fd(sb),
            );
            match result {
                Some(Ok((fd, size, uuid))) => {
                    // SAFETY: out_descriptor checked non-null above.
                    unsafe {
                        let d = &mut *out_descriptor;
                        d.fd = fd;
                        d.handle_type_raw = OPAQUE_FD_HANDLE_TYPE_RAW;
                        d.size = size;
                        d.device_uuid = uuid;
                    }
                    0
                }
                // fd already written -1 on entry; leave it on both error paths.
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

#[cfg(not(target_os = "linux"))]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_export_storage_buffer_opaque_fd(
    _scope_token: *const c_void,
    _buffer: *const c_void,
    out_descriptor: *mut OpaqueFdExportDescriptorRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_export_storage_buffer_opaque_fd",
        || -> i32 {
            if !out_descriptor.is_null() {
                // SAFETY: caller-provided out-pointer.
                unsafe { (*out_descriptor).fd = -1 };
            }
            write_err_non_linux(
                "export_storage_buffer_opaque_fd",
                err_buf,
                err_buf_cap,
                err_len,
            )
        },
        1,
    )
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_wrap_storage_buffer_as_pixel_buffer(
    scope_token: *const c_void,
    storage_buffer: *const c_void,
    width: u32,
    height: u32,
    bytes_per_pixel: u32,
    format_raw: u32,
    out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_wrap_storage_buffer_as_pixel_buffer",
        || -> i32 {
            if out_pixel_buffer.is_null() || storage_buffer.is_null() {
                write_err(
                    "wrap_storage_buffer_as_pixel_buffer: null storage_buffer / out_pixel_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let format = match pixel_format_from_raw(format_raw) {
                Some(f) => f,
                None => {
                    write_err(
                        &format!(
                            "wrap_storage_buffer_as_pixel_buffer: invalid format_raw {format_raw}"
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // SAFETY: borrowed `*const StorageBuffer` for the call.
            let sb = unsafe { &*(storage_buffer as *const crate::core::rhi::StorageBuffer) };
            // Reject degenerate / out-of-bounds pixel shapes BEFORE
            // `PixelBuffer::from_host_vulkan_buffer` caches the dims. A
            // `PixelBuffer` whose cached `width*height*bytes_per_pixel`
            // claims more bytes than the backing OPAQUE_FD `VkBuffer`
            // holds would let a downstream consumer read past the
            // allocation (silent OOB read, no panic / validation
            // complaint). Symmetric with `create_opaque_fd_export_buffer`'s
            // `byte_size > 0` guard. `sb.byte_size()` is a pure cached-POD
            // read (no ABI hop, no handle deref).
            if width == 0 || height == 0 || bytes_per_pixel == 0 {
                write_err(
                    &format!(
                        "wrap_storage_buffer_as_pixel_buffer: zero dimension \
                         (width={width}, height={height}, bytes_per_pixel={bytes_per_pixel})"
                    ),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let required_bytes = (width as u64)
                .checked_mul(height as u64)
                .and_then(|wh| wh.checked_mul(bytes_per_pixel as u64));
            match required_bytes {
                Some(required) if required <= sb.byte_size() => {}
                _ => {
                    write_err(
                        &format!(
                            "wrap_storage_buffer_as_pixel_buffer: pixel shape \
                             {width}x{height}x{bytes_per_pixel} requires {} bytes, \
                             exceeds storage buffer byte_size {}",
                            required_bytes
                                .map(|r| r.to_string())
                                .unwrap_or_else(|| "u64-overflow".to_string()),
                            sb.byte_size()
                        ),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            }
            let result = with_full_scope_or_err(
                scope_token,
                "wrap_storage_buffer_as_pixel_buffer",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| {
                    gpu.wrap_storage_buffer_as_pixel_buffer(
                        sb,
                        width,
                        height,
                        bytes_per_pixel,
                        format,
                    )
                },
            );
            match result {
                Some(Ok(pb)) => {
                    // SAFETY: host wrote the `PixelBuffer` PluginAbiObject
                    // (cached width/height/format from the caller's inputs)
                    // into the caller's slot.
                    unsafe {
                        std::ptr::write(out_pixel_buffer as *mut crate::core::rhi::PixelBuffer, pb);
                    }
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

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_wrap_storage_buffer_as_pixel_buffer(
    _scope_token: *const c_void,
    _storage_buffer: *const c_void,
    _width: u32,
    _height: u32,
    _bytes_per_pixel: u32,
    _format_raw: u32,
    _out_pixel_buffer: *mut c_void,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_wrap_storage_buffer_as_pixel_buffer",
        || {
            write_err_non_linux(
                "wrap_storage_buffer_as_pixel_buffer",
                err_buf,
                err_buf_cap,
                err_len,
            )
        },
        1,
    )
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_copy_texture_to_storage_buffer_and_signal(
    scope_token: *const c_void,
    texture_handle: *const c_void,
    source_layout_raw: i32,
    storage_buffer: *const c_void,
    consume_done_handle: *const c_void,
    consume_done_wait_value: u64,
    produce_done_handle: *const c_void,
    produce_done_signal_value: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    use std::sync::Arc;
    run_host_extern_c(
        "host_gpu_full_copy_texture_to_storage_buffer_and_signal",
        || -> i32 {
            if texture_handle.is_null() || storage_buffer.is_null() {
                write_err(
                    "copy_texture_to_storage_buffer_and_signal: null texture_handle / storage_buffer",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            // SAFETY: borrowed `*const StorageBuffer` for the call.
            let dst = unsafe { &*(storage_buffer as *const crate::core::rhi::StorageBuffer) };
            // Reconstruct a borrowed source `Texture` from the inner-Arc
            // handle. `texture_handle` is the `Texture` PluginAbiObject's
            // `handle` field — `Arc::into_raw(Arc<TextureInner>)` (same
            // shape `host_vulkan_texture_arc` consumes). Bump the strong
            // count, reconstruct the Arc, and re-wrap it as a `Texture`
            // whose own `Drop` (via the limited-access `drop_texture` slot)
            // balances the bump.
            // SAFETY: handle is a live `Arc::into_raw(Arc<TextureInner>)`.
            let texture = unsafe {
                let ptr = texture_handle as *const crate::core::rhi::texture::TextureInner;
                Arc::increment_strong_count(ptr);
                let arc = Arc::from_raw(ptr);
                let width = arc.width();
                let height = arc.height();
                let format = arc.format();
                crate::core::rhi::Texture::from_arc_into_raw(arc, width, height, format)
            };
            let source_layout = crate::core::rhi::VulkanLayout(source_layout_raw);
            // Timeline handles: null = none. A non-null handle is
            // `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)` (the
            // exportable-timeline PluginAbiObject's inner handle, minted by
            // #1260); borrow without taking ownership.
            let consume = if consume_done_handle.is_null() {
                None
            } else {
                // SAFETY: borrowed timeline handle valid for the call.
                Some((
                    unsafe {
                        &*(consume_done_handle
                            as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore)
                    },
                    consume_done_wait_value,
                ))
            };
            let produce = if produce_done_handle.is_null() {
                None
            } else {
                // SAFETY: borrowed timeline handle valid for the call.
                Some((
                    unsafe {
                        &*(produce_done_handle
                            as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore)
                    },
                    produce_done_signal_value,
                ))
            };
            let result = with_full_scope_or_err(
                scope_token,
                "copy_texture_to_storage_buffer_and_signal",
                err_buf,
                err_buf_cap,
                err_len,
                |gpu| {
                    gpu.copy_texture_to_storage_buffer_and_signal(
                        &texture,
                        source_layout,
                        dst,
                        consume,
                        produce,
                    )
                },
            );
            match result {
                Some(Ok(())) => 0,
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

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_full_copy_texture_to_storage_buffer_and_signal(
    _scope_token: *const c_void,
    _texture_handle: *const c_void,
    _source_layout_raw: i32,
    _storage_buffer: *const c_void,
    _consume_done_handle: *const c_void,
    _consume_done_wait_value: u64,
    _produce_done_handle: *const c_void,
    _produce_done_signal_value: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_full_copy_texture_to_storage_buffer_and_signal",
        || {
            write_err_non_linux(
                "copy_texture_to_storage_buffer_and_signal",
                err_buf,
                err_buf_cap,
                err_len,
            )
        },
        1,
    )
}

/// Shared "not available on this platform" writer for the non-Linux
/// OPAQUE_FD/CUDA slot stubs (the surface is Linux-only RHI).
#[cfg(not(target_os = "linux"))]
fn write_err_non_linux(slot: &str, err_buf: *mut u8, err_buf_cap: usize, err_len: *mut usize) -> i32 {
    super::super::super::shared::wire::write_err(
        &format!("{slot}: not available on this platform"),
        err_buf,
        err_buf_cap,
        err_len,
    );
    1
}

#[cfg(test)]
mod reserved_m32_wire_format_tests {
    //! Tier-1 wire-format tests for the v11 FullAccess slots.
    //!
    //! The still-reserved slots (present target, hardware video,
    //! exportable timeline, texture readback) must return
    //! [`NOT_YET_PROVIDED_RC`] with a "not yet provided" message; each
    //! drop slot is a null-safe no-op. The landed OPAQUE_FD/CUDA slots
    //! (#1262) instead assert their GPU-free guard paths (null-handle,
    //! null-out-param, invalid-args, invalid-scope); their positive
    //! mint/copy paths are GPU-gated integration tests.
    //!
    //! Mental-revert: replace a still-reserved stub body with
    //! `unimplemented!()` and the matching test aborts the process
    //! instead of asserting the typed refusal; drop a landed-slot guard
    //! and the matching test trips on a UB deref or an unchecked scope
    //! hit.

    use std::ffi::c_void;

    use super::super::super::HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE;
    use super::NOT_YET_PROVIDED_RC;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    /// Tier-1 wire test: a null out-param short-circuits to a typed
    /// error before any scope / window decode. Mentally reverting the
    /// null guard to a deref segfaults instead of returning rc 1.
    #[cfg(target_os = "linux")]
    #[test]
    fn create_present_target_null_out_is_typed_error() {
        let (mut buf, mut len) = make_err_buf();
        // Non-null window, null out → the null-out branch fires.
        let window = streamlib_plugin_abi::RawWindowHandleRepr::default();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_present_target)(
                std::ptr::null(),
                &window,
                64,
                64,
                1,
                std::ptr::null(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("null window / out_present_target"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    /// Tier-1 wire test: the reserved Win32 (3) / AppKit (4)
    /// discriminants return the typed not-yet-provided refusal BEFORE
    /// scope resolution — activation lands a new dispatch arm, never a
    /// layout bump.
    #[cfg(target_os = "linux")]
    #[test]
    fn create_present_target_reserved_win32_appkit_is_not_yet_provided() {
        for kind in [3u32, 4u32] {
            let (mut buf, mut len) = make_err_buf();
            let mut out = [0u8; 32];
            let window = streamlib_plugin_abi::RawWindowHandleRepr {
                kind,
                window_or_surface: 0x1000,
                ..Default::default()
            };
            let rc = unsafe {
                (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_present_target)(
                    std::ptr::null(),
                    &window,
                    64,
                    64,
                    1,
                    std::ptr::null(),
                    out.as_mut_ptr() as *mut c_void,
                    buf.as_mut_ptr(),
                    buf.len(),
                    &mut len,
                )
            };
            assert_eq!(rc, NOT_YET_PROVIDED_RC, "kind {kind}");
            assert!(
                err_buf_as_str(&buf, len).contains("Win32/AppKit reserved"),
                "kind {kind} got: {}",
                err_buf_as_str(&buf, len)
            );
        }
    }

    /// Tier-1 wire test: an out-of-range window-handle kind is a typed
    /// invalid-args error (distinct from the reserved-discriminant path).
    #[cfg(target_os = "linux")]
    #[test]
    fn create_present_target_unknown_kind_is_typed_error() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 32];
        let window = streamlib_plugin_abi::RawWindowHandleRepr {
            kind: 99,
            ..Default::default()
        };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_present_target)(
                std::ptr::null(),
                &window,
                64,
                64,
                1,
                std::ptr::null(),
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("unknown window-handle kind 99"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn create_encoder_session_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let mut aw: u32 = 0;
        let mut ah: u32 = 0;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_encoder_session)(
                std::ptr::null(),
                std::ptr::null(),
                &mut out,
                &mut aw,
                &mut ah,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("create_encoder_session: not yet provided"));
    }

    #[test]
    fn create_decoder_session_reports_not_yet_provided() {
        let (mut buf, mut len) = make_err_buf();
        let mut out: *const c_void = std::ptr::null();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_decoder_session)(
                std::ptr::null(),
                std::ptr::null(),
                &mut out,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, NOT_YET_PROVIDED_RC);
        assert!(err_buf_as_str(&buf, len).contains("create_decoder_session: not yet provided"));
    }

    // #1260 landed the exportable-timeline mint slot: the reserved
    // NotYetProvided stub is replaced by a real host body. These two
    // GPU-free wire tests lock the arg-guard behavior (the positive mint
    // round-trip is hardware-gated in `tests/`). Mental-revert: drop the
    // `out_timeline.is_null()` guard and the null-out test below UB-writes
    // a 16-byte `HostTimelineSemaphore` through a null pointer.
    #[test]
    fn create_exportable_timeline_semaphore_reports_null_out_param() {
        let (mut buf, mut len) = make_err_buf();
        // Null scope token is fine here — the null-out guard fires first.
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_exportable_timeline_semaphore)(
                std::ptr::null(),
                0,
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(
            err_buf_as_str(&buf, len)
                .contains("create_exportable_timeline_semaphore: null out_timeline pointer"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
        #[cfg(not(target_os = "linux"))]
        assert!(
            err_buf_as_str(&buf, len)
                .contains("create_exportable_timeline_semaphore: not available on this platform")
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn create_exportable_timeline_semaphore_reports_invalid_scope_on_null_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 16];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_exportable_timeline_semaphore)(
                std::ptr::null(),
                0,
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("create_exportable_timeline_semaphore: invalid escalate scope"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    // ========================================================================
    // OPAQUE_FD / CUDA buffer surface (#1262) — real host bodies.
    //
    // These slots run inside an escalate scope; the positive mint/copy
    // paths need a live GPU and are GPU-gated (integration tests). The
    // GPU-free wire tests below lock the null-handle / null-out-param /
    // invalid-args / invalid-scope guards that fire before any device
    // work. Mental-revert: dropping a guard turns the matching assertion
    // into a UB deref (SIGSEGV) or an unchecked scope hit.
    // ========================================================================

    #[test]
    #[cfg(target_os = "linux")]
    fn create_opaque_fd_export_buffer_rejects_null_out_param() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_opaque_fd_export_buffer)(
                std::ptr::null(),
                4096,
                1,
                std::ptr::null_mut(), // null out_buffer
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("create_opaque_fd_export_buffer: null out_buffer"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn create_opaque_fd_export_buffer_rejects_zero_byte_size() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 32];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_opaque_fd_export_buffer)(
                std::ptr::null(),
                0, // invalid byte_size
                1,
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("byte_size must be > 0"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn create_opaque_fd_export_buffer_rejects_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 32];
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.create_opaque_fd_export_buffer)(
                std::ptr::null(), // null scope token
                4096,
                1,
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("create_opaque_fd_export_buffer: invalid escalate scope"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn export_storage_buffer_opaque_fd_writes_minus_one_fd_on_null_scope() {
        let (mut buf, mut len) = make_err_buf();
        let mut desc = streamlib_plugin_abi::OpaqueFdExportDescriptorRepr {
            fd: 7, // start non-negative to prove the body writes -1
            handle_type_raw: 0,
            size: 0,
            device_uuid: [0u8; 16],
        };
        // Aligned 32-byte backing so the body can *form* (never read) a
        // `&StorageBuffer` before the null-scope check fires — the scope
        // closure that would deref its fields never runs on a null token.
        let backing = [0u64; 4];
        let buffer_ptr = backing.as_ptr() as *const c_void;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.export_storage_buffer_opaque_fd)(
                std::ptr::null(),
                buffer_ptr,
                &mut desc,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        // FD-failure convention: fd written -1 on any non-zero return.
        assert_eq!(desc.fd, -1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("export_storage_buffer_opaque_fd: invalid escalate scope"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn export_storage_buffer_opaque_fd_writes_minus_one_fd_on_null_buffer() {
        let (mut buf, mut len) = make_err_buf();
        let mut desc = streamlib_plugin_abi::OpaqueFdExportDescriptorRepr {
            fd: 7,
            handle_type_raw: 0,
            size: 0,
            device_uuid: [0u8; 16],
        };
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.export_storage_buffer_opaque_fd)(
                std::ptr::null(),
                std::ptr::null(), // null buffer
                &mut desc,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert_eq!(desc.fd, -1);
        assert!(
            err_buf_as_str(&buf, len).contains("null buffer / out_descriptor"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn wrap_storage_buffer_as_pixel_buffer_rejects_null_out_param() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.wrap_storage_buffer_as_pixel_buffer)(
                std::ptr::null(),
                std::ptr::null(),
                64,
                64,
                4,
                0x42475241, // Bgra32
                std::ptr::null_mut(), // null out_pixel_buffer
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("null storage_buffer / out_pixel_buffer"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn wrap_storage_buffer_as_pixel_buffer_rejects_invalid_format() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 64];
        let bogus_storage = 0x1usize as *const c_void;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.wrap_storage_buffer_as_pixel_buffer)(
                std::ptr::null(),
                bogus_storage,
                64,
                64,
                4,
                0xDEAD_BEEF, // invalid format_raw
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("wrap_storage_buffer_as_pixel_buffer: invalid format_raw"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn wrap_storage_buffer_as_pixel_buffer_rejects_null_scope_token() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 64];
        // Aligned 32-byte `StorageBuffer`-shaped backing whose cached
        // `byte_size` (offset 16 → `[u64; 4]` index 2) is large enough
        // to clear the dimension guard, so the null-scope check is what
        // fires — not the zero-dim / oversized guards ahead of it.
        // `byte_size()` is a pure POD read of this field (no handle
        // deref).
        let mut backing = [0u64; 4];
        backing[2] = 64 * 64 * 4; // byte_size_cached
        let storage_ptr = backing.as_ptr() as *const c_void;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.wrap_storage_buffer_as_pixel_buffer)(
                std::ptr::null(),
                storage_ptr,
                64,
                64,
                4,
                0x42475241, // Bgra32 (valid — pushes past the format decode)
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("wrap_storage_buffer_as_pixel_buffer: invalid escalate scope"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn wrap_storage_buffer_as_pixel_buffer_rejects_zero_dimension() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 64];
        // Aligned `StorageBuffer`-shaped backing with a generous cached
        // byte_size — the zero-dimension guard must fire before the
        // oversized guard and before scope resolution.
        let mut backing = [0u64; 4];
        backing[2] = 1 << 20; // byte_size_cached
        let storage_ptr = backing.as_ptr() as *const c_void;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.wrap_storage_buffer_as_pixel_buffer)(
                std::ptr::null(),
                storage_ptr,
                0, // zero width
                64,
                4,
                0x42475241, // Bgra32 (valid — pushes past the format decode)
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len)
                .contains("wrap_storage_buffer_as_pixel_buffer: zero dimension"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn wrap_storage_buffer_as_pixel_buffer_rejects_oversized_shape() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 64];
        // Aligned `StorageBuffer`-shaped backing whose cached byte_size
        // (4096) is smaller than the requested pixel shape
        // (64*64*4 = 16384) — the oversized guard must fire, blocking a
        // `PixelBuffer` that would claim more bytes than the backing
        // OPAQUE_FD buffer holds.
        let mut backing = [0u64; 4];
        backing[2] = 4096; // byte_size_cached < 64*64*4
        let storage_ptr = backing.as_ptr() as *const c_void;
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.wrap_storage_buffer_as_pixel_buffer)(
                std::ptr::null(),
                storage_ptr,
                64,
                64,
                4,
                0x42475241, // Bgra32 (valid — pushes past the format decode)
                out.as_mut_ptr() as *mut c_void,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains(
                "wrap_storage_buffer_as_pixel_buffer: pixel shape 64x64x4 requires 16384 bytes, \
                 exceeds storage buffer byte_size 4096"
            ),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn copy_texture_to_storage_buffer_and_signal_rejects_null_handles() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.copy_texture_to_storage_buffer_and_signal)(
                std::ptr::null(),
                std::ptr::null(), // null texture_handle
                0,
                std::ptr::null(), // null storage_buffer
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
            err_buf_as_str(&buf, len).contains("null texture_handle / storage_buffer"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[test]
    fn drop_slots_are_null_safe_no_ops() {
        // Every drop slot must be a null-safe no-op — a caller dropping a
        // never-populated handle (e.g. after a failed create) must not
        // deref or panic. `drop_texture_readback`'s real body landed with
        // #1261 but its null path is still part of this contract.
        unsafe {
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.drop_present_target)(std::ptr::null());
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.drop_encoder_session)(std::ptr::null());
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.drop_decoder_session)(std::ptr::null());
            (HOST_GPU_CONTEXT_FULL_ACCESS_VTABLE.drop_texture_readback)(std::ptr::null());
        }
    }
}
