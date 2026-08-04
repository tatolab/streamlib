// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The device half of the pixel-exchange surface: handing CUDA a pointer
//! into memory the engine allocated, without a trip through host memory.
//!
//! The engine exports an OPAQUE_FD for a `StorageBuffer` it owns; CUDA
//! imports it with `cudaImportExternalMemory` and maps a device pointer
//! that aliases the same kernel allocation the `VkBuffer` is bound to.
//! Nothing is copied and CUDA never allocates — which is the whole point:
//! external native code receives memory in its own dialect, and Vulkan is
//! still written only by this engine.
//!
//! Three details are load-bearing and easy to get silently wrong.
//!
//! **Device binding.** The import must land on the GPU that owns the
//! memory, so the CUDA device is selected by matching
//! `cudaDeviceProp::uuid` against the exporting device's
//! `VkPhysicalDeviceIDProperties::deviceUUID`. Falling through to CUDA
//! device 0 works on every single-GPU rig and corrupts silently on a
//! multi-GPU one.
//!
//! **Fd ownership.** `cudaImportExternalMemory` takes the fd on success
//! and leaves it with the caller on failure — so the failure paths close
//! it and the success path must not.
//!
//! **Pointer classification.** A driver that downgraded the import to
//! pinned-host memory would still return a usable pointer, but a DLPack
//! capsule claiming `kDLCUDA` over it makes consumers issue device
//! copies against host memory. `cudaPointerGetAttributes` is what tells
//! the two apart, so the capsule reports what the driver actually did.

use std::os::unix::io::RawFd;

use cudarc::runtime::result::external_memory;
use cudarc::runtime::sys;
use streamlib_adapter_cuda::dlpack::{Device, DeviceType};

/// A CUDA import of engine-owned memory, alive for as long as the
/// surface it came from.
pub(crate) struct CudaImportedSurface {
    external_memory: sys::cudaExternalMemory_t,
    device_pointer: u64,
    dlpack_device: Device,
}

// SAFETY: the CUDA handles are plain driver-owned pointers with no
// thread affinity — the runtime API is thread-safe, and the only
// operation this type performs on them is a destroy in `Drop`.
unsafe impl Send for CudaImportedSurface {}
unsafe impl Sync for CudaImportedSurface {}

impl CudaImportedSurface {
    pub(crate) fn device_pointer(&self) -> u64 {
        self.device_pointer
    }

    /// What the driver actually produced — `kDLCUDA` for true device
    /// memory, `kDLCUDAHost` if it classified the import as pinned host.
    pub(crate) fn dlpack_device(&self) -> Device {
        self.dlpack_device
    }
}

impl Drop for CudaImportedSurface {
    fn drop(&mut self) {
        // SAFETY: `external_memory` came from a successful
        // `cudaImportExternalMemory` and is destroyed exactly once.
        if let Err(destroy_failure) =
            unsafe { external_memory::destroy_external_memory(self.external_memory) }
        {
            tracing::debug!(?destroy_failure, "cudaDestroyExternalMemory failed");
        }
    }
}

/// Why a device export was not possible. Rendered straight into the
/// Python exception, so each variant says what to do about it.
pub(crate) fn cuda_unavailable_reason() -> Option<String> {
    // SAFETY: the probe only attempts the dlopen; it touches no CUDA state.
    if !unsafe { sys::is_culib_present() } {
        return Some(
            "the CUDA driver library is not loadable: install an NVIDIA driver, or use the host \
             mapping (`as_numpy`) instead"
                .to_string(),
        );
    }
    None
}

