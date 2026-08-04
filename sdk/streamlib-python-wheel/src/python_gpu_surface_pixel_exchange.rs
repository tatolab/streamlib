// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The pixel-exchange surface: how Python reaches the pixels behind a
//! `GpuSurfaceHandle` without a CPU round trip.
//!
//! Two contracts live here, and both are load-bearing.
//!
//! **Lifetime.** The memory a DLPack capsule points at outlives the
//! handle that produced it. Python may keep a tensor after the frame is
//! released, so the engine value and the surface-store release debt sit
//! behind an [`GpuSurfaceOwnedMemory`] that every outstanding capsule
//! holds an `Arc` of; the pool slot comes back only when the handle and
//! the last capsule are both gone. Dropping that to a plain `close()`
//! would return the slot while a live tensor still addresses it, and the
//! producer's next frame would overwrite pixels Python is reading.
//!
//! **Layout.** DLPack expresses one strided linear buffer, so the shape
//! and strides are derived from the pixel format here rather than left
//! to the caller, strides are counted in elements (the DLPack spec's
//! unit, not numpy's bytes), and a multi-plane format is refused outright
//! rather than silently exported as its first plane.

use std::ffi::{CStr, c_void};
#[cfg(target_os = "linux")]
use std::os::unix::io::RawFd;
use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::{PyBufferError, PyNotImplementedError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use streamlib::sdk::context::{PooledTextureHandle, SurfaceStore};
use streamlib::sdk::rhi::{PixelBuffer, PixelFormat};

#[cfg(target_os = "linux")]
use crate::python_cuda_pixel_exchange::{CudaImportedSurface, import_opaque_fd_into_cuda};
use streamlib_adapter_cuda::dlpack::{
    self, DataType, DataTypeCode, Device, DeviceType, Flags, ManagedTensor,
    ManagedTensorVersioned,
};

/// Capsule names before a consumer takes ownership. The DLPack protocol
/// fixes these and their `used_` counterparts: a consumer renames the
/// capsule once it has adopted the tensor, and our destructors key off
/// that rename to decide whether the deleter is still ours to run.
const DLPACK_CAPSULE_NAME: &CStr = c"dltensor";
const DLPACK_VERSIONED_CAPSULE_NAME: &CStr = c"dltensor_versioned";

// =============================================================================
// Owned memory — the lifetime anchor shared by the handle and its capsules
// =============================================================================

/// An engine allocation whose memory CUDA can import: an OPAQUE_FD
/// `StorageBuffer`, plus the pixel-shaped view of the same allocation.
///
/// The two are one allocation, not two — `wrap_storage_buffer_as_pixel_buffer`
/// shares the underlying `Arc<HostVulkanBuffer>`. That is what lets a
/// host-visible exchange buffer serve numpy and CUDA from the same bytes.
pub(crate) struct DeviceExchangeBuffer {
    /// The allocation, pixel-shaped. This alone keeps it alive — the wrap
    /// shares the storage buffer's `Arc`, so holding both would be one
    /// refcount doing nothing.
    pub(crate) pixel_buffer: PixelBuffer,
    /// Device-local allocations have no host mapping, so `as_numpy` and
    /// the CPU DLPack path refuse rather than hand back a null pointer.
    pub(crate) device_local: bool,
    /// The OPAQUE_FD, exported at acquire time because exporting needs
    /// the privileged capability and `__dlpack__` does not have one.
    /// Consumed by the first CUDA import — which adopts the fd — and
    /// closed by `Drop` if no export ever happens.
    exported_opaque_fd: Mutex<Option<RawFd>>,
    byte_size: u64,
    /// The exporting device's `VkPhysicalDeviceIDProperties::deviceUUID`,
    /// which is how the CUDA import finds the GPU that owns the memory.
    vulkan_device_uuid: [u8; 16],
}

#[cfg(target_os = "linux")]
impl DeviceExchangeBuffer {
    pub(crate) fn new(
        pixel_buffer: PixelBuffer,
        device_local: bool,
        exported_opaque_fd: RawFd,
        byte_size: u64,
        vulkan_device_uuid: [u8; 16],
    ) -> Self {
        Self {
            pixel_buffer,
            device_local,
            exported_opaque_fd: Mutex::new(Some(exported_opaque_fd)),
            byte_size,
            vulkan_device_uuid,
        }
    }
}

impl Drop for DeviceExchangeBuffer {
    fn drop(&mut self) {
        // Only reached when nothing ever exported to CUDA — a successful
        // import adopts the fd and takes it out of this slot.
        if let Some(unconsumed_fd) = self.exported_opaque_fd.lock().take() {
            unsafe { libc::close(unconsumed_fd) };
        }
    }
}

/// The engine resource a [`GpuSurfaceOwnedMemory`] keeps alive.
pub(crate) enum GpuSurfaceOwnedValue {
    PixelBuffer(PixelBuffer),
    /// Held to keep the pool slot alive; the device-local export path reads
    /// it once the texture half of the exchange surface lands.
    #[expect(dead_code, reason = "lifetime-only until the texture export path reads it")]
    PooledTexture(PooledTextureHandle),
    DeviceExchange(DeviceExchangeBuffer),
}

/// The engine value plus the surface-store release it owes, held behind
/// an `Arc` shared by the Python handle and every DLPack capsule minted
/// from it.
///
/// Release runs in `Drop` rather than on `close()` precisely so a tensor
/// that outlives its frame keeps addressing live memory: the last holder
/// settles the debt, whether that is the handle or a capsule Python is
/// still holding.
pub(crate) struct GpuSurfaceOwnedMemory {
    owned_value: GpuSurfaceOwnedValue,
    /// Present only when the acquire checked the buffer into the store.
    /// The check-in parks a strong clone there, so the pool slot returns
    /// only if release evicts it.
    surface_store_owing_a_release: Option<SurfaceStore>,
    minted_surface_id: Option<String>,
    /// The CUDA import, made on first device export and kept here rather
    /// than on the handle: a capsule outliving its handle must keep the
    /// import alive too, or the device pointer it carries dangles.
    #[cfg(target_os = "linux")]
    cuda_import: Mutex<Option<Arc<CudaImportedSurface>>>,
}

impl GpuSurfaceOwnedMemory {
    pub(crate) fn new(
        owned_value: GpuSurfaceOwnedValue,
        surface_store_owing_a_release: Option<SurfaceStore>,
        minted_surface_id: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            owned_value,
            surface_store_owing_a_release,
            minted_surface_id,
            #[cfg(target_os = "linux")]
            cuda_import: Mutex::new(None),
        })
    }

    /// The host-mapped pixel view, or a refusal naming why this surface
    /// has none. The single answer to "can the CPU address these bytes?"
    /// — every host-side accessor routes through it.
    pub(crate) fn host_mapped_pixel_buffer(&self) -> PyResult<&PixelBuffer> {
        match &self.owned_value {
            GpuSurfaceOwnedValue::PixelBuffer(pixel_buffer) => Ok(pixel_buffer),
            GpuSurfaceOwnedValue::DeviceExchange(exchange) if !exchange.device_local => {
                Ok(&exchange.pixel_buffer)
            }
            GpuSurfaceOwnedValue::DeviceExchange(_) => Err(PyBufferError::new_err(
                "this exchange buffer is device-local: its bytes live in VRAM with no host \
                 mapping. Export it to CUDA with `__dlpack__`, or acquire it with \
                 `device_local=False` to reach it from the CPU as well.",
            )),
            GpuSurfaceOwnedValue::PooledTexture(_) => Err(PyNotImplementedError::new_err(
                "this surface is a pooled texture: its pixels are device-local and tiled, so CPU \
                 access goes through the texture export path rather than a host mapping",
            )),
        }
    }

    /// The pixel shape of this surface, host mapping or not.
    fn pixel_shape(&self) -> Option<&PixelBuffer> {
        match &self.owned_value {
            GpuSurfaceOwnedValue::PixelBuffer(pixel_buffer) => Some(pixel_buffer),
            GpuSurfaceOwnedValue::DeviceExchange(exchange) => Some(&exchange.pixel_buffer),
            GpuSurfaceOwnedValue::PooledTexture(_) => None,
        }
    }

    fn device_exchange(&self) -> Option<&DeviceExchangeBuffer> {
        match &self.owned_value {
            GpuSurfaceOwnedValue::DeviceExchange(exchange) => Some(exchange),
            GpuSurfaceOwnedValue::PixelBuffer(_) | GpuSurfaceOwnedValue::PooledTexture(_) => None,
        }
    }
}

