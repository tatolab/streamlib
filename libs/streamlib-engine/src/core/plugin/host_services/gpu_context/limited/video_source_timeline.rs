// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `GpuContextLimitedAccessVTable` video-source timeline-semaphore
//! publish / clear / wait + host-Arc accessor (v12 #958, v14 #1066).
//!
//! - `set` / `clear` install or remove the host's
//!   `Arc<HostVulkanTimelineSemaphore>` from the GpuContext's publish
//!   slot — producers (e.g. camera) call this to share their per-frame
//!   timeline with consumers.
//! - `wait` blocks on the timeline directly (no GpuContext deref
//!   needed; the timeline carries its own `vulkanalia::Device`).
//! - `host_video_source_timeline_arc` clones the publish slot's Arc
//!   for the cdylib so it can construct a borrowed wrapper without
//!   reaching back through the vtable per-call.

use std::ffi::c_void;
use std::sync::Arc;

use super::super::shared::handle_as_gpu_context;
use super::super::super::run_host_extern_c;
use super::super::super::shared::wire::write_err;

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_set_video_source_timeline_semaphore(
    handle: *const c_void,
    timeline_handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_set_video_source_timeline_semaphore",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            if timeline_handle.is_null() {
                return;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `timeline_handle` is a borrowed
                // `Arc::as_ptr(&Arc<HostVulkanTimelineSemaphore>)`
                // produced by the cdylib caller. Bump the refcount so
                // we can take a temporary owned Arc via `Arc::from_raw`;
                // the caller's Arc strong-count is unchanged.
                // Mirrors the `host_gpu_lim_register_texture` pattern
                // for borrowed `Arc<TextureInner>`-shaped handles.
                let ptr = timeline_handle
                    as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore;
                unsafe {
                    Arc::increment_strong_count(ptr);
                }
                let arc = unsafe { Arc::from_raw(ptr) };
                gpu.set_video_source_timeline_semaphore(&arc);
                // `arc` drops here, balancing the `increment_strong_count`
                // above. The slot holds its own `Arc::clone` (taken by
                // `set_video_source_timeline_semaphore` from the
                // borrow).
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = timeline_handle;
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_clear_video_source_timeline_semaphore(
    handle: *const c_void,
) {
    run_host_extern_c(
        "host_gpu_lim_clear_video_source_timeline_semaphore",
        || {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return;
            };
            #[cfg(target_os = "linux")]
            {
                gpu.clear_video_source_timeline_semaphore();
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = gpu;
            }
        },
        (),
    )
}

pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_wait_timeline_semaphore(
    _handle: *const c_void,
    timeline_handle: *const c_void,
    value: u64,
    timeout_ns: u64,
    err_buf: *mut u8,
    err_buf_cap: usize,
    err_len: *mut usize,
) -> i32 {
    run_host_extern_c(
        "host_gpu_lim_wait_timeline_semaphore",
        || {
            // `gpu_handle` is intentionally ignored — the timeline
            // borrow carries its own `vulkanalia::Device`, so the
            // wait runs against the timeline directly without
            // dereferencing any `GpuContext` instance. The handle
            // stays in the wire format for cross-slot consistency.
            if timeline_handle.is_null() {
                write_err(
                    "wait_timeline_semaphore: null timeline_handle",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                return 1;
            }
            #[cfg(target_os = "linux")]
            {
                // SAFETY: `timeline_handle` is a borrowed pointer
                // from the cdylib's
                // `HostVulkanTimelineSemaphore::wait_via_vtable`
                // (which gets it via `self as *const Self`). The
                // host borrow lasts only for the duration of the
                // wait call. We call `wait_direct` to bypass the
                // `host_callbacks().is_some()` check on `wait()`
                // itself — otherwise the host would re-dispatch
                // through the vtable into infinite recursion.
                let timeline = unsafe {
                    &*(timeline_handle
                        as *const crate::vulkan::rhi::HostVulkanTimelineSemaphore)
                };
                match timeline.wait_direct(value, timeout_ns) {
                    Ok(()) => 0,
                    Err(e) => {
                        write_err(
                            &format!("wait_timeline_semaphore: {e}"),
                            err_buf,
                            err_buf_cap,
                            err_len,
                        );
                        1
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (timeline_handle, value, timeout_ns);
                write_err(
                    "wait_timeline_semaphore: Linux-only",
                    err_buf,
                    err_buf_cap,
                    err_len,
                );
                1
            }
        },
        1,
    )
}

/// Clone the host's `Arc<HostVulkanTimelineSemaphore>` from the
/// publish slot and return the raw `Arc::into_raw` pointer to the
/// cdylib. The cdylib reconstitutes via `Arc::from_raw`; the host's
/// slot retains its own independent strong count. Returns null when
/// `gpu_handle` is null or when no producer has published a
/// timeline (the slot is `None`).
pub(in crate::core::plugin::host_services) unsafe extern "C" fn host_gpu_lim_host_video_source_timeline_arc(
    handle: *const c_void,
) -> *const c_void {
    run_host_extern_c(
        "host_gpu_lim_host_video_source_timeline_arc",
        || -> *const c_void {
            let Some(gpu) = (unsafe { handle_as_gpu_context(handle) }) else {
                return std::ptr::null();
            };
            #[cfg(target_os = "linux")]
            {
                match gpu.video_source_timeline_semaphore() {
                    Some(arc) => Arc::into_raw(arc) as *const c_void,
                    None => std::ptr::null(),
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = gpu;
                std::ptr::null()
            }
        },
        std::ptr::null(),
    )
}

#[cfg(test)]
mod tier1_wire_format_tests {
    //! Tier-1 wire-format tests for the v12 (#958)
    //! `set_video_source_timeline_semaphore` /
    //! `clear_video_source_timeline_semaphore` slots. Each wrapper
    //! must short-circuit on null gpu_handle (and `set` on null
    //! timeline_handle) without panicking and without dereferencing
    //! the null pointers.
    //!
    //! The non-null-handle path is exercised end-to-end by the
    //! `load_project_dylib_camera_smoke` integration test (which
    //! holds a real `Arc<HostVulkanTimelineSemaphore>` and is the
    //! only place a Tier-1 with-handle test could reach without
    //! constructing a real `GpuContext` here).
    //!
    //! Mental-revert: stub the wrapper bodies to
    //! `unimplemented!()` and these tests trip the underlying
    //! deref / panic — the wire-format claim regresses.

    use super::super::super::HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE;

    #[test]
    fn set_video_source_timeline_is_noop_on_null_gpu_handle() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .set_video_source_timeline_semaphore)(
                std::ptr::null(),
                std::ptr::null(),
            );
        }
    }

    // Note: the timeline_handle null guard at
    // host_gpu_lim_set_video_source_timeline_semaphore isn't reachable
    // at tier-1: the first guard (handle_as_gpu_context) short-circuits
    // on null gpu_handle, and a non-null garbage gpu_handle would
    // UB-deref before reaching the timeline check. The guard is
    // exercised end-to-end by load_project_dylib_camera_smoke (the
    // cdylib camera passes a valid gpu_handle and a real Arc-borrow
    // timeline_handle).

    #[test]
    fn clear_video_source_timeline_is_noop_on_null_gpu_handle() {
        unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .clear_video_source_timeline_semaphore)(std::ptr::null());
        }
    }

    /// v14 slot (#1066): tier-1 wire-format guard. Null `gpu_handle`
    /// must return null rather than dereferencing the pointer. The
    /// non-null-handle "slot empty" → null and "slot populated" →
    /// non-null Arc pointer paths are exercised end-to-end by the
    /// camera-display cdylib reproducer; a tier-1 unit test for them
    /// would need a real `GpuContext` instance, which this module
    /// deliberately avoids constructing.
    ///
    /// Mental-revert: stub the wrapper to `unimplemented!()` and
    /// this test trips the underlying panic — the null-guard
    /// contract regresses.
    #[test]
    fn host_video_source_timeline_arc_returns_null_on_null_gpu_handle() {
        let raw = unsafe {
            (HOST_GPU_CONTEXT_LIMITED_ACCESS_VTABLE
                .host_video_source_timeline_arc)(std::ptr::null())
        };
        assert!(raw.is_null(), "expected null on null gpu_handle");
    }
}
