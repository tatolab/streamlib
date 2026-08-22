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

use std::ffi::c_void;
use std::os::fd::{AsRawFd as _, IntoRawFd as _, OwnedFd};

use cudarc::runtime::result as cuda_result;
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
        // Order matters and is not interchangeable: CUDA requires every
        // buffer mapped onto an external-memory object to be freed
        // *before* the object is destroyed. Freeing after — or not at
        // all — leaks the mapping and violates the destroy precondition.
        //
        // The owning device must be current first: both cleanup calls act
        // on the calling thread's device, and Drop can run on any thread —
        // a Python GC of a capsule, the memo sweep — that never bound this
        // GPU.
        //
        // SAFETY: `device_pointer` came from `get_mapped_buffer` on
        // `external_memory` and is freed exactly once; `external_memory`
        // came from a successful `cudaImportExternalMemory`, is destroyed
        // exactly once, and by this line has no mapped buffers left.
        unsafe {
            if let Err(bind_failure) = sys::cudaSetDevice(self.dlpack_device.device_id).result() {
                tracing::debug!(
                    ?bind_failure,
                    device_id = self.dlpack_device.device_id,
                    "cudaSetDevice before external-memory cleanup failed"
                );
            }
            if let Err(free_failure) = cuda_result::memory_free(self.device_pointer as *mut c_void)
            {
                tracing::debug!(?free_failure, "cudaFree of the mapped buffer failed");
            }
            if let Err(destroy_failure) =
                external_memory::destroy_external_memory(self.external_memory)
            {
                tracing::debug!(?destroy_failure, "cudaDestroyExternalMemory failed");
            }
        }
    }
}

/// Wait until every stream on the import's device has retired its work.
///
/// The write-back publish reads the staging with a Vulkan copy, while a
/// consumer's writes ride whatever CUDA stream it chose — torch's own,
/// typically — and no fence connects the two APIs. The publish calls
/// this first, so the copy reads finished pixels rather than a torn
/// frame; without it every consumer would owe a `torch.cuda.synchronize()`
/// the API never asked for. Both calls target the primary context the
/// import (and torch) live in, and the thread's prior current device is
/// restored — this runs on the thread user Python executes on, and
/// leaving it repointed would surprise a multi-GPU consumer.
pub(crate) fn synchronize_every_stream_on_the_import_device(
    import: &CudaImportedSurface,
) -> Result<(), String> {
    let device_ordinal = import.dlpack_device.device_id;
    unsafe {
        let mut device_before_the_sync: i32 = 0;
        sys::cudaGetDevice(&mut device_before_the_sync)
            .result()
            .map_err(|failure| format!("cudaGetDevice: {failure:?}"))?;
        sys::cudaSetDevice(device_ordinal)
            .result()
            .map_err(|failure| format!("cudaSetDevice({device_ordinal}): {failure:?}"))?;
        let synchronized = sys::cudaDeviceSynchronize()
            .result()
            .map_err(|failure| format!("cudaDeviceSynchronize: {failure:?}"));
        if device_before_the_sync != device_ordinal {
            sys::cudaSetDevice(device_before_the_sync)
                .result()
                .map_err(|failure| {
                    format!("cudaSetDevice({device_before_the_sync}) to restore: {failure:?}")
                })?;
        }
        synchronized?;
    }
    Ok(())
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
        let cuda_uuid: [u8; 16] = properties.uuid.bytes.map(|signed_byte| signed_byte as u8);
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
/// Takes `opaque_fd` by value: every early return drops it closed, and
/// only the successful `cudaImportExternalMemory` — which adopts the fd —
/// releases it with `into_raw_fd`. Hand-closing on each failure path
/// would put the fd's lifetime back in the reader's head, and a later
/// early return would silently leak it.
pub(crate) fn import_opaque_fd_into_cuda(
    opaque_fd: OwnedFd,
    byte_size: u64,
    vulkan_device_uuid: [u8; 16],
) -> Result<CudaImportedSurface, String> {
    if let Some(reason) = cuda_unavailable_reason() {
        return Err(reason);
    }
    let device_ordinal = bind_cuda_device_by_uuid(vulkan_device_uuid)?;

    // SAFETY: the CUDA driver adopts the fd on success, so ownership is
    // released with `into_raw_fd` only for the call that may take it. On
    // failure the raw fd is re-adopted below so it is still closed once.
    let raw_fd = opaque_fd.as_raw_fd();
    let external_memory =
        match unsafe { external_memory::import_external_memory_opaque_fd(raw_fd, byte_size) } {
            Ok(external_memory) => {
                let _adopted_by_cuda = opaque_fd.into_raw_fd();
                external_memory
            }
            Err(import_failure) => {
                // `opaque_fd` still owns it; dropping here closes it.
                return Err(format!("cudaImportExternalMemory: {import_failure:?}"));
            }
        };

    // SAFETY: the flat-pointer mapping helper; the returned pointer
    // aliases the memory the OPAQUE_FD `VkBuffer` is bound to and stays
    // valid until `cudaDestroyExternalMemory`.
    let device_pointer =
        match unsafe { external_memory::get_mapped_buffer(external_memory, 0, byte_size) } {
            Ok(pointer) => pointer as u64,
            Err(mapping_failure) => {
                let _ = unsafe { external_memory::destroy_external_memory(external_memory) };
                return Err(format!(
                    "cudaExternalMemoryGetMappedBuffer: {mapping_failure:?}"
                ));
            }
        };

    let dlpack_device_type = match classify_device_pointer(device_pointer) {
        Ok(device_type) => device_type,
        Err(classification_failure) => {
            // The mapping exists by this point, so it has to go before the
            // object it was mapped onto — same order as `Drop`.
            unsafe {
                let _ = cuda_result::memory_free(device_pointer as *mut c_void);
                let _ = external_memory::destroy_external_memory(external_memory);
            }
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
    // SAFETY: sound only because the driver fully writes the struct when
    // the call succeeds — `assume_init` runs strictly after `.result()?`.
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