impl Drop for GpuSurfaceOwnedMemory {
    fn drop(&mut self) {
        if let (Some(store), Some(surface_id)) = (
            self.surface_store_owing_a_release.as_ref(),
            self.minted_surface_id.as_ref(),
        ) {
            // Best-effort, mirroring the subprocess release path: the
            // store eviction is what frees the pool slot, and the daemon
            // half already treats a dropped connection as a full release.
            if let Err(release_failure) = store.release(surface_id) {
                tracing::debug!(%surface_id, %release_failure, "surface-store release failed");
            }
        }
    }
}

// =============================================================================
// Tensor layout — one strided linear buffer, derived from the pixel format
// =============================================================================

/// The DLPack shape a pixel format maps to.
#[derive(Debug)]
pub(crate) struct PixelExchangeTensorLayout {
    pub(crate) shape: Vec<i64>,
    /// Strides in **elements**, per the DLPack spec — not numpy's bytes.
    pub(crate) strides: Vec<i64>,
    pub(crate) dtype: DataType,
}

/// Bytes per pixel and the trailing channel axis for a single-plane
/// format. `None` for formats DLPack cannot express as one buffer.
fn single_plane_shape(format: PixelFormat) -> Option<(u32, DataType, Option<i64>)> {
    match format {
        PixelFormat::Bgra32 | PixelFormat::Rgba32 | PixelFormat::Argb32 => {
            Some((4, DataType::U8, Some(4)))
        }
        // 16 bits per channel: the element is a u16, so the channel axis
        // is still 4 wide but each element spans two bytes.
        PixelFormat::Rgba64 => Some((
            8,
            DataType {
                code: DataTypeCode::UInt,
                bits: 16,
                lanes: 1,
            },
            Some(4),
        )),
        PixelFormat::Gray8 => Some((1, DataType::U8, None)),
        // Packed 4:2:2 — two bytes per pixel, but the byte pair is not a
        // per-pixel channel tuple. Exported as raw bytes with the trailing
        // axis naming the pair, which is what a shader or a converter
        // wants; interpreting it as colour is the consumer's job.
        PixelFormat::Yuyv422 | PixelFormat::Uyvy422 => Some((2, DataType::U8, Some(2))),
        // Multi-plane. DLPack expresses one strided linear buffer, so
        // exporting plane 0 alone would hand out luma while silently
        // dropping chroma.
        PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange => None,
        PixelFormat::Unknown => None,
    }
}

