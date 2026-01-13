// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! macOS RhiPixelBufferRef implementation.

use std::ptr::NonNull;

use crate::apple::corevideo_ffi::{
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    CVPixelBufferRef, CVPixelBufferRelease, CVPixelBufferRetain,
};
use crate::core::rhi::{PixelFormat, RhiPixelBufferRef};

impl RhiPixelBufferRef {
    /// Create from a raw CVPixelBufferRef.
    ///
    /// # Safety
    /// The caller must ensure the CVPixelBufferRef is valid.
    /// This function retains the buffer (increments refcount).
    pub unsafe fn from_cv_pixel_buffer(cv_buffer: CVPixelBufferRef) -> Option<Self> {
        if cv_buffer.is_null() {
            return None;
        }
        // Retain the buffer
        CVPixelBufferRetain(cv_buffer);
        Some(Self {
            inner: NonNull::new_unchecked(cv_buffer),
        })
    }

    /// Create from a raw CVPixelBufferRef without retaining.
    ///
    /// # Safety
    /// The caller must ensure the CVPixelBufferRef is valid and already retained.
    /// Use this when you're taking ownership of a buffer that was already retained
    /// (e.g., from AVFoundation callback).
    pub unsafe fn from_cv_pixel_buffer_no_retain(cv_buffer: CVPixelBufferRef) -> Option<Self> {
        if cv_buffer.is_null() {
            return None;
        }
        Some(Self {
            inner: NonNull::new_unchecked(cv_buffer),
        })
    }

    /// Get the raw CVPixelBufferRef.
    pub fn cv_pixel_buffer(&self) -> CVPixelBufferRef {
        self.inner.as_ptr()
    }

    /// Get the IOSurfaceRef backing this buffer.
    ///
    /// Returns the raw IOSurfaceRef for XPC-based cross-process sharing.
    /// Returns None if the buffer is not backed by an IOSurface.
    pub fn iosurface_ref(&self) -> Option<crate::apple::corevideo_ffi::IOSurfaceRef> {
        let surface =
            unsafe { crate::apple::corevideo_ffi::CVPixelBufferGetIOSurface(self.inner.as_ptr()) };
        if surface.is_null() {
            None
        } else {
            Some(surface)
        }
    }
}

/// Query pixel format from CVPixelBuffer.
pub(crate) fn format_impl(buffer_ref: &RhiPixelBufferRef) -> PixelFormat {
    let cv_format = unsafe { CVPixelBufferGetPixelFormatType(buffer_ref.inner.as_ptr()) };
    PixelFormat::from_cv_pixel_format_type(cv_format)
}

/// Query width from CVPixelBuffer.
pub(crate) fn width_impl(buffer_ref: &RhiPixelBufferRef) -> u32 {
    unsafe { CVPixelBufferGetWidth(buffer_ref.inner.as_ptr()) as u32 }
}

/// Query height from CVPixelBuffer.
pub(crate) fn height_impl(buffer_ref: &RhiPixelBufferRef) -> u32 {
    unsafe { CVPixelBufferGetHeight(buffer_ref.inner.as_ptr()) as u32 }
}

/// Clone implementation - retains the CVPixelBuffer.
pub(crate) fn clone_impl(buffer_ref: &RhiPixelBufferRef) -> RhiPixelBufferRef {
    unsafe {
        CVPixelBufferRetain(buffer_ref.inner.as_ptr());
        RhiPixelBufferRef {
            inner: buffer_ref.inner,
        }
    }
}

/// Drop implementation - releases the CVPixelBuffer.
pub(crate) fn drop_impl(buffer_ref: &mut RhiPixelBufferRef) {
    unsafe {
        CVPixelBufferRelease(buffer_ref.inner.as_ptr());
    }
}

// ============================================================================
// External Handle Export/Import for Cross-Process GPU Sharing
// ============================================================================

use crate::apple::corevideo_ffi::{
    mach_port_deallocate, mach_task_self, CVPixelBufferCreateWithIOSurface,
    CVPixelBufferGetIOSurface, IOSurfaceCreateMachPort, IOSurfaceGetID, IOSurfaceLookup,
    IOSurfaceLookupFromMachPort,
};
use crate::core::rhi::{RhiExternalHandle, RhiPixelBufferExport, RhiPixelBufferImport};
use crate::core::{Result, StreamError};

