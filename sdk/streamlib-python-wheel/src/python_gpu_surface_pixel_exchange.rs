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
use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::{PyBufferError, PyNotImplementedError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd as _;
#[cfg(target_os = "linux")]
use std::sync::LazyLock;
#[cfg(target_os = "linux")]
use streamlib::sdk::context::SurfaceDeviceExportStaging;
use streamlib::sdk::context::{GpuContextLimitedAccess, PooledTextureHandle, SurfaceStore};
use streamlib::sdk::rhi::{PixelBuffer, PixelFormat};

#[cfg(target_os = "linux")]
use crate::python_cuda_pixel_exchange::{CudaImportedSurface, import_opaque_fd_into_cuda};
#[cfg(target_os = "linux")]
use crate::python_helper_process_pixel_exchange::HelperCheckedOutPixelSurface;
use streamlib_adapter_cuda::dlpack::{
    self, DataType, DataTypeCode, Device, DeviceType, Flags, ManagedTensor, ManagedTensorVersioned,
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

/// The engine resource a [`GpuSurfaceOwnedMemory`] keeps alive.
pub(crate) enum GpuSurfaceOwnedValue {
    PixelBuffer(PixelBuffer),
    /// Held to keep the pool slot alive; the device-local export path reads
    /// it once the texture half of the exchange surface lands.
    #[expect(
        dead_code,
        reason = "lifetime-only until the texture export path reads it"
    )]
    PooledTexture(PooledTextureHandle),
    /// A surface checked out of the parent's surface-share service by a
    /// helper process: consumer-imported memory, plus the `release_handle`
    /// debt an acquired surface owes its parent.
    #[cfg(target_os = "linux")]
    HelperCheckedOut(HelperCheckedOutPixelSurface),
}

/// One host-visible plane, described the same way whichever process mapped
/// it — the engine's own allocation or a helper's consumer import. Every
/// CPU-side accessor (`base_address`, `bytes_per_row`, the DLPack capsule,
/// the lock's mapping check) derives from this one answer.
pub(crate) struct HostVisiblePixelPlaneView {
    pub(crate) base_address: *mut u8,
    pub(crate) bytes_per_row: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: PixelFormat,
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
    /// The engine capability the device-export path refills through —
    /// the one view the engine documents as stash-safe. `None` on a
    /// handle minted without one; device export refuses there.
    gpu_limited_access: Option<GpuContextLimitedAccess>,
    /// A texture registration this memory minted at acquire and must
    /// undo on release, so the texture cache doesn't pin pool textures
    /// past their handles.
    texture_registration_to_unregister: Option<String>,
}

impl GpuSurfaceOwnedMemory {
    pub(crate) fn new(
        owned_value: GpuSurfaceOwnedValue,
        surface_store_owing_a_release: Option<SurfaceStore>,
        minted_surface_id: Option<String>,
        gpu_limited_access: Option<GpuContextLimitedAccess>,
    ) -> Arc<Self> {
        Arc::new(Self {
            owned_value,
            surface_store_owing_a_release,
            minted_surface_id,
            gpu_limited_access,
            texture_registration_to_unregister: None,
        })
    }

    /// A pooled texture registered at acquire: the registration is this
    /// memory's debt, undone when the last holder (handle or tensor)
    /// releases.
    pub(crate) fn new_with_texture_registration_debt(
        owned_value: GpuSurfaceOwnedValue,
        registered_surface_id: Option<String>,
        gpu_limited_access: Option<GpuContextLimitedAccess>,
    ) -> Arc<Self> {
        Arc::new(Self {
            owned_value,
            surface_store_owing_a_release: None,
            minted_surface_id: registered_surface_id.clone(),
            gpu_limited_access,
            texture_registration_to_unregister: registered_surface_id,
        })
    }