impl PixelExchangeTensorLayout {
    /// Derive the layout for a single-plane surface.
    ///
    /// `bytes_per_row` comes from the allocation rather than from
    /// `width * bytes_per_pixel` so a padded row pitch stays correct.
    pub(crate) fn for_pixel_format(
        format: PixelFormat,
        width: u32,
        height: u32,
        bytes_per_row: u64,
    ) -> PyResult<Self> {
        let Some((bytes_per_pixel, dtype, channel_axis)) = single_plane_shape(format) else {
            return Err(PyValueError::new_err(format!(
                "pixel format {:?} has no single-buffer DLPack shape: DLPack expresses one \
                 strided linear buffer, and exporting only its first plane would hand out \
                 part of the image. Convert to a packed format first.",
                format
            )));
        };

        let element_bytes = u64::from(dtype.bits / 8);
        if element_bytes == 0 || bytes_per_row % element_bytes != 0 {
            return Err(PyValueError::new_err(format!(
                "row pitch {bytes_per_row} is not a whole number of {element_bytes}-byte elements"
            )));
        }
        let row_stride_elements = (bytes_per_row / element_bytes) as i64;
        let channels_per_pixel = i64::from(bytes_per_pixel) / element_bytes as i64;

        let (shape, strides) = match channel_axis {
            Some(channels) => (
                vec![i64::from(height), i64::from(width), channels],
                vec![row_stride_elements, channels_per_pixel, 1],
            ),
            None => (
                vec![i64::from(height), i64::from(width)],
                vec![row_stride_elements, channels_per_pixel],
            ),
        };
        Ok(Self {
            shape,
            strides,
            dtype,
        })
    }
}