/// Select the CUDA device whose UUID matches the exporting Vulkan
/// device, and make it current.
///
/// The UUID match is the entire device-binding contract. A silent
/// fall-through to device 0 would import the memory onto the wrong GPU
/// on a multi-GPU rig and produce garbage rather than an error.
fn bind_cuda_device_by_uuid(vulkan_device_uuid: [u8; 16]) -> Result<i32, String> {
    let device_count = unsafe {
        let mut count: i32 = 0;
        sys::cudaGetDeviceCount(&mut count)
            .result()
            .map_err(|failure| format!("cudaGetDeviceCount: {failure:?}"))?;
        count
    };
    for ordinal in 0..device_count {
        let properties = unsafe {
            let mut properties = std::mem::MaybeUninit::<sys::cudaDeviceProp>::zeroed();
            if sys::cudaGetDeviceProperties_v2(properties.as_mut_ptr(), ordinal)
                .result()
                .is_err()
            {
                continue;
            }
            properties.assume_init()
        };
        let cuda_uuid: [u8; 16] = properties
            .uuid
            .bytes
            .map(|signed_byte| signed_byte as u8);
        if cuda_uuid == vulkan_device_uuid {
            unsafe {
                sys::cudaSetDevice(ordinal)
                    .result()
                    .map_err(|failure| format!("cudaSetDevice({ordinal}): {failure:?}"))?;
            }
            return Ok(ordinal);
        }
    }
    Err(format!(
        "no CUDA device matches the exporting Vulkan device (UUID {vulkan_device_uuid:02x?}); \
         the GPU that owns this memory is not visible to CUDA"
    ))
}

/// Import an engine-exported OPAQUE_FD into CUDA and map a device
/// pointer over it.
///
/// **Consumes `opaque_fd`**: CUDA takes ownership on success, and every
/// failure path closes it before returning.
pub(crate) fn import_opaque_fd_into_cuda(
    opaque_fd: RawFd,
    byte_size: u64,
    vulkan_device_uuid: [u8; 16],
) -> Result<CudaImportedSurface, String> {
    if let Some(reason) = cuda_unavailable_reason() {
        unsafe { libc::close(opaque_fd) };
        return Err(reason);
    }

    let device_ordinal = match bind_cuda_device_by_uuid(vulkan_device_uuid) {
        Ok(ordinal) => ordinal,
        Err(binding_failure) => {
            unsafe { libc::close(opaque_fd) };
            return Err(binding_failure);
        }
    };

    // SAFETY: on success the CUDA driver adopts `opaque_fd` and it must
    // not be closed here; on failure ownership stays with us.
    let external_memory =
        match unsafe { external_memory::import_external_memory_opaque_fd(opaque_fd, byte_size) } {
            Ok(external_memory) => external_memory,
            Err(import_failure) => {
                unsafe { libc::close(opaque_fd) };
                return Err(format!("cudaImportExternalMemory: {import_failure:?}"));
            }
        };

    // SAFETY: the flat-pointer mapping helper; the returned pointer
    // aliases the memory the OPAQUE_FD `VkBuffer` is bound to and stays
    // valid until `cudaDestroyExternalMemory`.
    let device_pointer = match unsafe {
        external_memory::get_mapped_buffer(external_memory, 0, byte_size)
    } {
        Ok(pointer) => pointer as u64,
        Err(mapping_failure) => {
            let _ = unsafe { external_memory::destroy_external_memory(external_memory) };
            return Err(format!(
                "cudaExternalMemoryGetMappedBuffer: {mapping_failure:?}"
            ));
        }
    };

    let dlpack_device_type =
        match classify_device_pointer(device_pointer) {
            Ok(device_type) => device_type,
            Err(classification_failure) => {
                let _ = unsafe { external_memory::destroy_external_memory(external_memory) };
                return Err(classification_failure);
            }
        };

    Ok(CudaImportedSurface {
        external_memory,
        device_pointer,
        dlpack_device: Device {
            device_type: dlpack_device_type,
            device_id: device_ordinal,
        },
    })
}

/// Ask the driver what kind of memory the imported pointer actually is.
fn classify_device_pointer(device_pointer: u64) -> Result<DeviceType, String> {
    let attributes = unsafe {
        let mut attributes = std::mem::MaybeUninit::<sys::cudaPointerAttributes>::uninit();
        sys::cudaPointerGetAttributes(
            attributes.as_mut_ptr(),
            device_pointer as *const std::ffi::c_void,
        )
        .result()
        .map_err(|failure| format!("cudaPointerGetAttributes: {failure:?}"))?;
        attributes.assume_init()
    };
    match attributes.type_ {
        sys::cudaMemoryType::cudaMemoryTypeDevice => Ok(DeviceType::Cuda),
        sys::cudaMemoryType::cudaMemoryTypeHost => Ok(DeviceType::CudaHost),
        unexpected => Err(format!(
            "the imported pointer is {unexpected:?} — neither device nor pinned-host memory, so \
             no DLPack device type describes it. This is a driver-level surprise, not a usage \
             error; do not paper over it by assuming kDLCUDA."
        )),
    }
}
