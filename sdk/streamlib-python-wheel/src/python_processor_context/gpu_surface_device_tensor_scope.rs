// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

#[cfg(target_os = "linux")]
use parking_lot::Mutex;
use pyo3::exceptions::PyBufferError;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use pyo3::exceptions::PyNotImplementedError;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib_adapter_cuda::dlpack::DeviceType;

use crate::python_gpu_surface_pixel_exchange::GpuSurfaceOwnedMemory;
#[cfg(target_os = "macos")]
use crate::python_gpu_surface_pixel_exchange::{METAL_DLPACK_DEVICE, metal_dlpack_capsule};
#[cfg(target_os = "linux")]
use crate::python_gpu_surface_pixel_exchange::{
    PreparedDeviceExport, StagedWriteBackSource, device_dlpack_capsule, prepare_device_export,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_gpu_surface_pixel_exchange::{
    dlpack_device_as_python_pair, exchange_shape_for_max_version,
};
#[cfg(target_os = "macos")]
use crate::python_metal_framework_queue_synchronization::drain_torch_mps_queue_if_imported;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::left_by_a_propagating_exception;

/// The scope a third-party GPU package reaches a surface's pixels through.
///
/// On Linux, entering blits the surface into its linear device-export
/// staging and serves CUDA capsules over it; leaving normally blits any write
/// back ahead of the engine's next read; leaving by a propagating exception
/// discards the write. On macOS the capsules are `kDLMetal` over the
/// surface's own IOSurface pages, so a write lands in the surface itself:
/// leaving — normally or by a raise — retires the Metal frameworks' queues
/// before the scope closes, and a raise leaves whatever stores already
/// landed. The engine owns the ordering — no fence or timeline vocabulary
/// appears here.
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
    /// Whether the scope is entered — the structural guard on every capsule.
    #[cfg(target_os = "macos")]
    device_tensor_scope_is_entered: std::sync::atomic::AtomicBool,
}