/// Row pitch of a pixel buffer's first plane, derived from the
/// allocation so padding is honoured rather than assumed away.
pub(crate) fn pixel_buffer_bytes_per_row(pixel_buffer: &PixelBuffer) -> PyResult<u64> {
    let height = u64::from(pixel_buffer.height);
    if height == 0 {
        return Err(PyValueError::new_err(
            "surface has zero height; no row pitch exists",
        ));
    }
    let plane_size = pixel_buffer.plane_size(0);
    if plane_size == 0 {
        return Err(PyRuntimeError::new_err(
            "surface reports a zero-byte plane; it is not CPU-mappable",
        ));
    }
    if plane_size % height != 0 {
        return Err(PyRuntimeError::new_err(format!(
            "plane size {plane_size} is not a whole number of {height} rows"
        )));
    }
    Ok(plane_size / height)
}

// =============================================================================
// DLPack capsule
// =============================================================================

/// Run the managed tensor's deleter when a capsule is garbage-collected
/// without a consumer having taken it.
///
/// The DLPack protocol makes the rename to `used_dltensor` the transfer
/// of ownership: a renamed capsule's tensor belongs to its consumer, and
/// running the deleter here as well would free it twice.
unsafe extern "C" fn dlpack_capsule_destructor(capsule: *mut pyo3::ffi::PyObject) {
    unsafe {
        if pyo3::ffi::PyCapsule_IsValid(capsule, DLPACK_CAPSULE_NAME.as_ptr()) == 0 {
            // Either consumed (renamed) or not ours. `PyCapsule_IsValid`
            // does not set an exception, so there is nothing to clear.
            return;
        }
        let managed_tensor =
            pyo3::ffi::PyCapsule_GetPointer(capsule, DLPACK_CAPSULE_NAME.as_ptr())
                as *mut ManagedTensor;
        if managed_tensor.is_null() {
            // Defensive: `IsValid` already proved the name matches, so a
            // null pointer here would be a capsule built by someone else.
            pyo3::ffi::PyErr_Clear();
            return;
        }
        if let Some(deleter) = (*managed_tensor).deleter {
            deleter(managed_tensor);
        }
    }
}

/// Wrap a managed tensor in the PyCapsule `from_dlpack` consumers expect.
///
/// Takes ownership of `managed_tensor`: on success the capsule's
/// destructor (or its consumer, after the rename) runs the deleter; on
/// failure the deleter runs here so the tensor is never leaked.
fn dlpack_capsule_from_managed_tensor<'py>(
    python: Python<'py>,
    managed_tensor: *mut ManagedTensor,
) -> PyResult<Bound<'py, PyAny>> {
    let capsule = unsafe {
        pyo3::ffi::PyCapsule_New(
            managed_tensor as *mut c_void,
            DLPACK_CAPSULE_NAME.as_ptr(),
            Some(dlpack_capsule_destructor),
        )
    };
    if capsule.is_null() {
        unsafe {
            if let Some(deleter) = (*managed_tensor).deleter {
                deleter(managed_tensor);
            }
        }
        return Err(PyErr::fetch(python));
    }
    // SAFETY: `PyCapsule_New` returns a new strong reference, which
    // `from_owned_ptr` adopts.
    Ok(unsafe { Bound::from_owned_ptr(python, capsule) })
}

/// The DLPack device a host-visible surface reports.
///
/// `kDLCPU`: the pixel buffer is a persistently-mapped host-visible
/// allocation, so a consumer addresses it with ordinary loads — no CUDA
/// context is involved and claiming `kDLCUDA` would make torch try to
/// treat a host pointer as device memory.
pub(crate) const HOST_VISIBLE_DLPACK_DEVICE: Device = Device {
    device_type: DeviceType::Cpu,
    device_id: 0,
};

/// Run the versioned managed tensor's deleter when a capsule is
/// collected without a consumer having taken it. Same rename contract as
/// the unversioned destructor, against `dltensor_versioned`.
unsafe extern "C" fn dlpack_versioned_capsule_destructor(capsule: *mut pyo3::ffi::PyObject) {
    unsafe {
        if pyo3::ffi::PyCapsule_IsValid(capsule, DLPACK_VERSIONED_CAPSULE_NAME.as_ptr()) == 0 {
            return;
        }
        let managed_tensor =
            pyo3::ffi::PyCapsule_GetPointer(capsule, DLPACK_VERSIONED_CAPSULE_NAME.as_ptr())
                as *mut ManagedTensorVersioned;
        if managed_tensor.is_null() {
            pyo3::ffi::PyErr_Clear();
            return;
        }
        if let Some(deleter) = (*managed_tensor).deleter {
            deleter(managed_tensor);
        }
    }
}

