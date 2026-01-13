// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! RHI external handle for cross-process GPU resource sharing.

use crate::core::Result;

/// Platform-agnostic GPU resource handle for cross-process sharing.
///
/// This enum represents a handle that can be sent to another process,
/// which can then import the GPU resource without copying data.
///
/// On macOS, XPC is used to transfer IOSurface objects directly via
/// `IOSurfaceCreateXPCObject()` and `IOSurfaceLookupFromXPCObject()`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RhiExternalHandle {
    /// macOS: IOSurface ID (u32).
    /// Can be looked up in another process via IOSurfaceLookup().
    /// Note: This only works with kIOSurfaceIsGlobal (deprecated/removed).
    #[cfg(target_os = "macos")]
    IOSurface { id: u32 },

    /// macOS: IOSurface via mach port for cross-process sharing.
    /// The mach port is created via IOSurfaceCreateMachPort().
    #[cfg(target_os = "macos")]
    IOSurfaceMachPort { port: u32 },

    /// macOS: IOSurface via XPC object for cross-process sharing.
    /// The XPC object is created via IOSurfaceCreateXPCObject() and
    /// transferred via XPC connection. This is the preferred method
    /// as XPC handles mach port transfer automatically.
    #[cfg(target_os = "macos")]
    IOSurfaceXpc {
        /// Opaque XPC object pointer. Sent via XPC connection, not serialized.
        xpc_object: *mut std::ffi::c_void,
    },

    /// Linux: DMA-BUF file descriptor.
    /// Must be passed via SCM_RIGHTS ancillary data.
    #[cfg(target_os = "linux")]
    DmaBuf {
        fd: std::os::unix::io::RawFd,
        size: usize,
    },

    /// Windows: DXGI shared handle.
    /// Can be opened in another process via OpenSharedHandle().
    #[cfg(target_os = "windows")]
    DxgiShared { handle: *mut std::ffi::c_void },
}

// SAFETY: RhiExternalHandle is Send because it contains only handles/IDs
// that can be safely sent between threads.
unsafe impl Send for RhiExternalHandle {}
unsafe impl Sync for RhiExternalHandle {}

impl RhiExternalHandle {
    /// Extract the mach port from an IOSurfaceMachPort handle (macOS only).
    #[cfg(target_os = "macos")]
    pub fn mach_port(&self) -> Option<u32> {
        match self {
            RhiExternalHandle::IOSurfaceMachPort { port } => Some(*port),
            _ => None,
        }
    }
}

/// Extension trait for exporting RhiPixelBuffer to external handle.
pub trait RhiPixelBufferExport {
    /// Export the GPU buffer for sharing with another process.
    fn export_handle(&self) -> Result<RhiExternalHandle>;
}

/// Extension trait for importing RhiPixelBuffer from external handle.
pub trait RhiPixelBufferImport {
    /// Import a GPU buffer from an external handle.
    fn from_external_handle(
        handle: RhiExternalHandle,
        width: u32,
        height: u32,
        format: super::PixelFormat,
    ) -> Result<Self>
    where
        Self: Sized;
}
