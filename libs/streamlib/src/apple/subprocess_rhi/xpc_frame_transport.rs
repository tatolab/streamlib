// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! XPC frame transport for IOSurface and shared memory.

use std::ffi::c_void;
use std::ptr::null_mut;

use tracing::trace;

use mach2::kern_return::KERN_SUCCESS;
use mach2::traps::mach_task_self;
use mach2::vm::{mach_vm_allocate, mach_vm_deallocate};
use mach2::vm_types::mach_vm_address_t;

use xpc_bindgen::{xpc_release, xpc_shmem_create, xpc_shmem_map};

use crate::core::error::StreamError;
use crate::core::subprocess_rhi::{FrameTransportHandle, SubprocessRhiFrameTransport};

// IOSurface XPC functions
#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    fn IOSurfaceCreateXPCObject(surface: *mut c_void) -> *mut c_void;
    fn IOSurfaceLookupFromXPCObject(xobj: *mut c_void) -> *mut c_void;
}

/// XPC frame transport implementation for macOS.
pub struct XpcFrameTransport;

impl SubprocessRhiFrameTransport for XpcFrameTransport {
    fn export_iosurface(surface: *mut c_void) -> Result<FrameTransportHandle, StreamError> {
        if surface.is_null() {
            return Err(StreamError::Configuration(
                "Cannot export null IOSurface".to_string(),
            ));
        }

        unsafe {
            let xpc_obj = IOSurfaceCreateXPCObject(surface);

            if xpc_obj.is_null() {
                return Err(StreamError::Configuration(
                    "Failed to create XPC object from IOSurface".to_string(),
                ));
            }

            trace!(
                "[XpcFrameTransport] Exported IOSurface as XPC object: {:p}",
                xpc_obj
            );
            Ok(FrameTransportHandle::GpuSurface {
                xpc_object: xpc_obj,
            })
        }
    }

    fn import_iosurface(handle: FrameTransportHandle) -> Result<*mut c_void, StreamError> {
        match handle {
            FrameTransportHandle::GpuSurface { xpc_object } => {
                if xpc_object.is_null() {
                    return Err(StreamError::Configuration(
                        "Cannot import from null XPC object".to_string(),
                    ));
                }

                unsafe {
                    let surface = IOSurfaceLookupFromXPCObject(xpc_object);

                    if surface.is_null() {
                        return Err(StreamError::Configuration(
                            "Failed to lookup IOSurface from XPC object".to_string(),
                        ));
                    }

                    trace!(
                        "[XpcFrameTransport] Imported IOSurface from XPC object: {:p}",
                        surface
                    );
                    Ok(surface)
                }
            }
            FrameTransportHandle::SharedMemory { .. } => Err(StreamError::Configuration(
                "Cannot import IOSurface from SharedMemory handle".to_string(),
            )),
        }
    }

    fn create_shared_memory(length: usize) -> Result<(FrameTransportHandle, *mut u8), StreamError> {
        // Page-align the size
        let page_size = 4096usize;
        let alloc_size = (length + page_size - 1) & !(page_size - 1);

        unsafe {
            // MUST use mach_vm_allocate, NOT malloc (XPC requirement)
            let mut region: mach_vm_address_t = 0;
            let kr = mach_vm_allocate(mach_task_self(), &mut region, alloc_size as u64, 1); // VM_FLAGS_ANYWHERE = 1

            if kr != KERN_SUCCESS {
                return Err(StreamError::Configuration(format!(
                    "Failed to allocate mach VM memory: {}",
                    kr
                )));
            }

            trace!(
                "[XpcFrameTransport] Allocated mach VM at 0x{:x}, size: {}",
                region,
                alloc_size
            );

            // Create XPC shmem object
            let shmem = xpc_shmem_create(region as *mut c_void, alloc_size);

            if shmem.is_null() {
                mach_vm_deallocate(mach_task_self(), region, alloc_size as u64);
                return Err(StreamError::Configuration(
                    "Failed to create xpc_shmem".to_string(),
                ));
            }

            trace!("[XpcFrameTransport] Created xpc_shmem object: {:p}", shmem);

            let handle = FrameTransportHandle::SharedMemory {
                xpc_shmem: shmem,
                length: alloc_size,
            };

            Ok((handle, region as *mut u8))
        }
    }

    fn map_shared_memory(handle: &FrameTransportHandle) -> Result<*const u8, StreamError> {
        match handle {
            FrameTransportHandle::SharedMemory { xpc_shmem, .. } => {
                if xpc_shmem.is_null() {
                    return Err(StreamError::Configuration(
                        "Cannot map null xpc_shmem".to_string(),
                    ));
                }

                unsafe {
                    let mut mapped_region: *mut c_void = null_mut();
                    let mapped_size = xpc_shmem_map(*xpc_shmem, &mut mapped_region);

                    if mapped_size == 0 || mapped_region.is_null() {
                        return Err(StreamError::Configuration(
                            "Failed to map xpc_shmem".to_string(),
                        ));
                    }

                    trace!(
                        "[XpcFrameTransport] Mapped xpc_shmem at {:p}, size: {}",
                        mapped_region,
                        mapped_size
                    );

                    Ok(mapped_region as *const u8)
                }
            }
            FrameTransportHandle::GpuSurface { .. } => Err(StreamError::Configuration(
                "Cannot map GpuSurface as shared memory".to_string(),
            )),
        }
    }

    fn unmap_shared_memory(ptr: *const u8, length: usize) -> Result<(), StreamError> {
        if ptr.is_null() {
            return Err(StreamError::Configuration(
                "Cannot unmap null pointer".to_string(),
            ));
        }

        unsafe {
            let result = libc::munmap(ptr as *mut c_void, length);

            if result != 0 {
                return Err(StreamError::Configuration(format!(
                    "Failed to unmap shared memory: {}",
                    std::io::Error::last_os_error()
                )));
            }

            trace!("[XpcFrameTransport] Unmapped shared memory at {:p}", ptr);
            Ok(())
        }
    }
}

/// Helper to release shared memory resources.
pub fn release_frame_transport_handle(handle: FrameTransportHandle) {
    unsafe {
        match handle {
            FrameTransportHandle::GpuSurface { xpc_object } => {
                if !xpc_object.is_null() {
                    xpc_release(xpc_object);
                }
            }
            FrameTransportHandle::SharedMemory { xpc_shmem, .. } => {
                if !xpc_shmem.is_null() {
                    xpc_release(xpc_shmem);
                }
            }
        }
    }
}