/// Wrap a versioned managed tensor in its PyCapsule, taking ownership on
/// the same terms as [`dlpack_capsule_from_managed_tensor`].
fn dlpack_versioned_capsule_from_managed_tensor<'py>(
    python: Python<'py>,
    managed_tensor: *mut ManagedTensorVersioned,
) -> PyResult<Bound<'py, PyAny>> {
    let capsule = unsafe {
        pyo3::ffi::PyCapsule_New(
            managed_tensor as *mut c_void,
            DLPACK_VERSIONED_CAPSULE_NAME.as_ptr(),
            Some(dlpack_versioned_capsule_destructor),
        )
    };
    if capsule.is_null() {
        unsafe {
            if let Some(deleter) = (*managed_tensor).deleter {
                deleter(managed_tensor);
            }
        }
        return Err(PyErr::fetch(python));
    }
    // SAFETY: `PyCapsule_New` returns a new strong reference.
    Ok(unsafe { Bound::from_owned_ptr(python, capsule) })
}

/// The exchange shape a consumer negotiated via `__dlpack__`'s
/// `max_version`.
///
/// The distinction is not cosmetic: only the versioned struct carries
/// flags, and an unversioned tensor has no way to say "this memory is
/// writable" — so `numpy.from_dlpack` marks it read-only, and a
/// processor that mutates frames in place gets nothing it can write to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DlpackExchangeShape {
    Unversioned,
    Versioned,
}

/// Pick the exchange shape from the consumer's `max_version`.
///
/// Absent means the consumer predates v1.0 and only understands the
/// unversioned struct. A major version below 1 means the same.
pub(crate) fn exchange_shape_for_max_version(
    max_version: Option<(u32, u32)>,
) -> DlpackExchangeShape {
    match max_version {
        Some((major, _)) if major >= 1 => DlpackExchangeShape::Versioned,
        _ => DlpackExchangeShape::Unversioned,
    }
}

/// Build a DLPack capsule over a host-visible pixel buffer.
///
/// `owned_memory` is cloned into the capsule's owner so the mapping stays
/// addressable until the consumer releases the tensor, even if the Python
/// handle was closed first. `read_only` reaches the consumer only on the
/// versioned path — the unversioned struct has nowhere to carry it.
pub(crate) fn host_visible_dlpack_capsule<'py>(
    python: Python<'py>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    exchange_shape: DlpackExchangeShape,
    read_only: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let pixel_buffer = owned_memory.host_mapped_pixel_buffer()?;

    let base_address = pixel_buffer.plane_base_address(0);
    if base_address.is_null() {
        return Err(PyBufferError::new_err(
            "surface has no host mapping; a DEVICE_LOCAL allocation reaches the CPU through the \
             texture export path instead",
        ));
    }

    let bytes_per_row = pixel_buffer_bytes_per_row(pixel_buffer)?;
    let layout = PixelExchangeTensorLayout::for_pixel_format(
        pixel_buffer.format(),
        pixel_buffer.width,
        pixel_buffer.height,
        bytes_per_row,
    )?;
    let owner = Box::new(Arc::clone(owned_memory));

    match exchange_shape {
        DlpackExchangeShape::Versioned => {
            let flags = if read_only {
                Flags::READ_ONLY
            } else {
                Flags::empty()
            };
            dlpack_versioned_capsule_from_managed_tensor(
                python,
                dlpack::build_managed_tensor_versioned(
                    base_address as u64,
                    layout.shape,
                    Some(layout.strides),
                    layout.dtype,
                    HOST_VISIBLE_DLPACK_DEVICE,
                    flags,
                    owner,
                ),
            )
        }
        DlpackExchangeShape::Unversioned => dlpack_capsule_from_managed_tensor(
            python,
            dlpack::build_managed_tensor(
                base_address as u64,
                layout.shape,
                Some(layout.strides),
                layout.dtype,
                HOST_VISIBLE_DLPACK_DEVICE,
                owner,
            ),
        ),
    }
}

