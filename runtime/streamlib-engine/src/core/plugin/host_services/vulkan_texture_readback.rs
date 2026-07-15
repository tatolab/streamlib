// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `VulkanTextureReadbackMethodsVTable` callbacks + static +
//! accessor (M32 #1261 fill-in against the reserved v1 methods vtable).
//!
//! Each slot resolves the boxed `Arc<VulkanTextureReadback>` from the
//! opaque `readback_handle` (the primitive clones its own
//! `Arc<HostVulkanDevice>` at construction, so the handle — not a gpu
//! scope token — is the key), then drives the engine primitive:
//!
//! - `submit` reconstructs the borrowed `Texture` PluginAbiObject from
//!   `texture_handle` via the make-borrow convention (cached POD
//!   populated from the inner — the #988 zeroed-borrow contract), maps
//!   the raw `VkImageLayout` `source_layout_raw` to a
//!   [`crate::core::rhi::TextureSourceLayout`] (typed error on an
//!   unsupported layout), and issues a ticket.
//! - `try_read` / `wait_and_read` hand back a raw borrow into the host
//!   persistent-mapped staging (row stride = `width × bytes_per_pixel`,
//!   no padding — tightly packed; the borrow is valid only until the
//!   next `submit` on the same handle).
//! - `try_read_copy` / `wait_and_copy` COPY into a caller buffer for
//!   plugins that must outlive the borrow window; `status 2` =
//!   `out_buf` too small (required length written to `out_len`, and the
//!   in-flight state is preserved so a retry with a larger buffer
//!   succeeds).
//!
//! Every body wraps in `run_host_extern_c` so a panic never unwinds
//! across the ABI.

use std::ffi::c_void;