    /// The pixel buffer a DMA-BUF export can be taken from, or a refusal
    /// naming why this surface cannot answer.
    ///
    /// An allocation carries one external-handle flavour: pool buffers are
    /// DMA-BUF, exchange buffers are OPAQUE_FD, and neither exports the
    /// other's handle type under VMA's per-pool memory configuration.
    pub(crate) fn dma_buf_exportable_pixel_buffer(&self) -> PyResult<&PixelBuffer> {
        match &self.owned_value {
            GpuSurfaceOwnedValue::PixelBuffer(pixel_buffer) => Ok(pixel_buffer),
            GpuSurfaceOwnedValue::PooledTexture(_) => Err(PyNotImplementedError::new_err(
                "exporting a pooled texture's DMA-BUF goes through the texture export path",
            )),
            #[cfg(target_os = "linux")]
            GpuSurfaceOwnedValue::HelperCheckedOut(_) => Err(PyNotImplementedError::new_err(
                "this surface is a consumer import: its DMA-BUF was already exported by the \
                 engine, and re-exporting an import is not a path. Publish the surface id and \
                 let the consumer check it out instead",
            )),
        }
    }

    /// The host-mapped pixel view, or a refusal naming why this surface
    /// has none. The single answer to "can the CPU address these bytes?"
    /// — every host-side accessor routes through it.
    pub(crate) fn host_visible_pixel_plane(&self) -> PyResult<HostVisiblePixelPlaneView> {
        match &self.owned_value {
            GpuSurfaceOwnedValue::PixelBuffer(pixel_buffer) => Ok(HostVisiblePixelPlaneView {
                base_address: pixel_buffer.plane_base_address(0),
                bytes_per_row: pixel_buffer_bytes_per_row(pixel_buffer)?,
                width: pixel_buffer.width,
                height: pixel_buffer.height,
                format: pixel_buffer.format(),
            }),
            GpuSurfaceOwnedValue::PooledTexture(_) => Err(PyNotImplementedError::new_err(
                "this surface is a pooled texture: its pixels are device-local and tiled, so CPU \
                 access goes through the texture export path rather than a host mapping",
            )),
            #[cfg(target_os = "linux")]
            GpuSurfaceOwnedValue::HelperCheckedOut(checked_out) => Ok(HostVisiblePixelPlaneView {
                base_address: checked_out.consumer_buffer.mapped_ptr(),
                bytes_per_row: checked_out.bytes_per_row,
                width: checked_out.width,
                height: checked_out.height,
                format: checked_out.format,
            }),
        }
    }
}