// =============================================================================
// Device export — the same memory, in CUDA's dialect
// =============================================================================

/// Import this surface's memory into CUDA, or return the cached import.
///
/// The import is cached on the shared owned-memory anchor rather than
/// redone per export, both because `cudaImportExternalMemory` is
/// expensive and because every tensor must name the *same* device
/// pointer — a second import would produce a second mapping of the same
/// bytes, and freeing one would strand the other.
#[cfg(target_os = "linux")]
fn cuda_import_for(
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<Arc<CudaImportedSurface>> {
    let mut cached = owned_memory.cuda_import.lock();
    if let Some(existing) = cached.as_ref() {
        return Ok(Arc::clone(existing));
    }
    let exchange = owned_memory.device_exchange().ok_or_else(|| {
        PyRuntimeError::new_err(
            "this surface was not allocated for device exchange: acquire it with \
             `acquire_device_exchange_buffer` so its memory is OPAQUE_FD-exportable. A pooled \
             pixel buffer is DMA-BUF-flavoured and cannot export the handle CUDA imports.",
        )
    })?;
    let opaque_fd = exchange.exported_opaque_fd.lock().take().ok_or_else(|| {
        PyRuntimeError::new_err(
            "this surface's export handle was already consumed and the import did not survive; \
             acquire the buffer again",
        )
    })?;
    let imported =
        import_opaque_fd_into_cuda(opaque_fd, exchange.byte_size, exchange.vulkan_device_uuid)
            .map(Arc::new)
            .map_err(PyRuntimeError::new_err)?;
    *cached = Some(Arc::clone(&imported));
    Ok(imported)
}

/// Build a DLPack capsule over this surface's CUDA device pointer.
///
/// The capsule owner holds both the engine allocation and the CUDA
/// import, so a tensor outliving its handle keeps a live mapping — the
/// same lifetime contract as the host path, extended across the second
/// allocator.
#[cfg(target_os = "linux")]
pub(crate) fn device_dlpack_capsule<'py>(
    python: Python<'py>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    exchange_shape: DlpackExchangeShape,
    read_only: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let imported = cuda_import_for(owned_memory)?;
    let pixel_shape = owned_memory
        .pixel_shape()
        .ok_or_else(|| PyRuntimeError::new_err("this surface has no pixel shape to export"))?;
    // A device-exchange allocation is tightly packed by construction —
    // it is sized `width * height * bytes_per_pixel` at acquire, with no
    // driver row padding to discover.
    let bytes_per_row = pixel_buffer_bytes_per_row(pixel_shape)?;
    let layout = PixelExchangeTensorLayout::for_pixel_format(
        pixel_shape.format(),
        pixel_shape.width,
        pixel_shape.height,
        bytes_per_row,
    )?;

    let device_pointer = imported.device_pointer();
    let device = imported.dlpack_device();
    // Two owners in one box: the engine's allocation and CUDA's mapping
    // of it. Dropping either early dangles the other.
    let owner: Box<dyn std::any::Any + Send> =
        Box::new((Arc::clone(owned_memory), Arc::clone(&imported)));

    match exchange_shape {
        DlpackExchangeShape::Versioned => {
            let flags = if read_only {
                Flags::READ_ONLY
            } else {
                Flags::empty()
            };
            dlpack_versioned_capsule_from_managed_tensor(
                python,
                dlpack::build_managed_tensor_versioned(
                    device_pointer,
                    layout.shape,
                    Some(layout.strides),
                    layout.dtype,
                    device,
                    flags,
                    owner,
                ),
            )
        }
        DlpackExchangeShape::Unversioned => dlpack_capsule_from_managed_tensor(
            python,
            dlpack::build_managed_tensor(
                device_pointer,
                layout.shape,
                Some(layout.strides),
                layout.dtype,
                device,
                owner,
            ),
        ),
    }
}

