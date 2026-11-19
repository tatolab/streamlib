// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::rhi::PixelFormat;
use streamlib_adapter_cuda::dlpack::{
    self, Device, DeviceType, Flags, ManagedTensor, ManagedTensorVersioned,
};

mod gpu_surface_owned_memory;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod pending_staged_write_back_to_surface;
#[cfg(all(test, target_os = "linux"))]
mod pending_staged_write_back_to_surface_tests;
mod pixel_exchange_tensor_layout;
#[cfg(target_os = "linux")]
mod pool_slots_holding_a_staged_edit;
#[cfg(all(test, target_os = "linux"))]
mod pool_slots_holding_a_staged_edit_tests;
#[cfg(test)]
mod tests;

pub(crate) use gpu_surface_owned_memory::GpuSurfaceOwnedMemory;
#[cfg(target_os = "linux")]
pub(crate) use linux::{
    PreparedDeviceExport, device_dlpack_capsule, device_export_available, imported_device_for,
    map_the_cpu_staging_without_reading_a_frame_in, prepare_device_export,
    publish_tensor_storage_buffer_device_writes, read_the_frame_into_its_cpu_staging,
    tensor_storage_buffer_dlpack_capsule,
};
#[cfg(target_os = "macos")]
pub(crate) use macos::{
    METAL_DLPACK_DEVICE, metal_dlpack_capsule, publish_tensor_storage_buffer_device_writes,
    tensor_storage_buffer_dlpack_capsule,
};
#[cfg(target_os = "linux")]
pub(crate) use pending_staged_write_back_to_surface::StagedWriteBackSource;
pub(crate) use pixel_exchange_tensor_layout::PixelExchangeTensorLayout;
#[cfg(target_os = "linux")]
pub(crate) use pool_slots_holding_a_staged_edit::PoolSlotsHoldingAStagedEdit;

/// Capsule names before a consumer takes ownership. The DLPack protocol
/// fixes these and their `used_` counterparts: a consumer renames the
/// capsule once it has adopted the tensor, and our destructors key off
/// that rename to decide whether the deleter is still ours to run.
const DLPACK_CAPSULE_NAME: &CStr = c"dltensor";
const DLPACK_VERSIONED_CAPSULE_NAME: &CStr = c"dltensor_versioned";

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

/// `device` as the `(device_type, device_id)` pair `__dlpack_device__`
/// answers.
pub(crate) fn dlpack_device_as_python_pair(device: Device) -> (i32, i32) {
    (device.device_type as i32, device.device_id)
}

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
    // Non-null by construction on both arms: the coherent mapping answers
    // only when it has one, and the staged arm refuses at import without
    // a mapping to hand out.
    let plane_view = owned_memory.host_visible_pixel_plane()?;

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

/// A capsule owner that is dropped holding the interpreter's thread state.
///
/// A consumer runs the DLPack deleter wherever it frees the tensor: torch
/// can free one inside its own `gil_scoped_release`, on a thread pyo3 still
/// counts as attached, so a bare `Python::attach` in the owner's `Drop`
/// would run Python (or `detach`) with no thread state and abort the
/// interpreter. `PyGILState_Ensure` asks CPython itself, so it reacquires
/// a released thread state, nests under a held one, and creates one on a
/// foreign thread.
struct CapsuleOwnerDroppedWithTheInterpreterAttached(Option<dlpack::CapsuleOwner>);

impl Drop for CapsuleOwnerDroppedWithTheInterpreterAttached {
    fn drop(&mut self) {
        let owner = self.0.take();
        // SAFETY: `Py_IsInitialized` may be called at any time; the ensure is
        // paired with exactly one release on this thread.
        unsafe {
            if pyo3::ffi::Py_IsInitialized() == 0 {
                drop(owner);
                return;
            }
            let interpreter_state_before_the_owner_drop = pyo3::ffi::PyGILState_Ensure();
            drop(owner);
            pyo3::ffi::PyGILState_Release(interpreter_state_before_the_owner_drop);
        }
    }
}

/// Wrap `address` in the negotiated DLPack exchange shape — the one
/// place the shape selection and the read-only flag derivation live, so
/// the host and device exports cannot drift.
pub(crate) fn dlpack_capsule_over<'py>(
    python: Python<'py>,
    address: u64,
    layout: PixelExchangeTensorLayout,
    device: Device,
    exchange_shape: DlpackExchangeShape,
    read_only: bool,
    owner: dlpack::CapsuleOwner,
) -> PyResult<Bound<'py, PyAny>> {
    let owner: dlpack::CapsuleOwner =
        Box::new(CapsuleOwnerDroppedWithTheInterpreterAttached(Some(owner)));
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

/// Off Linux there is no staging to map; the plane view refuses instead.
#[cfg(not(target_os = "linux"))]
pub(crate) fn map_the_cpu_staging_without_reading_a_frame_in(
    _python: Python<'_>,
    _owned_memory: &Arc<GpuSurfaceOwnedMemory>,
) -> PyResult<()> {
    Ok(())
}

/// Whether the surface is currently locked for CPU access, and for what.
///
/// An access-discipline flag, not a synchronisation point — it performs
/// no wait. Ordering against the producer comes from publication: a
/// built-in source waits on its own timeline before it writes the
/// surface id to its output port (the camera's V4L2 capture arm does this
/// at its host-readback wait), so a frame a consumer can name is a frame the
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