impl Drop for GpuSurfaceOwnedMemory {
    fn drop(&mut self) {
        if let (Some(engine_view), Some(registered_id)) = (
            self.gpu_limited_access.as_ref(),
            self.texture_registration_to_unregister.as_ref(),
        ) {
            // Also evicts the device-export staging engine-side.
            engine_view.unregister_texture(registered_id);
        }
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

/// The DLPack element dtype and trailing channel axis for a single-plane
/// format — the two facts the engine's canonical `bits_per_pixel` /
/// `plane_count` tables do not carry. `None` for formats DLPack cannot
/// express as one buffer (multi-plane; exporting plane 0 alone would
/// hand out luma while silently dropping chroma).
fn single_plane_shape(format: PixelFormat) -> Option<(u32, DataType, Option<i64>)> {
    if format.plane_count() > 1 || format == PixelFormat::Unknown {
        return None;
    }
    let bytes_per_pixel = format.bits_per_pixel() / 8;
    let (dtype, channel_axis) = match format {
        PixelFormat::Bgra32 | PixelFormat::Rgba32 | PixelFormat::Argb32 => (DataType::U8, Some(4)),
        // 16 bits per channel: the element is a u16, so the channel axis
        // is still 4 wide but each element spans two bytes.
        PixelFormat::Rgba64 => (
            DataType {
                code: DataTypeCode::UInt,
                bits: 16,
                lanes: 1,
            },
            Some(4),
        ),
        PixelFormat::Gray8 => (DataType::U8, None),
        // Packed 4:2:2 — two bytes per pixel, but the byte pair is not a
        // per-pixel channel tuple. Exported as raw bytes with the trailing
        // axis naming the pair; interpreting it as colour is the
        // consumer's job.
        PixelFormat::Yuyv422 | PixelFormat::Uyvy422 => (DataType::U8, Some(2)),
        PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange | PixelFormat::Unknown => {
            return None;
        }
    };
    Some((bytes_per_pixel, dtype, channel_axis))
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
        if element_bytes == 0 || !bytes_per_row.is_multiple_of(element_bytes) {
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
    if !plane_size.is_multiple_of(height) {
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
        let managed_tensor = pyo3::ffi::PyCapsule_GetPointer(capsule, DLPACK_CAPSULE_NAME.as_ptr())
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
// SAFETY: same protocol as `dlpack_capsule_destructor` — the pointer is
// dereferenced only after `PyCapsule_IsValid` proves the capsule still
// carries the pre-consumption name, so a consumer-adopted (renamed)
// tensor is never double-freed.
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
    let plane_view = owned_memory.host_visible_pixel_plane()?;

    if plane_view.base_address.is_null() {
        return Err(PyBufferError::new_err(
            "surface has no host mapping; a DEVICE_LOCAL allocation reaches the CPU through the \
             texture export path instead",
        ));
    }

    let layout = PixelExchangeTensorLayout::for_pixel_format(
        plane_view.format,
        plane_view.width,
        plane_view.height,
        plane_view.bytes_per_row,
    )?;
    dlpack_capsule_over(
        python,
        plane_view.base_address as u64,
        layout,
        HOST_VISIBLE_DLPACK_DEVICE,
        exchange_shape,
        read_only,
        Box::new(Arc::clone(owned_memory)),
    )
}

/// Wrap `address` in the negotiated DLPack exchange shape — the one
/// place the shape selection and the read-only flag derivation live, so
/// the host and device exports cannot drift.
fn dlpack_capsule_over<'py>(
    python: Python<'py>,
    address: u64,
    layout: PixelExchangeTensorLayout,
    device: Device,
    exchange_shape: DlpackExchangeShape,
    read_only: bool,
    owner: dlpack::CapsuleOwner,
) -> PyResult<Bound<'py, PyAny>> {
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
                    address,
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
                address,
                layout.shape,
                Some(layout.strides),
                layout.dtype,
                device,
                owner,
            ),
        ),
    }
}

// =============================================================================
// Device export — the same memory, in CUDA's dialect
// =============================================================================

/// One surface's device-export state: the engine staging plus CUDA's
/// import of it.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub(crate) struct SurfaceDeviceExport {
    pub(crate) staging: Arc<SurfaceDeviceExportStaging>,
    pub(crate) cuda_import: Arc<CudaImportedSurface>,
}

/// CUDA imports memoised per surface id, each validated against the
/// engine staging it was made for.
///
/// The engine owns the staging lifecycle (`GpuContext` caches it and
/// drops it with the context); CUDA is the wheel's runtime, so its
/// import lives here — a memo, not a lifecycle record. `Weak` is the
/// validity check: when the engine hands back a different staging (the
/// old one was evicted, or a new context reused the id), the pointer
/// mismatch forces a fresh import and the dead entry is replaced, so the
/// map is bounded by live stagings rather than by process history.
#[cfg(target_os = "linux")]
static CUDA_IMPORTS_BY_SURFACE: LazyLock<
    Mutex<
        HashMap<
            String,
            (
                std::sync::Weak<SurfaceDeviceExportStaging>,
                Arc<CudaImportedSurface>,
            ),
        >,
    >,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The engine capability this owned memory refills through, or the
