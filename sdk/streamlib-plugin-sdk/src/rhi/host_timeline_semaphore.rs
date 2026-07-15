// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cdylib-arm twin of the engine's exportable-timeline PluginAbiObject
//! (#1260).
//!
//! [`HostTimelineSemaphore`] is layout-stable
//! `#[repr(C)] { handle, methods }`; clone/drop AND method dispatch all
//! route through the self-contained per-type
//! [`streamlib_plugin_abi::HostTimelineSemaphoreMethodsVTable`] — no
//! parent-vtable pointer (unlike [`super::RhiColorConverter`]). The host
//! `HostVulkanTimelineSemaphore` backing stays in the engine; refcount
//! bookkeeping runs in host-compiled code via the vtable's `clone_handle`
//! / `drop_handle`.
//!
//! Minted by
//! [`crate::context::GpuContextFullAccess::create_exportable_timeline_semaphore`];
//! the producer feeds its [`Self::cdylib_handle`] into the
//! SurfaceStore `register_texture` producer path as a `produce_done` /
//! `consume_done` sidecar.

use std::ffi::c_void;

use streamlib_error::{Error, Result};
use streamlib_plugin_abi::HostTimelineSemaphoreMethodsVTable;

/// Cdylib-side view of an OPAQUE_FD-exportable timeline semaphore.
///
/// A monotonic 64-bit counter shared with a subprocess consumer via
/// [`Self::export_opaque_fd`]. Single-writer-per-edge (see
/// `docs/architecture/adapter-timeline-single-writer.md`): only one
/// process ever signals a given timeline, so [`Self::signal`] values are
/// strictly increasing by contract.
#[repr(C)]
pub struct HostTimelineSemaphore {
    /// Opaque handle to the host's `Arc<HostVulkanTimelineSemaphore>`.
    pub(crate) handle: *const c_void,
    /// Per-type vtable for plugin ABI clone/drop + method dispatch.
    pub(crate) methods: *const HostTimelineSemaphoreMethodsVTable,
}

// SAFETY: same shape as the engine twin. The handle is a host-owned
// `Arc<HostVulkanTimelineSemaphore>` (Send+Sync); the methods vtable
// pointer is `&'static` in the host image.
unsafe impl Send for HostTimelineSemaphore {}
unsafe impl Sync for HostTimelineSemaphore {}

