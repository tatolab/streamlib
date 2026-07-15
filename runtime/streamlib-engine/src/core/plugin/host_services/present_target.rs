// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side `PresentTargetMethodsVTable` static + method bodies (#1258).
//!
//! Each slot reconstructs the `Box<Mutex<VulkanPresentTarget>>` behind
//! the opaque `present_handle`, `try_lock`s it (matching
//! `RhiCommandRecorderInner`'s state-guard discipline so a concurrent or
//! misordered `begin_frame`/`end_frame` returns a typed error, never UB),
//! and drives the engine present-loop split
//! (`VulkanPresentTarget::begin_frame` / `end_frame` / `recreate` /
//! `set_hdr_metadata`). Every body is wrapped in the `run_host_extern_c`
//! panic net — no unwind crosses the plugin ABI. All swapchain-image
//! acquire + per-image render-finished-semaphore keying
//! (VUID-vkQueueSubmit2-semaphore-03868) stays host-side and opaque to
//! the caller across the begin/end split.

use std::ffi::c_void;

use streamlib_plugin_abi::{
    ColorTraitsRepr, HdrStaticMetadataRepr, PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION,
    PresentFrameBeginRepr, PresentTargetMethodsVTable, SemaphoreSubmitInfoRepr,
};

use super::host_callbacks;
use super::run_host_extern_c;
use super::shared::wire::write_err;

// ============================================================================
// Shared wire → engine decoders (Linux-only — reach the RHI color types).
// ============================================================================

/// Decode a nullable `*const ColorTraitsRepr` into an engine
/// [`crate::core::color::ColorTraits`]. A null pointer (or a whole-struct
/// `u32::MAX`/`u32::MAX`) is the legacy SDR pick (`None`). Shared by the
/// FullAccess `create_present_target` body and the `recreate` slot.
#[cfg(target_os = "linux")]
pub(in crate::core::plugin::host_services) fn color_traits_from_repr(
    color: *const ColorTraitsRepr,
) -> Option<crate::core::color::ColorTraits> {
    use crate::core::color::ColorTraits;
    if color.is_null() {
        return None;
    }
    // SAFETY: caller-provided, non-null; read once by value (pure POD).
    let repr = unsafe { *color };
    let primaries = if repr.primaries_raw == u32::MAX {
        None
    } else {
        primaries_id_from_raw(repr.primaries_raw)
    };
    let transfer = if repr.transfer_raw == u32::MAX {
        None
    } else {
        transfer_id_from_raw(repr.transfer_raw)
    };
    Some(ColorTraits {
        primaries,
        transfer,
    })
}

#[cfg(target_os = "linux")]
fn primaries_id_from_raw(raw: u32) -> Option<crate::core::color::PrimariesId> {
    use crate::core::color::PrimariesId::*;
    Some(match raw {
        0 => Bt709,
        1 => Bt470M,
        2 => Bt470Bg,
        3 => Smpte170m,
        4 => Smpte240m,
        5 => Film,
        6 => Bt2020,
        7 => Smpte428,
        8 => Smpte431,
        9 => Smpte432,
        10 => Ebu3213,
        _ => return None,
    })
}

#[cfg(target_os = "linux")]
fn transfer_id_from_raw(raw: u32) -> Option<crate::core::color::TransferId> {
    use crate::core::color::TransferId::*;
    Some(match raw {
        0 => Linear,
        1 => Srgb,
        2 => Bt709,
        3 => Pq,
        4 => Hlg,
        _ => return None,
    })
}

#[cfg(target_os = "linux")]
fn hdr_static_metadata_from_repr(
    repr: &HdrStaticMetadataRepr,
) -> crate::core::color::HdrStaticMetadata {
    crate::core::color::HdrStaticMetadata {
        display_primary_red: repr.display_primary_red,
        display_primary_green: repr.display_primary_green,
        display_primary_blue: repr.display_primary_blue,
        white_point: repr.white_point,
        min_luminance_cd_m2: repr.min_luminance_cd_m2,
        max_luminance_cd_m2: repr.max_luminance_cd_m2,
        max_content_light_level: repr.max_content_light_level,
        max_frame_average_light_level: repr.max_frame_average_light_level,
    }
}