/// refusal naming how to get one.
fn engine_view_for(
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<&GpuContextLimitedAccess> {
    owned_memory.gpu_limited_access.as_ref().ok_or_else(|| {
        PyRuntimeError::new_err(
            "this handle was minted without the engine capability the device export refills \
             through; acquire or resolve it via ctx.gpu_limited_access",
        )
    })
}

/// This surface's device export: engine staging (engine-cached), CUDA
/// import (memoised here, validated against that staging).
#[cfg(target_os = "linux")]
pub(crate) fn surface_device_export_for(
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<SurfaceDeviceExport> {
    let surface_id = owned_memory.minted_surface_id.as_deref().ok_or_else(|| {
        PyRuntimeError::new_err(
            "this surface carries no id, so there is nothing to key a device export on; device \
             tensors come from graph frames (resolve_surface) or published pixel buffers",
        )
    })?;
    let engine_view = engine_view_for(owned_memory)?;
    let staging = engine_view
        .surface_device_export_staging(surface_id)
        .map_err(|failure| PyRuntimeError::new_err(failure.to_string()))?;

    let mut memo = CUDA_IMPORTS_BY_SURFACE.lock();
    // Entries whose staging died (context torn down, surface
    // unregistered) are unreachable by the ptr-eq check below; sweep
    // them so the memo is bounded by live stagings, not process history.
    memo.retain(|_, (staging_it_was_made_for, _)| staging_it_was_made_for.strong_count() > 0);
    if let Some((staging_it_was_made_for, cuda_import)) = memo.get(surface_id)
        && staging_it_was_made_for
            .upgrade()
            .is_some_and(|previous| Arc::ptr_eq(&previous, &staging))
    {
        return Ok(SurfaceDeviceExport {
            staging,
            cuda_import: Arc::clone(cuda_import),
        });
    }
    let (raw_fd, byte_size, vulkan_device_uuid) = engine_view
        .export_device_staging_opaque_fd(&staging)
        .map_err(|failure| PyRuntimeError::new_err(failure.to_string()))?;
    // SAFETY: the export hands over a fresh dup'd fd nothing else holds.
    let opaque_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw_fd) };
    let cuda_import = import_opaque_fd_into_cuda(opaque_fd, byte_size, vulkan_device_uuid)
        .map(Arc::new)
        .map_err(PyRuntimeError::new_err)?;
    memo.insert(
        surface_id.to_string(),
        (Arc::downgrade(&staging), Arc::clone(&cuda_import)),
    );
    Ok(SurfaceDeviceExport {
        staging,
        cuda_import,
    })
}

/// Everything a device capsule needs, produced with no GIL attached:
/// the refill runs a GPU submit and a bounded wait, and the first call
/// additionally allocates staging and imports into CUDA.
#[cfg(target_os = "linux")]
pub(crate) struct PreparedDeviceExport {
    pub(crate) export: SurfaceDeviceExport,
    pub(crate) layout: PixelExchangeTensorLayout,
    pub(crate) writable: bool,
}

/// Refill the staging from the surface's current pixels and derive the
/// tensor layout — the detachable half of a device export. Call inside
/// `python.detach`.
#[cfg(target_os = "linux")]
pub(crate) fn prepare_device_export(
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<PreparedDeviceExport> {
    let export = surface_device_export_for(owned_memory)?;
    engine_view_for(owned_memory)?
        .refill_device_export_staging(&export.staging)
        .map_err(|failure| PyRuntimeError::new_err(failure.to_string()))?;

    // Geometry comes from the staging alone — the object the byte span
    // was sized for — never mixed with the handle's own pixel view.
    let staging = &export.staging;
    // The engine records the pixel shape for both source kinds (a
    // texture source maps to its 4-byte color shape — BGRA stays BGRA).
    let pixel_format = staging.pixel_format().ok_or_else(|| {
        PyRuntimeError::new_err("this staging carries no pixel shape; nothing to lay out")
    })?;
    let layout = PixelExchangeTensorLayout::for_pixel_format(
        pixel_format,
        staging.surface_width(),
        staging.surface_height(),
        staging.bytes_per_row(),
    )?;
    let writable = staging.writable();
    Ok(PreparedDeviceExport {
        export,
        layout,
        writable,
    })
}

/// Build the capsule over a prepared export — the attached half; touches
/// no engine lock.
#[cfg(target_os = "linux")]
pub(crate) fn device_dlpack_capsule<'py>(
    python: Python<'py>,
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    prepared: PreparedDeviceExport,
    exchange_shape: DlpackExchangeShape,
    read_only_lock: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let read_only = read_only_lock || !prepared.writable;
    let owner: dlpack::CapsuleOwner = Box::new((
        Arc::clone(owned_memory),
        Arc::clone(&prepared.export.staging),
        Arc::clone(&prepared.export.cuda_import),
    ));
    dlpack_capsule_over(
        python,
        prepared.export.cuda_import.device_pointer(),
        prepared.layout,
        prepared.export.cuda_import.dlpack_device(),
        exchange_shape,
        read_only,
        owner,
    )
}