impl RhiPixelBufferExport for RhiPixelBufferRef {
    /// Export the CVPixelBuffer's IOSurface for cross-process sharing.
    ///
    /// Uses IOSurface ID for sharing. Requires kIOSurfaceIsGlobal to be set
    /// on the IOSurface (which RhiPixelBufferPool does automatically).
    ///
    /// For camera frames (which don't have kIOSurfaceIsGlobal), use
    /// `export_handle_as_mach_port()` instead.
    fn export_handle(&self) -> Result<RhiExternalHandle> {
        let cv_buffer = self.inner.as_ptr();

        // Get the IOSurface backing this CVPixelBuffer
        let iosurface = unsafe { CVPixelBufferGetIOSurface(cv_buffer) };

        if iosurface.is_null() {
            tracing::error!(
                "IOSurface export (ID) FAILED: CVPixelBuffer {:p} is not backed by an IOSurface (pid={})",
                cv_buffer,
                std::process::id()
            );
            return Err(StreamError::Configuration(
                "CVPixelBuffer is not backed by an IOSurface".into(),
            ));
        }

        // Get the IOSurface ID for cross-process sharing
        // This requires kIOSurfaceIsGlobal to be set (done by RhiPixelBufferPool)
        let id = unsafe { IOSurfaceGetID(iosurface) };

        if id == 0 {
            tracing::error!(
                "IOSurface export (ID) FAILED: ID is 0 for IOSurface {:p} (pid={}). \
                 This may indicate the IOSurface doesn't have kIOSurfaceIsGlobal set. \
                 For camera frames, use export_handle_as_mach_port() instead.",
                iosurface,
                std::process::id()
            );
            return Err(StreamError::Configuration(
                "IOSurface has invalid ID 0".into(),
            ));
        }

        tracing::trace!(
            "IOSurface export (ID): CVPixelBuffer {:p} -> IOSurface {:p} (ID={}) (pid={})",
            cv_buffer,
            iosurface,
            id,
            std::process::id()
        );

        Ok(RhiExternalHandle::IOSurface { id })
    }
}

impl RhiPixelBufferRef {
    /// Export the CVPixelBuffer's IOSurface as a mach port for cross-process sharing.
    ///
    /// This is the recommended method for cross-process IOSurface sharing on macOS.
    /// It works for ALL IOSurfaces, including camera frames that don't have
    /// kIOSurfaceIsGlobal set.
    ///
    /// The returned mach port should be sent via SCM_RIGHTS ancillary data.
    /// The receiving process uses `IOSurfaceLookupFromMachPort()`.
    ///
    /// Returns: `(RhiExternalHandle::IOSurfaceMachPort, mach_port_value)` where
    /// mach_port_value is the raw port to send via SCM_RIGHTS.
    pub fn export_handle_as_mach_port(&self) -> Result<(RhiExternalHandle, u32)> {
        let cv_buffer = self.inner.as_ptr();

        // Get the IOSurface backing this CVPixelBuffer
        let iosurface = unsafe { CVPixelBufferGetIOSurface(cv_buffer) };

        if iosurface.is_null() {
            tracing::error!(
                "IOSurface export (mach port) FAILED: CVPixelBuffer {:p} is not backed by an IOSurface (pid={})",
                cv_buffer,
                std::process::id()
            );
            return Err(StreamError::Configuration(
                "CVPixelBuffer is not backed by an IOSurface".into(),
            ));
        }

        // Get IOSurface ID for logging/debugging
        let id = unsafe { IOSurfaceGetID(iosurface) };

        // Create mach port for cross-process sharing
        // This works for ANY IOSurface, even without kIOSurfaceIsGlobal
        let mach_port = unsafe { IOSurfaceCreateMachPort(iosurface) };

        if mach_port == 0 {
            tracing::error!(
                "IOSurface export (mach port) FAILED: IOSurfaceCreateMachPort returned 0 \
                 for IOSurface {:p} (ID={}) (pid={})",
                iosurface,
                id,
                std::process::id()
            );
            return Err(StreamError::Configuration(
                "IOSurfaceCreateMachPort failed".into(),
            ));
        }

        tracing::trace!(
            "IOSurface export (mach port): CVPixelBuffer {:p} -> IOSurface {:p} (ID={}) -> mach_port={} (pid={})",
            cv_buffer,
            iosurface,
            id,
            mach_port,
            std::process::id()
        );

        Ok((
            RhiExternalHandle::IOSurfaceMachPort { port: mach_port },
            mach_port,
        ))
    }