impl PythonGpuSurfaceDeviceTensorScope {
    pub(super) fn over(owned_memory: Arc<GpuSurfaceOwnedMemory>) -> Self {
        Self {
            owned_memory,
            #[cfg(target_os = "linux")]
            prepared_device_export: Mutex::new(None),
            #[cfg(target_os = "macos")]
            device_tensor_scope_is_entered: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The refusal unless this scope is entered.
    #[cfg(target_os = "macos")]
    fn require_this_device_tensor_scope_entered(&self) -> PyResult<()> {
        if self
            .device_tensor_scope_is_entered
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            Ok(())
        } else {
            Err(device_tensor_scope_not_entered_error())
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn device_tensor_scope_not_entered_error() -> PyErr {
    PyRuntimeError::new_err(
        "this device-tensor scope is not entered: use it as a context manager \
         (`with surface.as_device_tensor() as tensor:`) — entering is what opens the \
         device view the tensor reads",
    )
}

/// The refusal a scope entered twice answers with.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn device_tensor_scope_already_entered_error() -> PyErr {
    PyRuntimeError::new_err(
        "this device-tensor scope is already entered; a scope serves one entry — open a new \
         scope with as_device_tensor() for the next one",
    )
}

#[pymethods]
impl PythonGpuSurfaceDeviceTensorScope {
    fn __enter__(python_self: PyRef<'_, Self>) -> PyResult<PyRef<'_, Self>> {
        #[cfg(target_os = "linux")]
        {
            if python_self.prepared_device_export.lock().is_some() {
                return Err(device_tensor_scope_already_entered_error());
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
        // Every surface a macOS helper checks out takes a write-back — its
        // IOSurface is the allocation's only backing — so what is left to
        // refuse is a device that cannot alias the pages from Metal.
        #[cfg(target_os = "macos")]
        {
            if python_self
                .device_tensor_scope_is_entered
                .swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(device_tensor_scope_already_entered_error());
            }
            if let Err(no_metal_buffer) = python_self
                .owned_memory
                .metal_buffer_over_the_iosurface_pages()
            {
                python_self
                    .device_tensor_scope_is_entered
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                return Err(PyRuntimeError::new_err(format!(
                    "this surface has no Metal view, so no device tensor can write it in \
                     place: {no_metal_buffer}. Its pixels stay reachable on the host — \
                     lock(), then as_numpy or __dlpack__"
                )));
            }
            Ok(python_self)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Err(PyNotImplementedError::new_err(
            "no GPU surface exchange exists on this platform",
        ))
    }

    /// Leaving normally publishes the write; leaving by a propagating
    /// exception publishes nothing more. On Linux that is a blit back, or
    /// its discard — blitting a half-written view back would publish a torn
    /// frame. On macOS the stores already sit in the surface, so both ways
    /// out retire the Metal frameworks' queues, and a raise leaves the stores
    /// that landed. Always answers `False`: a raise is never suppressed, nor
    /// replaced by a failure to retire the queues under it.
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
        #[cfg(target_os = "macos")]
        {
            if !self
                .device_tensor_scope_is_entered
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Ok(false);
            }
            let metal_writes_retired = drain_torch_mps_queue_if_imported(python);
            if left_by_a_propagating_exception(exception_type) {
                if let Err(retire_failure) = metal_writes_retired {
                    tracing::warn!(
                        "retiring the Metal frameworks' queues under a propagating exception \
                         failed: {retire_failure}"
                    );
                }
                return Ok(false);
            }
            metal_writes_retired?;
            Ok(false)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (python, exception_type);
            Ok(false)
        }
    }

    /// The device the scope's tensors live on — CUDA on Linux, Metal on
    /// macOS.
    fn __dlpack_device__(&self) -> PyResult<(i32, i32)> {
        #[cfg(target_os = "linux")]
        {
            let device = self
                .prepared_device_export
                .lock()
                .as_ref()
                .map(|prepared| prepared.export.imported_dlpack_device())
                .ok_or_else(device_tensor_scope_not_entered_error)?;
            Ok(dlpack_device_as_python_pair(device))
        }
        #[cfg(target_os = "macos")]
        {
            self.require_this_device_tensor_scope_entered()?;
            Ok(dlpack_device_as_python_pair(METAL_DLPACK_DEVICE))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Err(PyNotImplementedError::new_err(
            "no GPU surface exchange exists on this platform",
        ))
    }

    /// A DLPack capsule over the scope's device view — what
    /// `torch.from_dlpack` and `mx.from_dlpack` consume. Always writable: a
    /// scope over a surface that takes no write-back refused at `__enter__`.
    #[pyo3(signature = (stream = None, max_version = None, dl_device = None, copy = None))]
    fn __dlpack__<'py>(
        &self,
        python: Python<'py>,
        stream: Option<&Bound<'py, PyAny>>,
        max_version: Option<(u32, u32)>,
        dl_device: Option<(i32, i32)>,
        copy: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // No stream to order against. On Linux the blit-out retired before
        // `__enter__` returned, and the blit-back at `__exit__` runs a
        // device-wide CUDA synchronize before the engine's copy reads the
        // staging. On macOS torch and MLX pass no stream for `kDLMetal`;
        // the exit's drain and the MLX `mx.eval` contract order the write.
        let _ = stream;
        if copy == Some(true) {
            return Err(PyBufferError::new_err(
                "this scope exports in place; ask the consumer to copy the tensor instead",
            ));
        }
        if let Some((device_type, _)) = dl_device
            && device_type == DeviceType::Cpu as i32
        {
            return Err(PyBufferError::new_err(
                "this scope serves the device side only; for the host mapping use the \
                 surface handle itself — lock() plus as_numpy or __dlpack__",
            ));
        }
        #[cfg(target_os = "linux")]
        {
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
        #[cfg(target_os = "macos")]
        {
            self.require_this_device_tensor_scope_entered()?;
            let no_read_only_lock_applies = false;
            metal_dlpack_capsule(
                python,
                &self.owned_memory,
                exchange_shape_for_max_version(max_version),
                no_read_only_lock_applies,
            )
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (python, max_version);
            Err(PyNotImplementedError::new_err(
                "no GPU surface exchange exists on this platform",
            ))
        }
    }
}