/// Reconstruct the `Mutex<VulkanPresentTarget>` behind an opaque
/// `present_handle` (`Box::into_raw(Box<Mutex<VulkanPresentTarget>>)` from
/// `PresentTarget::from_target`). Borrowed — never reclaims the Box.
#[cfg(target_os = "linux")]
unsafe fn handle_as_present_target<'a>(
    handle: *const c_void,
) -> Option<&'a crate::vulkan::rhi::PresentTargetInner> {
    if handle.is_null() {
        return None;
    }
    // SAFETY: non-null handle is the leaked `Box<PresentTargetInner>`
    // pointer minted by `PresentTarget::from_target`; valid for the call.
    Some(unsafe { &*(handle as *const crate::vulkan::rhi::PresentTargetInner) })
}

// ============================================================================
// begin_frame
// ============================================================================

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_present_target_begin_frame(
    present_handle: *const c_void,
    out_frame: *mut PresentFrameBeginRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_begin_frame",
        || -> i32 {
            if out_frame.is_null() {
                write_err("begin_frame: null out_frame", err_buf, err_buf_cap, err_len);
                return 1;
            }
            let Some(mtx) = (unsafe { handle_as_present_target(present_handle) }) else {
                write_err(
                    "begin_frame: null present_handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let mut target = match mtx.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    write_err(
                        "begin_frame: present target busy or poisoned \
                         (concurrent or panicked begin_frame/end_frame)",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            match target.begin_frame() {
                Ok(Some(acquired)) => {
                    let recorder_handle = target.in_flight_recorder_handle();
                    let frame = PresentFrameBeginRepr {
                        recorder_handle: recorder_handle as u64,
                        image_raw: acquired.image_raw,
                        image_view_raw: acquired.image_view_raw,
                        frame_index: acquired.frame_index,
                        extent_w: acquired.extent.0,
                        extent_h: acquired.extent.1,
                        acquired_ok: 1,
                        color_format_raw: acquired.color_format as u32,
                        _reserved_padding: 0,
                    };
                    // SAFETY: out_frame non-null per the guard above.
                    unsafe { std::ptr::write(out_frame, frame) };
                    0
                }
                Ok(None) => {
                    // OUT_OF_DATE_KHR: acquired_ok = 0, recorder_handle = 0.
                    // The caller drives `recreate` and does NOT call
                    // `end_frame` (no frame was stashed in flight).
                    let frame = PresentFrameBeginRepr {
                        acquired_ok: 0,
                        ..Default::default()
                    };
                    // SAFETY: out_frame non-null per the guard above.
                    unsafe { std::ptr::write(out_frame, frame) };
                    0
                }
                Err(e) => {
                    write_err(&format!("begin_frame: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// ============================================================================
// end_frame
// ============================================================================

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_present_target_end_frame(
    present_handle: *const c_void,
    recorder_handle: *const c_void,
    extra_waits_ptr: *const SemaphoreSubmitInfoRepr,
    extra_waits_count: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_end_frame",
        || -> i32 {
            let Some(mtx) = (unsafe { handle_as_present_target(present_handle) }) else {
                write_err(
                    "end_frame: null present_handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let mut target = match mtx.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    write_err(
                        "end_frame: present target busy or poisoned \
                         (concurrent or panicked begin_frame/end_frame)",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // Recorder-identity check: the handle the caller drove must be
            // the frame's borrowed internal recorder — a mismatch would
            // mean the caller recorded into a different recorder than the
            // one this frame submits, silently dropping the draws.
            let expected = target.in_flight_recorder_handle();
            if expected.is_null() {
                write_err(
                    "end_frame: no frame in flight (misordered begin_frame/end_frame)",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            if recorder_handle != expected {
                write_err(
                    "end_frame: recorder handle mismatch — not the frame's \
                     borrowed recorder from begin_frame",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let extra_waits: &[SemaphoreSubmitInfoRepr] = if extra_waits_count == 0 {
                &[]
            } else if extra_waits_ptr.is_null() {
                write_err(
                    "end_frame: null extra_waits_ptr with non-zero count",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            } else {
                // SAFETY: caller-owned array valid for the call.
                unsafe { std::slice::from_raw_parts(extra_waits_ptr, extra_waits_count) }
            };
            match target.end_frame_from_wire(extra_waits) {
                Ok(_present_ok) => 0,
                Err(e) => {
                    write_err(&format!("end_frame: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// ============================================================================
// recreate
// ============================================================================

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_present_target_recreate(
    present_handle: *const c_void,
    width: u32,
    height: u32,
    color: *const ColorTraitsRepr,
    out_color_format_raw: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_recreate",
        || -> i32 {
            let Some(mtx) = (unsafe { handle_as_present_target(present_handle) }) else {
                write_err("recreate: null present_handle", err_buf, err_buf_cap, err_len);
                return 1;
            };
            let mut target = match mtx.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    write_err(
                        "recreate: present target busy or poisoned",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            let color_traits = color_traits_from_repr(color);
            match target.recreate(width, height, color_traits.as_ref()) {
                Ok(()) => {
                    // Write the live post-recreate format so the cdylib's
                    // cached `color_format_raw` refreshes immediately — a
                    // recreate can flip SDR BGRA8 → HDR10 FP16, and without
                    // this out-param the cached getter is stale until the
                    // next begin_frame (the make-borrow staleness class,
                    // `docs/learnings/cdylib-make-borrow-cached-fields.md`).
                    if !out_color_format_raw.is_null() {
                        // SAFETY: caller-provided out-pointer.
                        unsafe { *out_color_format_raw = target.color_format() as u32 };
                    }
                    0
                }
                Err(e) => {
                    write_err(&format!("recreate: {e}"), err_buf, err_buf_cap, err_len);
                    1
                }
            }
        },
        1,
    )
}

// ============================================================================
// set_hdr_metadata
// ============================================================================

#[cfg(target_os = "linux")]
unsafe extern "C" fn host_present_target_set_hdr_metadata(
    present_handle: *const c_void,
    metadata: *const HdrStaticMetadataRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_set_hdr_metadata",
        || -> i32 {
            if metadata.is_null() {
                write_err(
                    "set_hdr_metadata: null metadata",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            let Some(mtx) = (unsafe { handle_as_present_target(present_handle) }) else {
                write_err(
                    "set_hdr_metadata: null present_handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            };
            let mut target = match mtx.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    write_err(
                        "set_hdr_metadata: present target busy or poisoned",
                        err_buf,
                        err_buf_cap,
                        err_len,
                    );
                    return 1;
                }
            };
            // SAFETY: metadata non-null per the guard; read once by ref.
            let hdr = hdr_static_metadata_from_repr(unsafe { &*metadata });
            // No-op host-side when the swapchain colorspace is not
            // HDR-signaling (handled inside `set_hdr_metadata`).
            match target.set_hdr_metadata(&hdr) {
                Ok(()) => 0,
                Err(e) => {
                    write_err(
                        &format!("set_hdr_metadata: {e}"),
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

// ============================================================================
// Non-Linux stubs — the present target only ships on Linux; the slots
// must resolve on every platform for ABI layout stability, returning the
// typed not-yet-provided refusal (Apple's display path is CAMetalLayer,
// outside this surface).
// ============================================================================

#[cfg(not(target_os = "linux"))]
use super::shared::wire::{NOT_YET_PROVIDED_RC, not_yet_provided};

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_present_target_begin_frame(
    _present_handle: *const c_void,
    _out_frame: *mut PresentFrameBeginRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_begin_frame",
        || not_yet_provided("begin_frame", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_present_target_end_frame(
    _present_handle: *const c_void,
    _recorder_handle: *const c_void,
    _extra_waits_ptr: *const SemaphoreSubmitInfoRepr,
    _extra_waits_count: usize,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_end_frame",
        || not_yet_provided("end_frame", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn host_present_target_recreate(
    _present_handle: *const c_void,
    _width: u32,
    _height: u32,
    _color: *const ColorTraitsRepr,
    _out_color_format_raw: *mut u32,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_recreate",
        || not_yet_provided("recreate", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

#[cfg(not(target_os = "linux"))]
unsafe extern "C" fn host_present_target_set_hdr_metadata(
    _present_handle: *const c_void,
    _metadata: *const HdrStaticMetadataRepr,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_present_target_set_hdr_metadata",
        || not_yet_provided("set_hdr_metadata", err_buf, err_buf_cap, err_len),
        NOT_YET_PROVIDED_RC,
    )
}

/// Host-side `PresentTargetMethodsVTable`, wired to the real bodies
/// (Linux) / typed not-yet-provided stubs (non-Linux).
pub static HOST_PRESENT_TARGET_METHODS_VTABLE: PresentTargetMethodsVTable =
    PresentTargetMethodsVTable {
        layout_version: PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION,
        _reserved_padding: 0,
        begin_frame: host_present_target_begin_frame,
        end_frame: host_present_target_end_frame,
        recreate: host_present_target_recreate,
        set_hdr_metadata: host_present_target_set_hdr_metadata,
    };

/// Accessor for the host's static `PresentTargetMethodsVTable` — used by
/// the `PresentTarget` PluginAbiObject constructor (host mode resolves the
/// local static; cdylib mode resolves the host-installed pointer cached on
/// [`super::HostCallbacks`]).
pub fn host_present_target_methods_vtable() -> *const PresentTargetMethodsVTable {
    match host_callbacks() {
        Some(c) if !c.present_target_methods_vtable.is_null() => c.present_target_methods_vtable,
        _ => &HOST_PRESENT_TARGET_METHODS_VTABLE,
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
            HOST_PRESENT_TARGET_METHODS_VTABLE.layout_version,
            PRESENT_TARGET_METHODS_VTABLE_LAYOUT_VERSION
        );
    }

    // ------------------------------------------------------------------
    // Tier-1 wire-format tests: null-handle / null-out-param /
    // invalid-args paths that need no GPU. The positive acquire →
    // draw → present path is GPU + window bound and lives in the
    // camera→display E2E harness. Mentally reverting a null guard to a
    // deref makes the matching test segfault instead of returning the
    // typed refusal.
    // ------------------------------------------------------------------

    #[cfg(target_os = "linux")]
    #[test]
    fn begin_frame_null_handle_is_typed_error() {
        let (mut buf, mut len) = make_err_buf();
        let mut frame = PresentFrameBeginRepr::default();
        let rc = unsafe {
            (HOST_PRESENT_TARGET_METHODS_VTABLE.begin_frame)(
                std::ptr::null(),
                &mut frame,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("begin_frame: null present_handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn begin_frame_null_out_frame_is_typed_error() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_PRESENT_TARGET_METHODS_VTABLE.begin_frame)(
                std::ptr::null(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("begin_frame: null out_frame"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn end_frame_null_handle_is_typed_error() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_PRESENT_TARGET_METHODS_VTABLE.end_frame)(
                std::ptr::null(),
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
            err_buf_as_str(&buf, len).contains("end_frame: null present_handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recreate_null_handle_is_typed_error() {
        let (mut buf, mut len) = make_err_buf();
        let mut fmt: u32 = 0;
        let rc = unsafe {
            (HOST_PRESENT_TARGET_METHODS_VTABLE.recreate)(
                std::ptr::null(),
                64,
                64,
                std::ptr::null(),
                &mut fmt,
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("recreate: null present_handle"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn set_hdr_metadata_null_metadata_is_typed_error() {
        let (mut buf, mut len) = make_err_buf();
        let rc = unsafe {
            (HOST_PRESENT_TARGET_METHODS_VTABLE.set_hdr_metadata)(
                std::ptr::null(),
                std::ptr::null(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len,
            )
        };
        assert_eq!(rc, 1);
        assert!(
            err_buf_as_str(&buf, len).contains("set_hdr_metadata: null metadata"),
            "got: {}",
            err_buf_as_str(&buf, len)
        );
    }

    // ------------------------------------------------------------------
    // Wire decoder unit tests (GPU-free).
    // ------------------------------------------------------------------

    #[cfg(target_os = "linux")]
    #[test]
    fn color_traits_from_repr_null_is_legacy_sdr() {
        assert!(color_traits_from_repr(std::ptr::null()).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn color_traits_from_repr_decodes_hdr10_and_sentinels() {
        use crate::core::color::{PrimariesId, TransferId};
        // HDR10: Bt2020 primaries (6) + PQ transfer (3).
        let repr = ColorTraitsRepr {
            primaries_raw: 6,
            transfer_raw: 3,
        };
        let traits = color_traits_from_repr(&repr).expect("some");
        assert_eq!(traits.primaries, Some(PrimariesId::Bt2020));
        assert_eq!(traits.transfer, Some(TransferId::Pq));
        // u32::MAX sentinel on both axes = whole-struct None-equivalent.
        let none_repr = ColorTraitsRepr {
            primaries_raw: u32::MAX,
            transfer_raw: u32::MAX,
        };
        let traits = color_traits_from_repr(&none_repr).expect("some");
        assert_eq!(traits.primaries, None);
        assert_eq!(traits.transfer, None);
    }
}