    /// Export the CVPixelBuffer's IOSurface as an XPC object for cross-process sharing.
    ///
    /// This is the preferred method for cross-process IOSurface sharing on macOS.
    /// XPC handles mach port transfer automatically, eliminating the need for
    /// SCM_RIGHTS handling.
    ///
    /// Returns: `RhiExternalHandle::IOSurfaceXpc` containing the XPC object pointer.
    /// The XPC object should be sent via XPC connection (see `XpcFrameChannel`).
    pub fn export_handle_as_xpc(&self) -> Result<RhiExternalHandle> {
        // IOSurface XPC functions for cross-process sharing
        #[link(name = "IOSurface", kind = "framework")]
        extern "C" {
            fn IOSurfaceCreateXPCObject(surface: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }

        let cv_buffer = self.inner.as_ptr();

        // Get the IOSurface backing this CVPixelBuffer
        let iosurface = unsafe { CVPixelBufferGetIOSurface(cv_buffer) };

        if iosurface.is_null() {
            tracing::error!(
                "IOSurface export (XPC) FAILED: CVPixelBuffer {:p} is not backed by an IOSurface (pid={})",
                cv_buffer,
                std::process::id()
            );
            return Err(StreamError::Configuration(
                "CVPixelBuffer is not backed by an IOSurface".into(),
            ));
        }

        // Get IOSurface ID for logging/debugging
        let id = unsafe { IOSurfaceGetID(iosurface) };

        // Create XPC object for cross-process sharing
        // Cast to *mut c_void as IOSurfaceCreateXPCObject doesn't modify the surface
        let xpc_object = unsafe { IOSurfaceCreateXPCObject(iosurface as *mut std::ffi::c_void) };

        if xpc_object.is_null() {
            tracing::error!(
                "IOSurface export (XPC) FAILED: IOSurfaceCreateXPCObject returned null \
                 for IOSurface {:p} (ID={}) (pid={})",
                iosurface,
                id,
                std::process::id()
            );
            return Err(StreamError::Configuration(
                "IOSurfaceCreateXPCObject failed".into(),
            ));
        }

        tracing::trace!(
            "IOSurface export (XPC): CVPixelBuffer {:p} -> IOSurface {:p} (ID={}) -> xpc_object={:?} (pid={})",
            cv_buffer,
            iosurface,
            id,
            xpc_object,
            std::process::id()
        );

        Ok(RhiExternalHandle::IOSurfaceXpc { xpc_object })
    }
}

impl RhiPixelBufferImport for RhiPixelBufferRef {
    /// Import a CVPixelBuffer from an external handle.
    ///
    /// Supports:
    /// - `IOSurface { id }`: Legacy lookup by ID (requires kIOSurfaceIsGlobal, deprecated)
    /// - `IOSurfaceMachPort { port }`: Modern mach port-based sharing
    fn from_external_handle(
        handle: RhiExternalHandle,
        width: u32,
        height: u32,
        format: PixelFormat,
    ) -> Result<Self> {
        match handle {
            RhiExternalHandle::IOSurface { id } => {
                tracing::trace!(
                    "IOSurface import (ID): attempting lookup of ID {} (pid={}, {}x{} {:?})",
                    id,
                    std::process::id(),
                    width,
                    height,
                    format
                );

                // Look up the IOSurface by ID (only works with kIOSurfaceIsGlobal)
                let iosurface = unsafe { IOSurfaceLookup(id) };

                if iosurface.is_null() {
                    tracing::error!(
                        "IOSurface import FAILED: ID {} not found (pid={}). \
                         This may indicate the IOSurface was released or is not globally accessible. \
                         Consider using IOSurfaceMachPort for cross-process sharing.",
                        id,
                        std::process::id()
                    );
                    return Err(StreamError::Configuration(format!(
                        "IOSurface with ID {} not found",
                        id
                    )));
                }

                tracing::trace!(
                    "IOSurface import: lookup succeeded - ID {} -> IOSurface {:p} (pid={})",
                    id,
                    iosurface,
                    std::process::id()
                );

                create_cv_buffer_from_iosurface(iosurface)
            }

            RhiExternalHandle::IOSurfaceMachPort { port } => {
                tracing::trace!(
                    "IOSurface import (mach port): looking up mach_port={} (pid={}, {}x{} {:?})",
                    port,
                    std::process::id(),
                    width,
                    height,
                    format
                );

                // Look up the IOSurface from the mach port
                let iosurface = unsafe { IOSurfaceLookupFromMachPort(port) };

                if iosurface.is_null() {
                    tracing::error!(
                        "IOSurface import FAILED: mach_port={} lookup returned null (pid={}). \
                         The mach port may be invalid or already deallocated.",
                        port,
                        std::process::id()
                    );
                    return Err(StreamError::Configuration(format!(
                        "IOSurface mach port {} lookup failed",
                        port
                    )));
                }

                // Get the ID for logging
                let id = unsafe { IOSurfaceGetID(iosurface) };
                tracing::trace!(
                    "IOSurface import: mach_port={} -> IOSurface {:p} (ID={}) (pid={})",
                    port,
                    iosurface,
                    id,
                    std::process::id()
                );

                // Deallocate the mach port now that we have the IOSurface reference
                // The IOSurface itself is retained by IOSurfaceLookupFromMachPort
                let task = unsafe { mach_task_self() };
                let result = unsafe { mach_port_deallocate(task, port) };
                if result != 0 {
                    tracing::warn!(
                        "Failed to deallocate mach_port={}: error {} (pid={})",
                        port,
                        result,
                        std::process::id()
                    );
                }

                create_cv_buffer_from_iosurface(iosurface)
            }

            RhiExternalHandle::IOSurfaceXpc { xpc_object } => {
                // IOSurface XPC functions for cross-process sharing
                #[link(name = "IOSurface", kind = "framework")]
                extern "C" {
                    fn IOSurfaceLookupFromXPCObject(
                        xobj: *mut std::ffi::c_void,
                    ) -> *mut std::ffi::c_void;
                }

                tracing::trace!(
                    "IOSurface import (XPC): looking up xpc_object={:?} (pid={}, {}x{} {:?})",
                    xpc_object,
                    std::process::id(),
                    width,
                    height,
                    format
                );

                // Look up the IOSurface from the XPC object
                let iosurface = unsafe { IOSurfaceLookupFromXPCObject(xpc_object) };

                if iosurface.is_null() {
                    tracing::error!(
                        "IOSurface import FAILED: XPC object {:?} lookup returned null (pid={}). \
                         The XPC object may be invalid.",
                        xpc_object,
                        std::process::id()
                    );
                    return Err(StreamError::Configuration(
                        "IOSurface XPC object lookup failed".into(),
                    ));
                }

                // Get the ID for logging
                let id = unsafe { IOSurfaceGetID(iosurface) };
                tracing::trace!(
                    "IOSurface import: xpc_object={:?} -> IOSurface {:p} (ID={}) (pid={})",
                    xpc_object,
                    iosurface,
                    id,
                    std::process::id()
                );

                create_cv_buffer_from_iosurface(iosurface)
            }

            #[allow(unreachable_patterns)]
            _ => Err(StreamError::NotSupported(
                "External handle type not supported on macOS".into(),
            )),
        }
    }
}

/// Helper to create CVPixelBuffer from IOSurface.
fn create_cv_buffer_from_iosurface(
    iosurface: crate::apple::corevideo_ffi::IOSurfaceRef,
) -> Result<RhiPixelBufferRef> {
    let mut cv_buffer: crate::apple::corevideo_ffi::CVPixelBufferRef = std::ptr::null_mut();

    let result = unsafe {
        CVPixelBufferCreateWithIOSurface(
            std::ptr::null(), // default allocator
            iosurface,
            std::ptr::null(), // no attributes needed
            &mut cv_buffer,
        )
    };

    if result != crate::apple::corevideo_ffi::kCVReturnSuccess {
        tracing::error!(
            "CVPixelBufferCreateWithIOSurface failed with error {} (pid={})",
            result,
            std::process::id()
        );
        return Err(StreamError::Configuration(format!(
            "Failed to create CVPixelBuffer from IOSurface: error {}",
            result
        )));
    }

    if cv_buffer.is_null() {
        tracing::error!(
            "CVPixelBufferCreateWithIOSurface returned null (pid={})",
            std::process::id()
        );
        return Err(StreamError::Configuration(
            "CVPixelBufferCreateWithIOSurface returned null".into(),
        ));
    }

    tracing::trace!(
        "IOSurface import SUCCESS: CVPixelBuffer {:p} (pid={})",
        cv_buffer,
        std::process::id()
    );

    // The CVPixelBuffer is already retained by CreateWithIOSurface,
    // so use from_cv_pixel_buffer_no_retain
    unsafe { RhiPixelBufferRef::from_cv_pixel_buffer_no_retain(cv_buffer) }
        .ok_or_else(|| StreamError::Configuration("Failed to wrap CVPixelBuffer".into()))
}

/// Create an RhiPixelBufferRef from a raw IOSurfaceRef.
///
/// This is the implementation called from core/rhi/pixel_buffer_ref.rs.
///
/// # Safety
/// The caller must ensure the IOSurfaceRef is valid.
pub unsafe fn from_iosurface_ref_impl(
    iosurface: crate::apple::corevideo_ffi::IOSurfaceRef,
) -> Result<RhiPixelBufferRef> {
    if iosurface.is_null() {
        return Err(StreamError::Configuration("IOSurfaceRef is null".into()));
    }

    let id = IOSurfaceGetID(iosurface);
    tracing::trace!(
        "from_iosurface_ref_impl: IOSurface {:p} (ID={}) (pid={})",
        iosurface,
        id,
        std::process::id()
    );

    create_cv_buffer_from_iosurface(iosurface)
}