/// The DLPack device this surface's memory is imported onto, importing
/// it if that has not happened yet.
///
/// Answering without the import would mean guessing whether the driver
/// produced device or pinned-host memory — and a wrong guess here
/// contradicts the capsule the very next call hands back.
#[cfg(target_os = "linux")]
pub(crate) fn imported_device_for(
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<Device> {
    Ok(cuda_import_for(owned_memory)?.dlpack_device())
}

/// Whether this surface was allocated for device exchange.
pub(crate) fn is_device_exchange(owned_memory: &Arc<GpuSurfaceOwnedMemory>) -> bool {
    owned_memory.device_exchange().is_some()
}

// =============================================================================
// CPU access gate
// =============================================================================

/// Whether the surface is currently locked for CPU access, and for what.
///
/// The gate is what makes "read before the producer signalled" a refusal
/// rather than a half-drawn frame: `lock` is where the wait for the
/// producer happens, so an export taken without one has no ordering
/// guarantee behind it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CpuAccessLock {
    Unlocked,
    ReadOnly,
    ReadWrite,
}

pub(crate) struct CpuAccessGate {
    state: Mutex<CpuAccessLock>,
}

impl CpuAccessGate {
    pub(crate) fn new_unlocked() -> Self {
        Self {
            state: Mutex::new(CpuAccessLock::Unlocked),
        }
    }

    pub(crate) fn lock_for(&self, read_only: bool) {
        *self.state.lock() = if read_only {
            CpuAccessLock::ReadOnly
        } else {
            CpuAccessLock::ReadWrite
        };
    }

    pub(crate) fn unlock(&self) {
        *self.state.lock() = CpuAccessLock::Unlocked;
    }

    pub(crate) fn is_locked(&self) -> bool {
        *self.state.lock() != CpuAccessLock::Unlocked
    }

    /// Whether the current lock forbids writing. An unlocked surface
    /// reports read-only; the export path refuses it before the answer
    /// matters.
    pub(crate) fn is_read_only(&self) -> bool {
        *self.state.lock() != CpuAccessLock::ReadWrite
    }

