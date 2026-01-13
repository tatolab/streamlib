// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Frame transport handles for cross-process frame sharing.

use std::ffi::c_void;

#[cfg(target_os = "linux")]
use std::os::unix::io::RawFd;

use crate::core::error::StreamError;

/// Platform-specific handle for transporting frames across process boundaries.
///
/// This enum represents the low-level transport mechanism for each platform:
/// - macOS: IOSurface XPC objects for GPU frames, xpc_shmem for CPU frames
/// - Linux: DMA-BUF file descriptors for GPU frames, memfd for CPU frames
/// - Windows: DXGI shared handles for GPU frames, named shared memory for CPU frames
#[derive(Debug)]
pub enum FrameTransportHandle {
    // ===================
    // macOS Variants
    // ===================
    /// GPU frame via IOSurface XPC object (macOS).
    ///
    /// Created with `IOSurfaceCreateXPCObject`, imported with `IOSurfaceLookupFromXPCObject`.
    /// Zero-copy GPU texture sharing.
    #[cfg(target_os = "macos")]
    GpuSurface {
        /// XPC object created by `IOSurfaceCreateXPCObject`.
        xpc_object: *mut c_void,
    },

    /// CPU frame via xpc_shmem (macOS).
    ///
    /// Created with `xpc_shmem_create`, imported with `xpc_shmem_map`.
    /// Zero-copy shared memory for AudioFrame/DataFrame.
    /// IMPORTANT: Memory MUST be allocated with `mach_vm_allocate`, NOT malloc.
    #[cfg(target_os = "macos")]
    SharedMemory {
        /// XPC shared memory object.
        xpc_shmem: *mut c_void,
        /// Length of the shared memory region in bytes.
        length: usize,
    },

    // ===================
    // Linux Variants
    // ===================
    /// GPU frame via DMA-BUF file descriptor (Linux).
    ///
    /// Used for GPU texture sharing via `dma_buf_fd`.
    #[cfg(target_os = "linux")]
    DmaBuf {
        /// DMA-BUF file descriptor.
        fd: RawFd,
    },

    /// CPU frame via memfd (Linux).
    ///
    /// Created with `memfd_create`, shared via Unix domain socket fd passing.
    #[cfg(target_os = "linux")]
    Memfd {
        /// Memory file descriptor.
        fd: RawFd,
        /// Length of the shared memory region in bytes.
        length: usize,
    },

    // ===================
    // Windows Variants
    // ===================
    /// GPU frame via DXGI shared handle (Windows).
    #[cfg(target_os = "windows")]
    DxgiSharedHandle {
        /// DXGI shared handle (HANDLE).
        handle: *mut c_void,
    },

    /// CPU frame via named shared memory (Windows).
    #[cfg(target_os = "windows")]
    NamedSharedMemory {
        /// Name of the shared memory object.
        name: String,
        /// Length of the shared memory region in bytes.
        length: usize,
    },
}

// Safety: The raw pointers are XPC objects managed by the XPC runtime
// which is thread-safe.
#[cfg(target_os = "macos")]
unsafe impl Send for FrameTransportHandle {}
#[cfg(target_os = "macos")]
unsafe impl Sync for FrameTransportHandle {}

#[cfg(target_os = "linux")]
unsafe impl Send for FrameTransportHandle {}
#[cfg(target_os = "linux")]
unsafe impl Sync for FrameTransportHandle {}

#[cfg(target_os = "windows")]
unsafe impl Send for FrameTransportHandle {}
#[cfg(target_os = "windows")]
unsafe impl Sync for FrameTransportHandle {}

impl FrameTransportHandle {
    /// Check if this handle represents a GPU frame.
    pub fn is_gpu_frame(&self) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            Self::GpuSurface { .. } => true,
            #[cfg(target_os = "macos")]
            Self::SharedMemory { .. } => false,

            #[cfg(target_os = "linux")]
            Self::DmaBuf { .. } => true,
            #[cfg(target_os = "linux")]
            Self::Memfd { .. } => false,

            #[cfg(target_os = "windows")]
            Self::DxgiSharedHandle { .. } => true,
            #[cfg(target_os = "windows")]
            Self::NamedSharedMemory { .. } => false,
        }
    }

    /// Check if this handle represents a CPU frame.
    pub fn is_cpu_frame(&self) -> bool {
        !self.is_gpu_frame()
    }

    /// Get the size of the CPU frame data (if applicable).
    pub fn cpu_frame_length(&self) -> Option<usize> {
        match self {
            #[cfg(target_os = "macos")]
            Self::SharedMemory { length, .. } => Some(*length),

            #[cfg(target_os = "linux")]
            Self::Memfd { length, .. } => Some(*length),

            #[cfg(target_os = "windows")]
            Self::NamedSharedMemory { length, .. } => Some(*length),

            _ => None,
        }
    }
}

/// Frame transport trait for exporting/importing frames across process boundaries.
pub trait SubprocessRhiFrameTransport: Send + Sync {
    // ===================
    // GPU Frame Export/Import
    // ===================

    /// Export an IOSurface as a transport handle (macOS).
    #[cfg(target_os = "macos")]
    fn export_iosurface(surface: *mut c_void) -> Result<FrameTransportHandle, StreamError>;

    /// Import an IOSurface from a transport handle (macOS).
    #[cfg(target_os = "macos")]
    fn import_iosurface(handle: FrameTransportHandle) -> Result<*mut c_void, StreamError>;

    // ===================
    // CPU Frame Export/Import
    // ===================

    /// Create a shared memory region for CPU frame data (macOS).
    ///
    /// Allocates memory with `mach_vm_allocate` and creates an XPC shmem object.
    /// Returns the handle and a pointer to the writable memory region.
    #[cfg(target_os = "macos")]
    fn create_shared_memory(length: usize) -> Result<(FrameTransportHandle, *mut u8), StreamError>;

    /// Map a received shared memory handle to local address space (macOS).
    ///
    /// Returns a pointer to the readable memory region.
    #[cfg(target_os = "macos")]
    fn map_shared_memory(handle: &FrameTransportHandle) -> Result<*const u8, StreamError>;

    /// Unmap a previously mapped shared memory region (macOS).
    #[cfg(target_os = "macos")]
    fn unmap_shared_memory(ptr: *const u8, length: usize) -> Result<(), StreamError>;

    // ===================
    // Linux Variants
    // ===================

    /// Export a DMA-BUF as a transport handle (Linux).
    #[cfg(target_os = "linux")]
    fn export_dmabuf(fd: RawFd) -> Result<FrameTransportHandle, StreamError>;

    /// Import a DMA-BUF from a transport handle (Linux).
    #[cfg(target_os = "linux")]
    fn import_dmabuf(handle: FrameTransportHandle) -> Result<RawFd, StreamError>;

    /// Create a memfd for CPU frame data (Linux).
    #[cfg(target_os = "linux")]
    fn create_memfd(
        name: &str,
        length: usize,
    ) -> Result<(FrameTransportHandle, *mut u8), StreamError>;

    /// Map a received memfd handle (Linux).
    #[cfg(target_os = "linux")]
    fn map_memfd(handle: &FrameTransportHandle) -> Result<*const u8, StreamError>;
}
