// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

#[cfg(target_os = "linux")]
use parking_lot::Mutex;
use pyo3::exceptions::PyBufferError;
#[cfg(not(target_os = "linux"))]
use pyo3::exceptions::PyNotImplementedError;
#[cfg(target_os = "linux")]
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
#[cfg(target_os = "linux")]
use streamlib_adapter_cuda::dlpack::DeviceType;

use crate::python_gpu_surface_pixel_exchange::GpuSurfaceOwnedMemory;
#[cfg(target_os = "linux")]
use crate::python_gpu_surface_pixel_exchange::{
    PreparedDeviceExport, StagedWriteBackSource, device_dlpack_capsule,
    exchange_shape_for_max_version, prepare_device_export,
};

#[cfg(target_os = "linux")]
use super::left_by_a_propagating_exception;

/// The scope a third-party GPU package reaches a surface's pixels through.
///
/// Entering blits the surface into its linear device-export staging and
/// serves DLPack capsules over it; leaving normally blits any write back,
/// ordered on the surface's timeline ahead of the engine's next read;
/// leaving by a propagating exception discards the write. The engine owns
/// the ordering — no fence or timeline vocabulary appears here.
///
/// Holds its own share of the owned memory, so the surface (and an
/// acquired texture's pool slot) outlives the handle for as long as the
/// scope or any capsule minted inside it does.
#[pyclass(name = "GpuSurfaceDeviceTensorScope", module = "streamlib", frozen)]
pub(crate) struct PythonGpuSurfaceDeviceTensorScope {
    owned_memory: Arc<GpuSurfaceOwnedMemory>,
    /// The export prepared at `__enter__` — the blit has run and the
    /// layout is derived, so every capsule this scope mints serves that
    /// one blit instead of re-reading the surface mid-scope.
    #[cfg(target_os = "linux")]
    prepared_device_export: Mutex<Option<PreparedDeviceExport>>,
}

impl PythonGpuSurfaceDeviceTensorScope {
    pub(super) fn over(owned_memory: Arc<GpuSurfaceOwnedMemory>) -> Self {
        Self {
            owned_memory,
            #[cfg(target_os = "linux")]
            prepared_device_export: Mutex::new(None),
        }
    }

    /// The export `__enter__` prepared, or the refusal that says this
    /// scope is not entered — the structural guard on every capsule.
    #[cfg(target_os = "linux")]
    fn entered_device_export(&self) -> PyResult<PreparedDeviceExport> {
        self.prepared_device_export
            .lock()
            .clone()
            .ok_or_else(device_tensor_scope_not_entered_error)
    }
}

/// The refusal every accessor of an unentered scope answers with.
#[cfg(target_os = "linux")]
fn device_tensor_scope_not_entered_error() -> PyErr {
    PyRuntimeError::new_err(
        "this device-tensor scope is not entered: use it as a context manager \
         (`with surface.as_device_tensor() as tensor:`) — entering is what runs the \
         blit the tensor reads",
    )
}