impl HostTimelineSemaphore {
    /// Block until the timeline counter reaches or surpasses `value`.
    /// `timeout_ns == u64::MAX` waits with no timeout. Dispatches through
    /// the methods vtable's `wait` slot (host-side `vkWaitSemaphores`).
    pub fn wait(&self, value: u64, timeout_ns: u64) -> Result<()> {
        if self.methods.is_null() {
            return Err(Error::GpuError(
                "HostTimelineSemaphore::wait: methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods non-null per the guard; `handle` is the
        // borrowed host pointer the host derefs without taking ownership.
        let status = unsafe {
            ((*self.methods).wait)(
                self.handle,
                value,
                timeout_ns,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_unit(status, &err_buf, err_len)
    }

    /// CPU-side signal advancing the counter to `value`
    /// (single-writer-per-edge; `value` strictly increasing by contract).
    /// Dispatches through the methods vtable's `signal` slot (host-side
    /// `vkSignalSemaphore`).
    ///
    /// Cross-process caveat: a cross-process producer must advance its
    /// `produce_done` edge by GPU-queue completion (a queue-submit signal
    /// on the semaphore), NOT by this CPU-side `signal`. CPU-signalling
    /// `produce_done` before the GPU write has actually completed releases
    /// the shared surface early, and a subprocess consumer then samples a
    /// partially-written texture — a torn / black frame from a
    /// cross-process GPU write/read race the driver cannot observe across
    /// the process boundary. Use this CPU signal only for host-local
    /// timelines with no separately-scheduled GPU writer.
    pub fn signal(&self, value: u64) -> Result<()> {
        if self.methods.is_null() {
            return Err(Error::GpuError(
                "HostTimelineSemaphore::signal: methods vtable is null".into(),
            ));
        }
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods non-null per the guard.
        let status = unsafe {
            ((*self.methods).signal)(
                self.handle,
                value,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        status_to_unit(status, &err_buf, err_len)
    }

    /// Read the current timeline counter value (host-side
    /// `vkGetSemaphoreCounterValue`).
    pub fn current_value(&self) -> Result<u64> {
        if self.methods.is_null() {
            return Err(Error::GpuError(
                "HostTimelineSemaphore::current_value: methods vtable is null".into(),
            ));
        }
        let mut out_value: u64 = 0;
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods non-null per the guard; `out_value` is a valid
        // stack slot the host writes on success.
        let status = unsafe {
            ((*self.methods).current_value)(
                self.handle,
                &mut out_value as *mut u64,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(out_value)
        } else {
            Err(Error::GpuError(err_string(&err_buf, err_len)))
        }
    }

    /// Export a fresh OPAQUE_FD (`vkGetSemaphoreFdKHR`) for SCM_RIGHTS
    /// passing to a subprocess consumer. Each call returns a NEW fd; the
    /// caller owns it and must close it after use. On any error the fd is
    /// reported as `-1` (double-close guard) and an [`Error`] is returned.
    pub fn export_opaque_fd(&self) -> Result<i32> {
        if self.methods.is_null() {
            return Err(Error::GpuError(
                "HostTimelineSemaphore::export_opaque_fd: methods vtable is null".into(),
            ));
        }
        let mut out_fd: i32 = -1;
        let mut err_buf = [0u8; 256];
        let mut err_len: usize = 0;
        // SAFETY: methods non-null per the guard; `out_fd` is a valid
        // stack slot the host writes (the fresh fd on success, -1 on any
        // non-zero return).
        let status = unsafe {
            ((*self.methods).export_opaque_fd)(
                self.handle,
                &mut out_fd as *mut i32,
                err_buf.as_mut_ptr(),
                err_buf.len(),
                &mut err_len as *mut usize,
            )
        };
        if status == 0 {
            Ok(out_fd)
        } else {
            Err(Error::GpuError(err_string(&err_buf, err_len)))
        }
    }

    /// Raw inner-`Arc` handle pointer for the SurfaceStore producer path.
    /// This is the same `Arc::into_raw(Arc<HostVulkanTimelineSemaphore>)`
    /// pointer the host's `register_texture` slot derefs for its
    /// `produce_done` / `consume_done` sidecars. Crate-internal — the
    /// producer wrapper on [`crate::context::GpuContextFullAccess`] reads
    /// it; consumers hold the `HostTimelineSemaphore` value.
    pub(crate) fn cdylib_handle(&self) -> *const c_void {
        self.handle
    }
}

fn err_string(err_buf: &[u8], err_len: usize) -> String {
    String::from_utf8_lossy(&err_buf[..err_len.min(err_buf.len())]).into_owned()
}

fn status_to_unit(status: i32, err_buf: &[u8], err_len: usize) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(Error::GpuError(err_string(err_buf, err_len)))
    }
}

impl Clone for HostTimelineSemaphore {
    fn clone(&self) -> Self {
        if !self.handle.is_null() && !self.methods.is_null() {
            // SAFETY: handle + methods paired at mint time; the vtable's
            // `clone_handle` contract is `Arc::increment_strong_count`
            // host-side.
            unsafe {
                ((*self.methods).clone_handle)(self.handle);
            }
        }
        Self {
            handle: self.handle,
            methods: self.methods,
        }
    }
}

impl Drop for HostTimelineSemaphore {
    fn drop(&mut self) {
        if !self.handle.is_null() && !self.methods.is_null() {
            // SAFETY: matched with the host's `Arc::into_raw` and any
            // `clone_handle` bumps.
            unsafe {
                ((*self.methods).drop_handle)(self.handle);
            }
        }
    }
}

impl std::fmt::Debug for HostTimelineSemaphore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostTimelineSemaphore").finish()
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod layout_tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn host_timeline_semaphore_layout() {
        // Must match the engine's
        // `core/rhi/host_timeline_semaphore.rs::HostTimelineSemaphore`:
        //   handle  @ 0, methods @ 8. Total 16 bytes, align 8.
        assert_eq!(size_of::<HostTimelineSemaphore>(), 16);
        assert_eq!(align_of::<HostTimelineSemaphore>(), 8);
        assert_eq!(offset_of!(HostTimelineSemaphore, handle), 0);
        assert_eq!(offset_of!(HostTimelineSemaphore, methods), 8);
    }

    #[test]
    fn host_timeline_semaphore_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<HostTimelineSemaphore>();
    }

    #[test]
    fn methods_null_dispatch_is_typed_error_not_ub() {
        // A zero-initialized envelope (null handle + null methods) must
        // return a typed error from every method, never deref null.
        // Mental-revert: drop the `self.methods.is_null()` guards and each
        // call UB-derefs a null vtable pointer (SIGSEGV in the runner).
        let sem = HostTimelineSemaphore {
            handle: std::ptr::null(),
            methods: std::ptr::null(),
        };
        assert!(sem.wait(1, u64::MAX).is_err());
        assert!(sem.signal(1).is_err());
        assert!(sem.current_value().is_err());
        assert!(sem.export_opaque_fd().is_err());
        // Null handle + null methods: Drop must be a no-op (no dispatch).
        drop(sem);
    }
}