    /// Refuse an export taken outside a lock.
    pub(crate) fn require_locked(&self) -> PyResult<()> {
        if self.is_locked() {
            return Ok(());
        }
        Err(PyRuntimeError::new_err(
            "surface is not locked for CPU access: call lock() first (or use the surface as a \
             context manager). The lock is where the wait for the producer happens, so reading \
             without one can observe a half-written frame.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_layout_is_height_width_four_bytes() {
        let layout =
            PixelExchangeTensorLayout::for_pixel_format(PixelFormat::Bgra32, 640, 480, 640 * 4)
                .expect("bgra has a single-buffer shape");
        assert_eq!(layout.shape, vec![480, 640, 4]);
        // Element size is one byte, so element strides equal byte strides
        // — the same (bytes_per_row, 4, 1) the pre-wheel surface returned.
        assert_eq!(layout.strides, vec![2560, 4, 1]);
        assert_eq!(layout.dtype, DataType::U8);
    }

    #[test]
    fn a_padded_row_pitch_survives_into_the_stride() {
        // 640 pixels of BGRA is 2560 bytes; a driver that pads to 2688
        // must not be flattened into a tightly-packed view.
        let layout =
            PixelExchangeTensorLayout::for_pixel_format(PixelFormat::Bgra32, 640, 480, 2688)
                .expect("bgra has a single-buffer shape");
        assert_eq!(layout.shape, vec![480, 640, 4]);
        assert_eq!(layout.strides[0], 2688);
    }

    #[test]
    fn sixteen_bit_strides_are_counted_in_elements_not_bytes() {
        // DLPack strides are element counts. RGBA64 is 8 bytes per pixel
        // over 2-byte elements, so a 640-pixel row of 5120 bytes is 2560
        // elements and the channel step is 1 element, not 2 bytes.
        let layout =
            PixelExchangeTensorLayout::for_pixel_format(PixelFormat::Rgba64, 640, 480, 640 * 8)
                .expect("rgba64 has a single-buffer shape");
        assert_eq!(layout.shape, vec![480, 640, 4]);
        assert_eq!(layout.strides, vec![2560, 4, 1]);
        assert_eq!(layout.dtype.bits, 16);
    }

    #[test]
    fn gray8_has_no_channel_axis() {
        let layout =
            PixelExchangeTensorLayout::for_pixel_format(PixelFormat::Gray8, 320, 200, 320)
                .expect("gray8 has a single-buffer shape");
        assert_eq!(layout.shape, vec![200, 320]);
        assert_eq!(layout.strides, vec![320, 1]);
    }

    /// NV12 is two planes; a one-buffer export would be luma only.
    #[test]
    fn multi_plane_formats_are_refused_rather_than_truncated() {
        Python::initialize();
        for format in [PixelFormat::Nv12VideoRange, PixelFormat::Nv12FullRange] {
            let refusal = PixelExchangeTensorLayout::for_pixel_format(format, 640, 480, 640)
                .expect_err("nv12 must not export as one buffer");
            assert!(
                refusal.to_string().contains("one strided linear buffer"),
                "the refusal should say why, got: {refusal}"
            );
        }
    }

    #[test]
    fn the_access_gate_refuses_an_export_taken_without_a_lock() {
        Python::initialize();
        let gate = CpuAccessGate::new_unlocked();
        let refusal = gate.require_locked().expect_err("unlocked must refuse");
        assert!(refusal.to_string().contains("not locked"));

        gate.lock_for(true);
        assert!(gate.require_locked().is_ok());

        gate.unlock();
        assert!(
            gate.require_locked().is_err(),
            "unlock closes the gate again"
        );
    }

    // -------------------------------------------------------------------
    // Capsule ownership. The DLPack protocol splits responsibility for the
    // deleter between producer and consumer at the rename, and getting that
    // split wrong is either a leak or a double free — neither of which a
    // pixel-shaped test would surface.
    // -------------------------------------------------------------------

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The name a consumer renames the capsule to once it has adopted the
    /// tensor — the other half of the protocol pair.
    const DLPACK_CONSUMED_CAPSULE_NAME: &CStr = c"used_dltensor";

    /// A managed tensor over a fake pointer whose owner counts its drops.
    fn counted_managed_tensor(drops: &Arc<AtomicUsize>) -> *mut ManagedTensor {
        struct CountedOwner(Arc<AtomicUsize>);
        impl Drop for CountedOwner {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        dlpack::build_managed_tensor(
            0xCAFE_F00D,
            vec![4, 4, 4],
            Some(vec![16, 4, 1]),
            DataType::U8,
            HOST_VISIBLE_DLPACK_DEVICE,
            Box::new(CountedOwner(Arc::clone(drops))),
        )
    }

    /// A capsule nobody consumed still owns its tensor, so collection must
    /// free it — exactly once.
    #[test]
    fn an_unconsumed_capsule_frees_its_tensor_on_collection() {
        Python::initialize();
        let drops = Arc::new(AtomicUsize::new(0));
        Python::attach(|python| {
            let capsule = dlpack_capsule_from_managed_tensor(
                python,
                counted_managed_tensor(&drops),
            )
            .expect("capsule");
            assert_eq!(drops.load(Ordering::SeqCst), 0, "still owned by the capsule");
            drop(capsule);
        });
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "collection must run the deleter exactly once"
        );
    }

    /// Renaming to `used_dltensor` is how a consumer signals it has adopted
    /// the tensor. Freeing it here as well would be a double free of memory
    /// torch is still using.
    #[test]
    fn a_consumed_capsule_is_left_alone_by_the_destructor() {
        Python::initialize();
        let drops = Arc::new(AtomicUsize::new(0));
        let adopted = Python::attach(|python| {
            let capsule = dlpack_capsule_from_managed_tensor(
                python,
                counted_managed_tensor(&drops),
            )
            .expect("capsule");
            // What every `from_dlpack` implementation does on adoption.
            let adopted = unsafe {
                let raw = pyo3::ffi::PyCapsule_GetPointer(
                    capsule.as_ptr(),
                    DLPACK_CAPSULE_NAME.as_ptr(),
                ) as *mut ManagedTensor;
                assert_eq!(
                    pyo3::ffi::PyCapsule_SetName(
                        capsule.as_ptr(),
                        DLPACK_CONSUMED_CAPSULE_NAME.as_ptr(),
                    ),
                    0,
                );
                raw
            };
            drop(capsule);
            adopted
        });
        assert_eq!(
            drops.load(Ordering::SeqCst),
            0,
            "a consumed capsule's tensor belongs to its consumer"
        );
        // The consumer's own release.
        unsafe {
            let deleter = (*adopted).deleter.expect("deleter");
            deleter(adopted);
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