/// Publish a device-side write back into the surface, so every other
/// holder observes the edit. Runs at unlock/close when a writable device
/// tensor was taken under the write lock.
#[cfg(target_os = "linux")]
pub(crate) fn publish_device_write_back_to_surface(
    owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<()> {
    let export = surface_device_export_for(owned_memory)?;
    engine_view_for(owned_memory)?
        .copy_device_export_staging_back_to_surface(&export.staging)
        .map_err(|failure| PyRuntimeError::new_err(failure.to_string()))?;
    Ok(())
}

/// The DLPack device this surface's tensors would live on, importing on
/// first ask — the driver's own classification of the mapped pointer is
/// the only honest answer.
#[cfg(target_os = "linux")]
pub(crate) fn imported_device_for(owned_memory: &Arc<GpuSurfaceOwnedMemory>) -> PyResult<Device> {
    Ok(surface_device_export_for(owned_memory)?
        .cuda_import
        .dlpack_device())
}

/// Whether this surface can even attempt a device export: it has an id
/// to key on and the engine capability to refill through.
#[cfg(target_os = "linux")]
pub(crate) fn device_export_available(owned_memory: &Arc<GpuSurfaceOwnedMemory>) -> bool {
    owned_memory.minted_surface_id.is_some() && owned_memory.gpu_limited_access.is_some()
}

/// Off Linux there is no device export; every surface serves its host side.
#[cfg(not(target_os = "linux"))]
pub(crate) fn device_export_available(_owned_memory: &Arc<GpuSurfaceOwnedMemory>) -> bool {
    false
}

// =============================================================================
// CPU access gate
// =============================================================================

/// Whether the surface is currently locked for CPU access, and for what.
///
/// An access-discipline flag, not a synchronisation point — it performs
/// no wait. Ordering against the producer comes from publication: a
/// built-in source waits on its own timeline before it writes the
/// surface id to its output port (`camera_source.rs` does this at its
/// host-readback wait), so a frame a consumer can name is a frame the
/// GPU has finished writing. The gate's job is narrower: it makes the
/// read/write intent explicit at the call site, and it is what carries
/// `read_only` through to the exported tensor's flags.
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
            "surface is not locked for CPU access: call lock() first, or use the surface as a \
             context manager. lock(read_only=False) is also what marks the exported tensor \
             writable.",
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
        let layout = PixelExchangeTensorLayout::for_pixel_format(PixelFormat::Gray8, 320, 200, 320)
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
            let capsule =
                dlpack_capsule_from_managed_tensor(python, counted_managed_tensor(&drops))
                    .expect("capsule");
            assert_eq!(
                drops.load(Ordering::SeqCst),
                0,
                "still owned by the capsule"
            );
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
            let capsule =
                dlpack_capsule_from_managed_tensor(python, counted_managed_tensor(&drops))
                    .expect("capsule");
            // What every `from_dlpack` implementation does on adoption.
            let adopted = unsafe {
                let raw =
                    pyo3::ffi::PyCapsule_GetPointer(capsule.as_ptr(), DLPACK_CAPSULE_NAME.as_ptr())
                        as *mut ManagedTensor;
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