#[pymethods]
impl PythonGpuSurfaceDeviceTensorScope {
    fn __enter__(python_self: PyRef<'_, Self>) -> PyResult<PyRef<'_, Self>> {
        #[cfg(target_os = "linux")]
        {
            if python_self.prepared_device_export.lock().is_some() {
                return Err(PyRuntimeError::new_err(
                    "this device-tensor scope is already entered; a scope serves one blit — \
                     open a new scope with as_device_tensor() for the next one",
                ));
            }
            let prepared = prepare_device_export(python_self.py(), &python_self.owned_memory)?;
            if !prepared.writable {
                return Err(PyRuntimeError::new_err(
                    "this surface cannot take a write-back — it is a pool member its \
                     producer still owns, or a texture allocated without \"copy_dst\" usage — \
                     so no write door edits it: this write-in-place scope refuses rather than \
                     discarding your edits silently, and the cast object's cpu() hands \
                     its array out read-only under the same rule. Reading needs no write door: lock(), \
                     then as_numpy or __dlpack__",
                ));
            }
            *python_self.prepared_device_export.lock() = Some(prepared);
            Ok(python_self)
        }
        #[cfg(not(target_os = "linux"))]
        Err(PyNotImplementedError::new_err(
            "the device-tensor scope is a Linux capability: it rides the CUDA device export, \
             which this platform does not carry",
        ))
    }

    /// Leaving normally blits any write back into the surface; leaving by
    /// a propagating exception discards it — the write did not finish,
    /// and blitting a half-written view back would publish a torn frame.
    /// Always answers `False`: discarding never suppresses the raise.
    #[pyo3(signature = (exception_type = None, exception = None, traceback = None))]
    fn __exit__(
        &self,
        python: Python<'_>,
        exception_type: Option<&Bound<'_, PyAny>>,
        exception: Option<&Bound<'_, PyAny>>,
        traceback: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let _ = (exception, traceback);
        #[cfg(target_os = "linux")]
        {
            let _prepared_released = self.prepared_device_export.lock().take();
            if left_by_a_propagating_exception(exception_type) {
                self.owned_memory.pending_staged_write_back().discard();
            } else {
                self.owned_memory
                    .pending_staged_write_back()
                    .publish_if_armed(python, &self.owned_memory)?;
            }
            Ok(false)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (python, exception_type);
            Ok(false)
        }
    }

    /// The CUDA device the scope's tensors live on.
    fn __dlpack_device__(&self) -> PyResult<(i32, i32)> {
        #[cfg(target_os = "linux")]
        {
            let device = self
                .prepared_device_export
                .lock()
                .as_ref()
                .map(|prepared| prepared.export.imported_dlpack_device())
                .ok_or_else(device_tensor_scope_not_entered_error)?;
            Ok((device.device_type as i32, device.device_id))
        }
        #[cfg(not(target_os = "linux"))]
        Err(PyNotImplementedError::new_err(
            "the device-tensor scope is a Linux capability",
        ))
    }

    /// A DLPack capsule over the blitted view — what `torch.from_dlpack`
    /// consumes. Writable when the surface's export is; a writable
    /// capsule is what arms the blit-back at `__exit__`.
    #[pyo3(signature = (stream = None, max_version = None, dl_device = None, copy = None))]
    fn __dlpack__<'py>(
        &self,
        python: Python<'py>,
        stream: Option<&Bound<'py, PyAny>>,
        max_version: Option<(u32, u32)>,
        dl_device: Option<(i32, i32)>,
        copy: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // No stream to order against here: the blit-out retired before
        // `__enter__` returned, and the blit-back at `__exit__` runs a
        // device-wide CUDA synchronize before the engine's copy reads
        // the staging.
        let _ = stream;
        if copy == Some(true) {
            return Err(PyBufferError::new_err(
                "this scope exports in place; ask the consumer to copy the tensor instead",
            ));
        }
        #[cfg(target_os = "linux")]
        {
            if let Some((device_type, _)) = dl_device
                && device_type == DeviceType::Cpu as i32
            {
                return Err(PyBufferError::new_err(
                    "this scope serves the device side only; for the host mapping use the \
                     surface handle itself — lock() plus as_numpy or __dlpack__",
                ));
            }
            let prepared = self.entered_device_export()?;
            // Always writable inside a scope: `__enter__` refused a
            // read-only export, so every capsule minted here arms the
            // blit-back.
            let no_read_only_lock_applies = false;
            // Armed before the capsule is minted, like every other door:
            // the surface's cell is shared with the handle that minted this
            // scope, so a CPU staged edit already outstanding is refused
            // here instead of being overwritten at this scope's exit.
            self.owned_memory
                .pending_staged_write_back()
                .arm(StagedWriteBackSource::DeviceExportStaging)?;
            let capsule = device_dlpack_capsule(
                python,
                &self.owned_memory,
                prepared,
                exchange_shape_for_max_version(max_version),
                no_read_only_lock_applies,
            )?;
            Ok(capsule)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (python, max_version, dl_device);
            Err(PyNotImplementedError::new_err(
                "the device-tensor scope is a Linux capability",
            ))
        }
    }
}