use streamlib_plugin_abi::{
    VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION, VulkanTextureReadbackMethodsVTable,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::write_err;

#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use super::shared::borrow::make_texture_borrow;
#[cfg(target_os = "linux")]
use crate::core::rhi::{ReadbackTicket, TextureSourceLayout};

// ============================================================================
// Real host bodies (Linux-only — `VulkanTextureReadback` is Linux-only).
// ============================================================================

/// SAFETY helper: borrow the boxed `Arc<VulkanTextureReadback>` behind
/// the opaque handle without taking ownership (drop is the authority of
/// `drop_texture_readback`).
#[cfg(target_os = "linux")]
unsafe fn readback_arc<'a>(
    readback_handle: *const c_void,
) -> &'a Arc<crate::vulkan::rhi::VulkanTextureReadback> {
    // SAFETY: `readback_handle` is
    // `Box::into_raw(Box<Arc<VulkanTextureReadback>>)`; borrowing the
    // inner Arc through `&*` is sound host-side and never takes ownership.
    unsafe { &*(readback_handle as *const Arc<crate::vulkan::rhi::VulkanTextureReadback>) }
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_submit(
    readback_handle: *const c_void,
    texture_handle: *const c_void,
    source_layout_raw: i32,
    out_ticket_handle_id: *mut u64,
    out_ticket_counter: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_submit",
        || -> i32 {
            if readback_handle.is_null() {
                write_err("submit: null readback handle", err_buf, err_buf_cap, err_len);
                return 1;
            }
            if texture_handle.is_null() {
                write_err("submit: null texture handle", err_buf, err_buf_cap, err_len);
                return 1;
            }
            if out_ticket_handle_id.is_null() || out_ticket_counter.is_null() {
                write_err("submit: null out pointer", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let layout = match TextureSourceLayout::from_vulkan_layout_raw(source_layout_raw) {
                Some(l) => l,
                None => {
                    write_err(
                        &format!("submit: unsupported source_layout_raw {source_layout_raw}"),
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let arc = unsafe { readback_arc(readback_handle) };
            // Reconstruct the borrowed Texture PluginAbiObject with cached
            // POD populated from the inner (the #988 make-borrow
            // contract) — `submit` validates via `format()/width()/height()`.
            let texture = make_texture_borrow(texture_handle);
            match arc.submit(&texture, layout) {
                Ok(ticket) => {
                    // SAFETY: out pointers null-checked above.
                    unsafe {
                        std::ptr::write(out_ticket_handle_id, ticket.handle_id);
                        std::ptr::write(out_ticket_counter, ticket.counter);
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_try_read(
    readback_handle: *const c_void,
    ticket_handle_id: u64,
    ticket_counter: u64,
    out_ready: *mut u32,
    out_bytes_ptr: *mut *const u8,
    out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_try_read",
        || -> i32 {
            if readback_handle.is_null() {
                write_err("try_read: null readback handle", err_buf, err_buf_cap, err_len);
                return 1;
            }
            if out_ready.is_null() || out_bytes_ptr.is_null() || out_len.is_null() {
                write_err("try_read: null out pointer", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let arc = unsafe { readback_arc(readback_handle) };
            let ticket = ReadbackTicket {
                handle_id: ticket_handle_id,
                counter: ticket_counter,
            };
            match arc.try_read(ticket) {
                Ok(Some(bytes)) => {
                    // SAFETY: out pointers null-checked above; `bytes`
                    // borrows the host persistent-mapped staging (valid
                    // until the next submit on this handle).
                    unsafe {
                        std::ptr::write(out_ready, 1);
                        std::ptr::write(out_bytes_ptr, bytes.as_ptr());
                        std::ptr::write(out_len, bytes.len());
                    }
                    0
                }
                Ok(None) => {
                    // SAFETY: out pointers null-checked above.
                    unsafe {
                        std::ptr::write(out_ready, 0);
                        std::ptr::write(out_bytes_ptr, std::ptr::null());
                        std::ptr::write(out_len, 0);
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_wait_and_read(
    readback_handle: *const c_void,
    ticket_handle_id: u64,
    ticket_counter: u64,
    timeout_ns: u64,
    out_bytes_ptr: *mut *const u8,
    out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_wait_and_read",
        || -> i32 {
            if readback_handle.is_null() {
                write_err(
                    "wait_and_read: null readback handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if out_bytes_ptr.is_null() || out_len.is_null() {
                write_err("wait_and_read: null out pointer", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let arc = unsafe { readback_arc(readback_handle) };
            let ticket = ReadbackTicket {
                handle_id: ticket_handle_id,
                counter: ticket_counter,
            };
            match arc.wait_and_read(ticket, timeout_ns) {
                Ok(bytes) => {
                    // SAFETY: out pointers null-checked above; `bytes`
                    // borrows the host persistent-mapped staging.
                    unsafe {
                        std::ptr::write(out_bytes_ptr, bytes.as_ptr());
                        std::ptr::write(out_len, bytes.len());
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_try_read_copy(
    readback_handle: *const c_void,
    ticket_handle_id: u64,
    ticket_counter: u64,
    out_ready: *mut u32,
    out_buf: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_try_read_copy",
        || -> i32 {
            if readback_handle.is_null() {
                write_err(
                    "try_read_copy: null readback handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if out_ready.is_null() || out_len.is_null() {
                write_err("try_read_copy: null out pointer", err_buf, err_buf_cap, err_len);
                return 1;
            }
            // Null `out_buf` would otherwise pass the size gate below (when
            // `out_cap >= required`), then the consuming read would drop
            // the frame while the copy is skipped — reporting success
            // (out_ready=1) with zero bytes delivered (silent data loss on
            // the public ABI). Reject BEFORE the consuming read; the twin
            // always passes a non-null buffer (a too-small one drives the
            // status-2 grow-and-retry), so a null here is always a caller
            // bug, never a size query.
            if out_buf.is_null() {
                write_err("try_read_copy: null out_buf", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let arc = unsafe { readback_arc(readback_handle) };
            // Size-check BEFORE the consuming read so an undersized
            // buffer (status 2) leaves the in-flight state intact for a
            // retry with a larger buffer. The required length is the
            // primitive's staging size — never recomputed here.
            let required = arc.staging_size() as usize;
            // SAFETY: out_len null-checked above.
            unsafe { std::ptr::write(out_len, required) };
            if out_cap < required {
                write_err(
                    &format!("try_read_copy: out_buf too small (need {required}, have {out_cap})"),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 2;
            }
            let ticket = ReadbackTicket {
                handle_id: ticket_handle_id,
                counter: ticket_counter,
            };
            match arc.try_read(ticket) {
                Ok(Some(bytes)) => {
                    // SAFETY: `out_cap >= required == bytes.len()` per the
                    // size-check; out_buf null-checked above and writable
                    // for `out_cap` bytes.
                    unsafe {
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, bytes.len());
                        std::ptr::write(out_len, bytes.len());
                        std::ptr::write(out_ready, 1);
                    }
                    0
                }
                Ok(None) => {
                    unsafe { std::ptr::write(out_ready, 0) };
                    0
                }
                Err(e) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_wait_and_copy(
    readback_handle: *const c_void,
    ticket_handle_id: u64,
    ticket_counter: u64,
    timeout_ns: u64,
    out_buf: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_wait_and_copy",
        || -> i32 {
            if readback_handle.is_null() {
                write_err(
                    "wait_and_copy: null readback handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if out_len.is_null() {
                write_err("wait_and_copy: null out pointer", err_buf, err_buf_cap, err_len);
                return 1;
            }
            // Null `out_buf` would otherwise pass the size gate below (when
            // `out_cap >= required`), then the blocking consuming read
            // would drop the frame while the copy is skipped — reporting
            // success with zero bytes delivered (silent data loss on the
            // public ABI). Reject BEFORE the consuming read; the twin
            // always passes a non-null buffer (a too-small one drives the
            // status-2 grow-and-retry), so a null here is always a caller
            // bug, never a size query.
            if out_buf.is_null() {
                write_err("wait_and_copy: null out_buf", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let arc = unsafe { readback_arc(readback_handle) };
            // Size-check before the (blocking) consuming read so an
            // undersized buffer never waits and never consumes the
            // in-flight state.
            let required = arc.staging_size() as usize;
            unsafe { std::ptr::write(out_len, required) };
            if out_cap < required {
                write_err(
                    &format!("wait_and_copy: out_buf too small (need {required}, have {out_cap})"),
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 2;
            }
            let ticket = ReadbackTicket {
                handle_id: ticket_handle_id,
                counter: ticket_counter,
            };
            match arc.wait_and_read(ticket, timeout_ns) {
                Ok(bytes) => {
                    // SAFETY: `out_cap >= required == bytes.len()`; out_buf
                    // null-checked above and writable for `out_cap` bytes.
                    unsafe {
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, bytes.len());
                        std::ptr::write(out_len, bytes.len());
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("{e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// ============================================================================
// Non-Linux stubs — `VulkanTextureReadback` is Linux-only.
// ============================================================================

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_submit(
    _readback_handle: *const c_void,
    _texture_handle: *const c_void,
    _source_layout_raw: i32,
    _out_ticket_handle_id: *mut u64,
    _out_ticket_counter: *mut u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_submit",
        || -> i32 {
            write_err(
                "submit: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_try_read(
    _readback_handle: *const c_void,
    _ticket_handle_id: u64,
    _ticket_counter: u64,
    _out_ready: *mut u32,
    _out_bytes_ptr: *mut *const u8,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_try_read",
        || -> i32 {
            write_err(
                "try_read: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_wait_and_read(
    _readback_handle: *const c_void,
    _ticket_handle_id: u64,
    _ticket_counter: u64,
    _timeout_ns: u64,
    _out_bytes_ptr: *mut *const u8,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_wait_and_read",
        || -> i32 {
            write_err(
                "wait_and_read: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_try_read_copy(
    _readback_handle: *const c_void,
    _ticket_handle_id: u64,
    _ticket_counter: u64,
    _out_ready: *mut u32,
    _out_buf: *mut u8,
    _out_cap: usize,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_try_read_copy",
        || -> i32 {
            write_err(
                "try_read_copy: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_texture_readback_wait_and_copy(
    _readback_handle: *const c_void,
    _ticket_handle_id: u64,
    _ticket_counter: u64,
    _timeout_ns: u64,
    _out_buf: *mut u8,
    _out_cap: usize,
    _out_len: *mut usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_texture_readback_wait_and_copy",
        || -> i32 {
            write_err(
                "wait_and_copy: not available on this platform",
                err_buf,
                err_buf_cap,
                err_len,
            );
            1
        },
        1,
    )
}

/// Host-side `VulkanTextureReadbackMethodsVTable`, wired to the real
/// bodies (Linux) or non-Linux stubs.
pub static HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE: VulkanTextureReadbackMethodsVTable =
    VulkanTextureReadbackMethodsVTable {
        layout_version: VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        submit: host_texture_readback_submit,
        try_read: host_texture_readback_try_read,
        wait_and_read: host_texture_readback_wait_and_read,
        try_read_copy: host_texture_readback_try_read_copy,
        wait_and_copy: host_texture_readback_wait_and_copy,
    };

/// Accessor for the host's static `VulkanTextureReadbackMethodsVTable`.
pub fn host_vulkan_texture_readback_methods_vtable() -> *const VulkanTextureReadbackMethodsVTable {
    match host_callbacks() {
        Some(c) if !c.vulkan_texture_readback_methods_vtable.is_null() => {
            c.vulkan_texture_readback_methods_vtable
        }
        _ => &HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_err_buf() -> ([u8; 256], usize) {
        ([0u8; 256], 0usize)
    }

    fn err_buf_as_str(buf: &[u8], len: usize) -> &str {
        std::str::from_utf8(&buf[..len]).expect("UTF-8")
    }

    #[test]
    fn layout_version_matches_constant() {
        assert_eq!(
            HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.layout_version,
            VULKAN_TEXTURE_READBACK_METHODS_VTABLE_LAYOUT_VERSION
        );
    }

    // -------- Tier-1 wire-format matrix (no GPU) --------
    //
    // These exercise the argument-validation prologue that runs BEFORE
    // any `readback_handle` dereference, so they don't need a real
    // `VulkanTextureReadback`. The success + error-taxonomy paths
    // (ForeignTicket / StaleTicket / InFlight / WaitTimeout / positive
    // round-trip) require a real handle and live in the hardware-gated
    // module below (and in the primitive's own tests).

    #[test]
    fn submit_null_readback_handle_errors() {
        let (mut buf, mut len) = make_err_buf();
        let mut hid = 0u64;
        let mut counter = 0u64;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.submit)(
                std::ptr::null(),
                std::ptr::null(),
                1, // GENERAL
                &mut hid,
                &mut counter,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("submit: null readback handle"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn submit_null_texture_handle_errors() {
        let (mut buf, mut len) = make_err_buf();
        let mut hid = 0u64;
        let mut counter = 0u64;
        // Non-null (bogus) readback handle so the null-texture check is
        // reached; the texture-null branch returns before any deref.
        let dummy = 0xDEAD_BEEFu64;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.submit)(
                &dummy as *const u64 as *const c_void,
                std::ptr::null(),
                1,
                &mut hid,
                &mut counter,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("submit: null texture handle"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn submit_null_out_param_errors() {
        let (mut buf, mut len) = make_err_buf();
        let dummy_rb = 0xDEAD_BEEFu64;
        let dummy_tex = 0xFEED_FACEu64;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.submit)(
                &dummy_rb as *const u64 as *const c_void,
                &dummy_tex as *const u64 as *const c_void,
                1,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("submit: null out pointer"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn submit_unsupported_source_layout_errors_before_deref() {
        // Non-null bogus handles + a bad raw layout: the layout decode
        // fails BEFORE any handle deref, so no real handle is needed.
        // Mental-revert: moving the layout decode after the arc deref
        // would SIGSEGV here on the bogus pointer.
        let (mut buf, mut len) = make_err_buf();
        let mut hid = 0u64;
        let mut counter = 0u64;
        let dummy_rb = 0xDEAD_BEEFu64;
        let dummy_tex = 0xFEED_FACEu64;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.submit)(
                &dummy_rb as *const u64 as *const c_void,
                &dummy_tex as *const u64 as *const c_void,
                9999, // not a supported VkImageLayout
                &mut hid,
                &mut counter,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("submit: unsupported source_layout_raw 9999"));
    }

    #[test]
    fn try_read_null_readback_handle_errors() {
        let (mut buf, mut len) = make_err_buf();
        let mut ready = 0u32;
        let mut bytes: *const u8 = std::ptr::null();
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.try_read)(
                std::ptr::null(),
                0,
                0,
                &mut ready,
                &mut bytes,
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("try_read: null readback handle"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn try_read_null_out_param_errors() {
        let (mut buf, mut len) = make_err_buf();
        let dummy_rb = 0xDEAD_BEEFu64;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.try_read)(
                &dummy_rb as *const u64 as *const c_void,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("try_read: null out pointer"));
    }

    #[test]
    fn wait_and_read_null_readback_handle_errors() {
        let (mut buf, mut len) = make_err_buf();
        let mut bytes: *const u8 = std::ptr::null();
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.wait_and_read)(
                std::ptr::null(),
                0,
                0,
                u64::MAX,
                &mut bytes,
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("wait_and_read: null readback handle"));
    }

    #[test]
    fn try_read_copy_null_readback_handle_errors() {
        let (mut buf, mut len) = make_err_buf();
        let mut ready = 0u32;
        let mut out = [0u8; 8];
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.try_read_copy)(
                std::ptr::null(),
                0,
                0,
                &mut ready,
                out.as_mut_ptr(),
                out.len(),
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("try_read_copy: null readback handle"));
    }

    #[test]
    fn wait_and_copy_null_readback_handle_errors() {
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 8];
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.wait_and_copy)(
                std::ptr::null(),
                0,
                0,
                u64::MAX,
                out.as_mut_ptr(),
                out.len(),
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        #[cfg(target_os = "linux")]
        assert!(err_buf_as_str(&buf, len).contains("wait_and_copy: null readback handle"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn wait_and_read_null_out_param_errors() {
        // Bogus non-null readback handle so the null-out check is reached;
        // the out-pointer-null branch returns before any handle deref.
        let (mut buf, mut len) = make_err_buf();
        let dummy_rb = 0xDEAD_BEEFu64;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.wait_and_read)(
                &dummy_rb as *const u64 as *const c_void,
                0,
                0,
                u64::MAX,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("wait_and_read: null out pointer"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn try_read_copy_null_out_param_errors() {
        // Bogus non-null readback handle; null out_ready/out_len return
        // before the arc deref.
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 8];
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.try_read_copy)(
                &(0xDEAD_BEEFu64) as *const u64 as *const c_void,
                0,
                0,
                std::ptr::null_mut(),
                out.as_mut_ptr(),
                out.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("try_read_copy: null out pointer"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn try_read_copy_null_out_buf_errors_before_consuming_read() {
        // The real ABI bug: out_buf=null with non-null out_ready/out_len.
        // The null-out_buf check now returns BEFORE the arc deref, so a
        // bogus non-null handle suffices (no GPU). Mental-revert: without
        // the out_buf null-check, out_buf=null with a large enough out_cap
        // would pass the size gate, consume the read, and report
        // out_ready=1 with zero bytes delivered.
        let (mut buf, mut len) = make_err_buf();
        let mut ready = 0u32;
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.try_read_copy)(
                &(0xDEAD_BEEFu64) as *const u64 as *const c_void,
                0,
                0,
                &mut ready,
                std::ptr::null_mut(),
                usize::MAX,
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("try_read_copy: null out_buf"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn wait_and_copy_null_out_param_errors() {
        // Bogus non-null readback handle; null out_len returns before the
        // arc deref.
        let (mut buf, mut len) = make_err_buf();
        let mut out = [0u8; 8];
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.wait_and_copy)(
                &(0xDEAD_BEEFu64) as *const u64 as *const c_void,
                0,
                0,
                u64::MAX,
                out.as_mut_ptr(),
                out.len(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("wait_and_copy: null out pointer"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn wait_and_copy_null_out_buf_errors_before_consuming_read() {
        // The real ABI bug: out_buf=null with a non-null out_len. The
        // null-out_buf check returns BEFORE the arc deref, so a bogus
        // non-null handle suffices (no GPU). Mental-revert: without the
        // out_buf null-check, out_buf=null with a large enough out_cap
        // would pass the size gate, block-consume the read, and report
        // success with zero bytes delivered.
        let (mut buf, mut len) = make_err_buf();
        let mut out_len = 0usize;
        let rc = unsafe {
            (HOST_VULKAN_TEXTURE_READBACK_METHODS_VTABLE.wait_and_copy)(
                &(0xDEAD_BEEFu64) as *const u64 as *const c_void,
                0,
                0,
                u64::MAX,
                std::ptr::null_mut(),
                usize::MAX,
                &mut out_len,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(err_buf_as_str(&buf, len).contains("wait_and_copy: null out_buf"));
    }

    // -------- Hardware-gated end-to-end round-trip through the ABI surface --------
    //
    // Exercises the full mint → submit → wait_and_read/copy chain in
    // host (Boxed) mode, driving the `TextureReadback` PluginAbiObject twin
    // through the methods vtable — the exit-criterion round-trip. Also
    // covers ForeignTicket / StaleTicket / double-submit-InFlight /
    // WaitTimeout / err_buf truncation against a real handle.
    #[cfg(target_os = "linux")]
    #[cfg(test)]
    mod hardware {
        use crate::core::Result;
        use crate::core::context::GpuContext;
        use crate::core::rhi::{
            ReadbackTicket, Texture, TextureDescriptor, TextureFormat, TextureSourceLayout,
            TextureUsages,
        };

        // FullAccess is reached the canonical way — `gpu.escalate(|full| ...)`
        // (Boxed / in-process mode) — because `GpuContextFullAccess::new`
        // is crate-module-private to `core::context`.
        fn fresh() -> Option<GpuContext> {
            GpuContext::init_for_platform().ok()
        }

        /// Allocate + fill a BGRA8 texture with a known pattern via the RHI
        /// upload primitive. The RHI leaves the image in
        /// `SHADER_READ_ONLY_OPTIMAL`, so callers submit it to the readback
        /// as [`TextureSourceLayout::ShaderReadOnly`].
        fn make_filled_texture(
            gpu: &GpuContext,
            width: u32,
            height: u32,
            pattern: impl Fn(u32, u32) -> [u8; 4],
        ) -> Texture {
            use crate::host_rhi::HostTextureExt;
            let device = &gpu.device().inner;
            let bpp = 4u64;
            let staging = crate::vulkan::rhi::HostVulkanBuffer::new(
                device,
                (width as u64) * (height as u64) * bpp,
            )
            .expect("staging");
            unsafe {
                let mut p = staging.mapped_ptr();
                for y in 0..height {
                    for x in 0..width {
                        let px = pattern(x, y);
                        std::ptr::copy_nonoverlapping(px.as_ptr(), p, 4);
                        p = p.add(4);
                    }
                }
            }
            let desc = TextureDescriptor {
                width,
                height,
                format: TextureFormat::Bgra8Unorm,
                usage: TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST
                    | TextureUsages::STORAGE_BINDING,
                label: Some("readback-abi-test-texture"),
            };
            let host_tex =
                crate::vulkan::rhi::HostVulkanTexture::new(device, &desc).expect("texture");
            let texture = <Texture as crate::host_rhi::HostTextureExt>::from_vulkan(host_tex);
            let img = texture.vulkan_inner().image().expect("vk image");
            // RHI upload primitive: UNDEFINED → TRANSFER_DST → copy →
            // SHADER_READ_ONLY_OPTIMAL, with a transient pool / command
            // buffer / fence and the guarded queue submit + fence wait. The
            // image ends shader-read-only, so the round-trip submits it as
            // `TextureSourceLayout::ShaderReadOnly`.
            unsafe {
                device
                    .upload_buffer_to_image(staging.buffer(), img, width, height)
                    .expect("upload_buffer_to_image");
            }
            texture
        }

        #[cfg_attr(
            not(feature = "hardware-tests"),
            ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
        )]
        #[test]
        fn abi_surface_round_trip_returns_matching_bytes() {
            let Some(gpu) = fresh() else {
                return;
            };
            let width = 32u32;
            let height = 32u32;
            let pattern = |x: u32, y: u32| {
                [
                    ((x.wrapping_mul(7)) & 0xFF) as u8,
                    ((y.wrapping_mul(11)) & 0xFF) as u8,
                    (((x ^ y).wrapping_mul(13)) & 0xFF) as u8,
                    0xFF,
                ]
            };
            let texture = make_filled_texture(&gpu, width, height, pattern);

            crate::core::context::GpuContextLimitedAccess::new(gpu.clone()).escalate(|full| -> Result<()> {
                let readback = full
                    .create_texture_readback("rt-abi", width, height, TextureFormat::Bgra8Unorm)
                    .expect("create_texture_readback");
                assert_eq!(readback.width(), width);
                assert_eq!(readback.height(), height);
                assert_eq!(readback.format(), TextureFormat::Bgra8Unorm);
                assert_eq!(readback.staging_size(), (width * height * 4) as u64);
                assert!(readback.handle_id() > 0);

                // Borrow path. A successful submit is also the submit-side
                // make-borrow cached-field regression: if `make_texture_borrow`
                // returned zeroed width/height/format, submit would trip
                // DescriptorMismatch and this would fail.
                let ticket = readback
                    .submit(&texture, TextureSourceLayout::ShaderReadOnly)
                    .expect("submit");
                let bytes = readback.wait_and_read(ticket, u64::MAX).expect("wait");
                for y in 0..height {
                    for x in 0..width {
                        let off = ((y * width + x) * 4) as usize;
                        assert_eq!(&bytes[off..off + 4], &pattern(x, y), "mismatch at ({x},{y})");
                    }
                }

                // Copy path (must outlive the borrow window).
                let ticket2 = readback
                    .submit(&texture, TextureSourceLayout::ShaderReadOnly)
                    .expect("submit 2");
                let owned = readback
                    .wait_and_copy(ticket2, u64::MAX)
                    .expect("wait_and_copy");
                assert_eq!(owned.len(), (width * height * 4) as usize);
                for y in 0..height {
                    for x in 0..width {
                        let off = ((y * width + x) * 4) as usize;
                        assert_eq!(&owned[off..off + 4], &pattern(x, y));
                    }
                }
                Ok(())
            })
            .expect("escalate");
        }

        #[cfg_attr(
            not(feature = "hardware-tests"),
            ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
        )]
        #[test]
        fn double_submit_reports_in_flight_through_surface() {
            let Some(gpu) = fresh() else {
                return;
            };
            let texture = make_filled_texture(&gpu, 16, 16, |_, _| [1, 2, 3, 4]);
            crate::core::context::GpuContextLimitedAccess::new(gpu.clone()).escalate(|full| -> Result<()> {
                let readback = full
                    .create_texture_readback("rt-inflight", 16, 16, TextureFormat::Bgra8Unorm)
                    .expect("create");
                let ticket = readback
                    .submit(&texture, TextureSourceLayout::ShaderReadOnly)
                    .expect("first");
                let err = readback
                    .submit(&texture, TextureSourceLayout::ShaderReadOnly)
                    .err()
                    .expect("expected in-flight");
                assert!(err.to_string().contains("in flight"), "got: {err}");
                let _ = readback.wait_and_read(ticket, u64::MAX).expect("drain");
                Ok(())
            })
            .expect("escalate");
        }

        #[cfg_attr(
            not(feature = "hardware-tests"),
            ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
        )]
        #[test]
        fn foreign_ticket_rejected_through_surface() {
            let Some(gpu) = fresh() else {
                return;
            };
            let texture = make_filled_texture(&gpu, 16, 16, |_, _| [0, 0, 0, 0xFF]);
            crate::core::context::GpuContextLimitedAccess::new(gpu.clone()).escalate(|full| -> Result<()> {
                let rb1 = full
                    .create_texture_readback("rt-foreign-1", 16, 16, TextureFormat::Bgra8Unorm)
                    .expect("rb1");
                let rb2 = full
                    .create_texture_readback("rt-foreign-2", 16, 16, TextureFormat::Bgra8Unorm)
                    .expect("rb2");
                let ticket = rb1
                    .submit(&texture, TextureSourceLayout::ShaderReadOnly)
                    .expect("submit");
                let err = rb2.try_read(ticket).err().expect("expected foreign");
                assert!(err.to_string().contains("foreign handle"), "got: {err}");
                let _ = rb1.wait_and_read(ticket, u64::MAX).expect("drain");
                Ok(())
            })
            .expect("escalate");
        }

        #[cfg_attr(
            not(feature = "hardware-tests"),
            ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
        )]
        #[test]
        fn stale_ticket_rejected_through_surface() {
            let Some(gpu) = fresh() else {
                return;
            };
            let texture = make_filled_texture(&gpu, 16, 16, |_, _| [9, 8, 7, 6]);
            crate::core::context::GpuContextLimitedAccess::new(gpu.clone()).escalate(|full| -> Result<()> {
                let readback = full
                    .create_texture_readback("rt-stale", 16, 16, TextureFormat::Bgra8Unorm)
                    .expect("rb");
                let ticket = readback
                    .submit(&texture, TextureSourceLayout::ShaderReadOnly)
                    .expect("submit");
                // A ticket with the right handle id but a wrong counter is
                // stale (the handle is single-in-flight). Fields are
                // crate-internal — constructible here.
                let stale = ReadbackTicket {
                    handle_id: ticket.handle_id,
                    counter: 999_999,
                };
                let err = readback.try_read(stale).err().expect("expected stale");
                assert!(
                    err.to_string().contains("does not match in-flight"),
                    "got: {err}"
                );
                let _ = readback.wait_and_read(ticket, u64::MAX).expect("drain");
                Ok(())
            })
            .expect("escalate");
        }
    }
}
