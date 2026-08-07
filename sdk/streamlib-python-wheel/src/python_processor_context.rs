// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The capability-typed runtime contexts handed to Python lifecycle hooks.
//!
//! Built in the helper process the processor runs in: everything a hook
//! reads is local, was passed down by the parent, or crosses to it — the
//! GPU surface through the exchange client, whose escalate wait releases
//! the GIL so a slow parent parks one thread and never the interpreter.
//! GIL discipline: a call that crosses to the parent is made attached —
//! there is no reaching the bridge otherwise — and releases the GIL inside
//! its own wait. Everything else that can block runs inside a
//! `python.detach(..)` closure.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use pyo3::exceptions::{
    PyBufferError, PyNotImplementedError, PyRuntimeError, PyTypeError, PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib::sdk::rhi::PixelFormat;
use streamlib_adapter_cuda::dlpack::DeviceType;

use crate::python_bag_conversion::{json_value_to_python_object, python_object_to_json_value};
use crate::python_gpu_surface_pixel_exchange::{
    CpuAccessGate, GpuSurfaceOwnedMemory, HOST_VISIBLE_DLPACK_DEVICE, device_export_available,
    exchange_shape_for_max_version, host_visible_dlpack_capsule,
};
#[cfg(target_os = "linux")]
use crate::python_gpu_surface_pixel_exchange::{
    device_dlpack_capsule, imported_device_for, prepare_device_export,
    publish_device_write_back_to_surface,
};
#[cfg(target_os = "linux")]
use crate::python_helper_process_pixel_exchange::HelperCheckedOutPixelSurface;
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

/// The refusal a GPU call gets when this process has neither an engine view
/// nor a channel to a parent that has one.
///
/// In a helper process the pixel exchange normally crosses to the parent;
/// reaching this refusal means the helper was started without its
/// surface-share channel — a platform without one, or a parent too old to
/// pass it.
/// The refusal `escalate` gives on either capability.
///
/// `sibling_capability_attribute_name` is the other capability on the same
/// context, so the message points at the whole surface the callback's
/// operations moved to rather than half of it.
fn escalate_scope_cannot_cross_the_process_boundary_error(
    sibling_capability_attribute_name: &str,
) -> PyErr {
    PyRuntimeError::new_err(format!(
        "escalate() gives its callback one atomic privileged scope, which cannot span a process \
         boundary. The operations it wrapped are methods on this capability and on \
         `{sibling_capability_attribute_name}` — call them directly; each is privileged on its own"
    ))
}

fn gpu_unreachable_from_a_helper_process_error() -> PyErr {
    PyRuntimeError::new_err(
        "the GPU is not reachable from this Python processor: its helper process was started \
         without a surface-share channel to the engine. Frames still flow — `ctx.inputs` and \
         `ctx.outputs` carry bags — but acquiring or mapping a surface cannot.",
    )
}

// =============================================================================
// GPU surface handle
// =============================================================================

/// An owned GPU surface as seen from Python.
///
/// Owning the engine value (rather than an id to re-resolve) is what keeps a
/// pool slot or a pooled texture alive until `close()` / the context manager
/// releases it. The value itself sits behind an `Arc` shared with every DLPack
/// capsule minted from this handle, so a tensor Python is still holding keeps
/// the memory addressable after the handle is closed.
#[pyclass(name = "GpuSurfaceHandle", module = "streamlib", frozen)]
pub(crate) struct PythonGpuSurfaceHandle {
    /// `None` for pooled textures — see [`Self::surface_id`].
    minted_surface_id: Option<String>,
    surface_width: u32,
    surface_height: u32,
    surface_format_name: String,
    owned_memory: Mutex<Option<Arc<GpuSurfaceOwnedMemory>>>,
    cpu_access: CpuAccessGate,
    /// A writable device tensor was exported under the current write
    /// lock; `unlock()` / `close()` publishes the staging back into the
    /// surface.
    #[cfg(target_os = "linux")]
    device_write_pending: std::sync::atomic::AtomicBool,
    /// Which DLPack side this handle serves when the consumer expresses
    /// no preference — decided once, so `__dlpack_device__` and
    /// `__dlpack__` cannot disagree across calls.
    #[cfg(target_os = "linux")]
    natural_dlpack_side_is_device: std::sync::OnceLock<bool>,
}

impl PythonGpuSurfaceHandle {
    fn new(
        minted_surface_id: Option<String>,
        surface_width: u32,
        surface_height: u32,
        surface_format_name: String,
        owned_memory: Arc<GpuSurfaceOwnedMemory>,
    ) -> Self {
        Self {
            minted_surface_id,
            surface_width,
            surface_height,
            surface_format_name,
            owned_memory: Mutex::new(Some(owned_memory)),
            cpu_access: CpuAccessGate::new_unlocked(),
            #[cfg(target_os = "linux")]
            device_write_pending: std::sync::atomic::AtomicBool::new(false),
            #[cfg(target_os = "linux")]
            natural_dlpack_side_is_device: std::sync::OnceLock::new(),
        }
    }

    /// A surface a helper process checked out of its parent: consumer-imported
    /// memory behind the same handle surface the engine path mints.
    #[cfg(target_os = "linux")]
    fn from_helper_checked_out_surface(checked_out: HelperCheckedOutPixelSurface) -> Self {
        let surface_id = checked_out.surface_id.clone();
        let (width, height, format) = (checked_out.width, checked_out.height, checked_out.format);
        Self::new(
            Some(surface_id.clone()),
            width,
            height,
            format.wire_name().to_string(),
            // The release an acquired surface owes its parent rides the
            // debt inside the checked-out value, so this holds nothing but
            // the value and the id it travels under.
            GpuSurfaceOwnedMemory::new(checked_out, Some(surface_id)),
        )
    }

    /// Drop this handle's share of the owned memory. The resource goes away
    /// once the last outstanding DLPack capsule does too; idempotent.
    ///
    /// The take is hoisted out of the drop expression so the mutex guard is
    /// released before the value drops: a helper-checked-out surface's drop
    /// re-attaches to the GIL for its `release_handle` round trip, and a
    /// thread holding this mutex while waiting for the GIL deadlocks against
    /// any GIL-holding thread reading the handle.
    fn release_owned_engine_value(&self) {
        let released_share = self.owned_memory.lock().take();
        drop(released_share);
    }

    /// Borrow the shared memory anchor, or fail if the handle is closed.
    fn owned_memory(&self) -> PyResult<Arc<GpuSurfaceOwnedMemory>> {
        self.owned_memory.lock().clone().ok_or_else(|| {
            PyRuntimeError::new_err("this surface is closed; acquire or resolve it again")
        })
    }
}

impl PythonGpuSurfaceHandle {
    /// Decide — once per handle — whether the no-preference DLPack side
    /// is the device. Detached work: the first call may allocate staging
    /// and import into CUDA. A handle that answered kDLCUDA here never
    /// silently downgrades later; a refill failure after this raises.
    #[cfg(target_os = "linux")]
    fn natural_side_is_device(
        &self,
        python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> bool {
        *self.natural_dlpack_side_is_device.get_or_init(|| {
            device_export_available(owned_memory)
                && imported_device_for(python, owned_memory).is_ok()
        })
    }

    /// Publish a pending device-side write, once. Shared by `unlock` and
    /// `close` so the context-manager spelling cannot silently drop an
    /// edit.
    #[cfg(target_os = "linux")]
    fn publish_pending_device_write(&self, python: Python<'_>) -> PyResult<()> {
        if !self
            .device_write_pending
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        // Bound before the `if let`, so the guard drops here: inside a
        // condition chain the guard temporary lives to the end of the whole
        // statement, and the publish crosses to the parent — the same
        // mutex-across-the-GIL hazard `release_owned_engine_value` documents.
        let owned_memory = self.owned_memory.lock().clone();
        if let Some(owned_memory) = owned_memory {
            publish_device_write_back_to_surface(python, &owned_memory)?;
        }
        Ok(())
    }
}

impl Drop for PythonGpuSurfaceHandle {
    /// Covers a handle the author never closed. Runs attached (pyclass
    /// deallocation) — acceptable because the release path attaches with
    /// `Python::attach` where it needs Python, which is re-entrant from an
    /// attached thread; `close()` remains the detached fast path.
    fn drop(&mut self) {
        self.release_owned_engine_value();
    }
}

#[pymethods]
impl PythonGpuSurfaceHandle {
    /// The id downstream processors resolve this surface by.
    #[getter]
    fn surface_id(&self) -> PyResult<String> {
        self.minted_surface_id.clone().ok_or_else(|| {
            PyNotImplementedError::new_err(
                "this surface carries no id: handles minted through the lease-bound full-access \
                 capability or a raw DMA-BUF import are not registered anywhere a consumer \
                 could resolve them",
            )
        })
    }

    #[getter]
    fn width(&self) -> u32 {
        self.surface_width
    }

    #[getter]
    fn height(&self) -> u32 {
        self.surface_height
    }

    #[getter]
    fn format(&self) -> String {
        self.surface_format_name.clone()
    }

    /// Row pitch in bytes, including any padding the allocation carries.
    #[getter]
    fn bytes_per_row(&self, python: Python<'_>) -> PyResult<u64> {
        let owned_memory = self.owned_memory()?;
        python.detach(|| Ok(owned_memory.host_visible_pixel_plane()?.bytes_per_row))
    }

    /// Base address of the host mapping, or `None` when the surface is
    /// not locked. Callers that want a typed view use `as_numpy` or
    /// `__dlpack__`; this is the escape hatch for building one by hand.
    #[getter]
    fn base_address(&self, python: Python<'_>) -> PyResult<Option<usize>> {
        if !self.cpu_access.is_locked() {
            return Ok(None);
        }
        let owned_memory = self.owned_memory()?;
        python.detach(|| {
            Ok(Some(
                owned_memory.host_visible_pixel_plane()?.base_address as usize,
            ))
        })
    }

    /// Release the underlying GPU resource. Idempotent.
    fn close(&self, python: Python<'_>) -> PyResult<()> {
        // Releasing can return a slot to a pool under engine locks and talk to
        // the surface-share daemon — detached, like every potentially-blocking
        // engine call. A pending device write publishes first: the
        // context-manager spelling reaches close without an explicit
        // unlock, and dropping the edit silently there is data loss.
        // A failed publish must not skip the release: the handle would
        // stay open with its pool slot pinned, in the exact spelling
        // (`with` → close) users write. Clean up, then surface the
        // failure.
        #[cfg(target_os = "linux")]
        let publish_outcome = self.publish_pending_device_write(python);
        python.detach(|| {
            self.cpu_access.unlock();
            self.release_owned_engine_value();
        });
        #[cfg(target_os = "linux")]
        publish_outcome?;
        Ok(())
    }

    fn __enter__(python_self: PyRef<'_, Self>) -> PyRef<'_, Self> {
        python_self
    }

    #[pyo3(signature = (*_exception_details))]
    fn __exit__(
        &self,
        python: Python<'_>,
        _exception_details: &Bound<'_, PyAny>,
    ) -> PyResult<bool> {
        self.close(python)?;
        Ok(false)
    }

    /// Open CPU access to the pixels, declaring read or write intent.
    ///
    /// This performs no wait: ordering against the producer comes from
    /// publication, since a source finishes its GPU work before it writes the
    /// surface id downstream. `read_only=False` is what marks an exported
    /// tensor writable.
    #[pyo3(signature = (read_only = true))]
    fn lock(&self, python: Python<'_>, read_only: bool) -> PyResult<()> {
        let owned_memory = self.owned_memory()?;
        python.detach(|| -> PyResult<()> {
            // The gate serves both sides: a device-only surface (a pooled
            // texture) has no host mapping to check, but its device export
            // still rides the same lock.
            match owned_memory.host_visible_pixel_plane() {
                Ok(plane_view) => {
                    if plane_view.base_address.is_null() {
                        return Err(PyBufferError::new_err(
                            "surface has no host mapping; it is a DEVICE_LOCAL allocation",
                        ));
                    }
                }
                Err(no_host_side) => {
                    if !device_export_available(&owned_memory) {
                        return Err(no_host_side);
                    }
                }
            }
            self.cpu_access.lock_for(read_only);
            Ok(())
        })
    }

    /// Close CPU access, publishing any pending device-side write back
    /// into the surface first. Idempotent.
    fn unlock(&self, python: Python<'_>) -> PyResult<()> {
        // The gate opens whether or not the publish succeeded — a
        // surface left locked after a failed publish would refuse
        // every later access with a message about locking, hiding
        // the real failure this raises.
        #[cfg(target_os = "linux")]
        let publish_outcome = self.publish_pending_device_write(python);
        python.detach(|| self.cpu_access.unlock());
        #[cfg(target_os = "linux")]
        publish_outcome?;
        Ok(())
    }

    /// The DLPack device this surface's tensors live on.
    ///
    /// A device-exchange surface answers with the CUDA device its memory is
    /// imported onto, performing the import if it has not happened yet — the
    /// driver's classification of the pointer is what distinguishes true
    /// device memory from a downgrade to pinned host memory, and guessing
    /// here would contradict the capsule `__dlpack__` goes on to hand back.
    fn __dlpack_device__(&self, python: Python<'_>) -> PyResult<(i32, i32)> {
        let owned_memory = self.owned_memory()?;
        // Routed through the same once-per-handle decision `__dlpack__`
        // serves, so a probe failure here (answered CPU) cannot be
        // followed by a successful device capsule there.
        #[cfg(target_os = "linux")]
        if self.natural_side_is_device(python, &owned_memory) {
            let device = imported_device_for(python, &owned_memory)?;
            return Ok((device.device_type as i32, device.device_id));
        }
        #[cfg(not(target_os = "linux"))]
        let _ = python;
        Ok((
            HOST_VISIBLE_DLPACK_DEVICE.device_type as i32,
            HOST_VISIBLE_DLPACK_DEVICE.device_id,
        ))
    }

    /// A DLPack capsule over the pixels — what `torch.from_dlpack` and
    /// `numpy.from_dlpack` consume.
    ///
    /// The tensor may outlive this handle: it holds its own share of the
    /// surface, so the pool slot is not reused until the tensor is released.
    /// A consumer that negotiates `max_version >= (1, 0)` gets the versioned
    /// exchange shape, which is the only one that can report the surface as
    /// writable.
    #[pyo3(signature = (stream = None, max_version = None, dl_device = None, copy = None))]
    fn __dlpack__<'py>(
        &self,
        python: Python<'py>,
        stream: Option<&Bound<'py, PyAny>>,
        max_version: Option<(u32, u32)>,
        dl_device: Option<(i32, i32)>,
        copy: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        // No stream to order against: the host mapping is CPU memory, and the
        // device path's ordering is the lock, not a CUDA stream.
        let _ = stream;
        // Refused rather than ignored — exporting in place when the consumer
        // asked for a copy hands back a tensor aliasing memory it believes it
        // owns, and the aliasing shows up much later as corruption.
        if copy == Some(true) {
            return Err(PyBufferError::new_err(
                "this surface exports in place; ask the consumer to copy the tensor instead",
            ));
        }
        self.cpu_access.require_locked()?;
        let owned_memory = self.owned_memory()?;
        let exchange_shape = exchange_shape_for_max_version(max_version);
        let read_only = self.cpu_access.is_read_only();

        // `dl_device` is the consumer's request for a particular side of a
        // surface that has two. Absent means "wherever you naturally
        // live" — the side `__dlpack_device__` already advertised, decided
        // once per handle. A device-side failure after that raises: a
        // consumer told kDLCUDA must never be handed a host capsule.
        #[cfg(target_os = "linux")]
        {
            let wants_host = match dl_device {
                Some((device_type, _)) => device_type == DeviceType::Cpu as i32,
                None => !self.natural_side_is_device(python, &owned_memory),
            };
            if wants_host {
                return host_visible_dlpack_capsule(
                    python,
                    &owned_memory,
                    exchange_shape,
                    read_only,
                );
            }
            // The refill is a GPU submit plus a bounded wait, and on the
            // first call the staging allocation and CUDA import too. It
            // detaches around the blocking work itself — a helper's arm
            // has to stay attached to reach the parent at all.
            let prepared = prepare_device_export(python, &owned_memory)?;
            let writable_export = !read_only && prepared.writable;
            let capsule =
                device_dlpack_capsule(python, &owned_memory, prepared, exchange_shape, read_only)?;
            if writable_export {
                self.device_write_pending
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(capsule)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = dl_device;
            host_visible_dlpack_capsule(python, &owned_memory, exchange_shape, read_only)
        }
    }

    /// A numpy view of the pixels, sharing memory with the surface.
    ///
    /// `(height, width, channels)` `uint8` for the 8-bit formats, with the
    /// allocation's row pitch preserved in the strides.
    fn as_numpy<'py>(python_self: &Bound<'py, Self>) -> PyResult<Bound<'py, PyAny>> {
        let python = python_self.py();
        // Imported lazily so the wheel never takes a numpy dependency: a user
        // reaching for a numpy view already has numpy.
        let numpy = python.import("numpy").map_err(|_| {
            PyRuntimeError::new_err("as_numpy needs numpy installed; `__dlpack__` works without it")
        })?;
        // `from_dlpack` consumes the exporting object and calls `__dlpack__`
        // on it, so there is exactly one export path. `device="cpu"` is not
        // decoration: an exchange surface's natural side is the GPU, and
        // without the request numpy would be handed a device pointer and
        // refuse it.
        let host_request = pyo3::types::PyDict::new(python);
        host_request.set_item("device", "cpu")?;
        numpy
            .call_method("from_dlpack", (python_self,), Some(&host_request))
            .map_err(|from_dlpack_failure| {
                // Only the specific unexpected-keyword TypeError means old
                // numpy; any other failure is numpy's own and passes through
                // with the hint chained as the cause, never replaced.
                if from_dlpack_failure.is_instance_of::<PyTypeError>(python)
                    && from_dlpack_failure.to_string().contains("device")
                {
                    let old_numpy_hint = PyRuntimeError::new_err(
                        "as_numpy needs numpy 2.1 or newer, whose `from_dlpack` accepts a \
                         `device` request; older numpy cannot ask for the host side of a \
                         surface",
                    );
                    old_numpy_hint.set_cause(python, Some(from_dlpack_failure));
                    return old_numpy_hint;
                }
                from_dlpack_failure
            })
    }
}

// =============================================================================
// GPU capability views
// =============================================================================

pub(crate) fn gpu_operation_error(failure: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(failure.to_string())
}

/// Non-allocating GPU capability, valid for the whole processor life.
///
/// Every call crosses to the parent through the exchange client — the
/// engine and its pools live one process away. `None` means this helper
/// has no surface-share channel, and every call refuses by name.
#[pyclass(name = "GpuContextLimitedAccess", module = "streamlib", frozen)]
pub(crate) struct PythonGpuContextLimitedAccess {
    helper_process_exchange_client: Option<Arc<HelperProcessGpuExchangeClient>>,
}

impl PythonGpuContextLimitedAccess {
    fn new_for_helper_process(
        helper_process_exchange_client: Option<Arc<HelperProcessGpuExchangeClient>>,
    ) -> Self {
        Self {
            helper_process_exchange_client,
        }
    }
}

#[pymethods]
impl PythonGpuContextLimitedAccess {
    /// Acquire a pixel buffer from the pre-reserved pool.
    ///
    /// The pool lives with the engine: the parent allocates and checks the
    /// buffer into surface-share, and this process checks it out and imports
    /// the mapping — same handle, same views.
    #[pyo3(signature = (width, height, format = "bgra"))]
    fn acquire_pixel_buffer(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        format: &str,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let pixel_format = parse_pixel_format_name(format)?;
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let checked_out = exchange_client.acquire_pixel_buffer(
                python,
                width,
                height,
                pixel_format.wire_name(),
            )?;
            return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
                checked_out,
            ));
        }
        let _ = (python, width, height, pixel_format);
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Acquire a pooled texture from the pre-reserved pool.
    ///
    /// Refused: a pool texture recycles per frame, and registering each
    /// acquire into surface-share would put a per-frame SCM_RIGHTS round
    /// trip and the service's lifetime bookkeeping on the hot path.
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer for textures"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter names are the API; stubtest compares them"
    )]
    fn acquire_texture(
        &self,
        width: u32,
        height: u32,
        format: &str,
        usage: Vec<String>,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        Err(PyRuntimeError::new_err(
            "device textures are not reachable from a Python processor: a pool texture is \
             not registered for cross-process import. `acquire_pixel_buffer` is the \
             CPU-reachable path; device-side tensors ride the device-export staging path",
        ))
    }

    /// Run `privileged_callback` with a temporary full-access GPU capability.
    ///
    /// Refused, and it is the shape rather than the reach that refuses it —
    /// the same answer `ctx.gpu_full_access.escalate` gives, because it is
    /// the same door.
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer for escalation"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter name is the API; stubtest compares it"
    )]
    fn escalate(&self, privileged_callback: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        Err(escalate_scope_cannot_cross_the_process_boundary_error(
            "ctx.gpu_full_access",
        ))
    }

    /// Resolve a surface id another processor published into a handle.
    fn resolve_surface(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let checked_out = exchange_client.resolve_surface(python, surface_id)?;
            return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
                checked_out,
            ));
        }
        let _ = (python, surface_id);
        Err(gpu_unreachable_from_a_helper_process_error())
    }
}

/// The privileged GPU capability a `setup` / `teardown` hook receives.
///
/// Every method here is one escalate round trip to the parent, which runs
/// the privileged work against the engine's own capability and answers
/// with a handle. That is the shape, not a degradation of one: the
/// engine's escalate gate serializes runtime-wide and waits for device
/// idle before releasing, so each op arrives back already ordered.
///
/// What does not survive the process boundary is a *scope* spanning
/// several ops — see this capability's `escalate` refusal.
#[pyclass(name = "GpuContextFullAccess", module = "streamlib", frozen)]
pub(crate) struct PythonGpuContextFullAccess {
    /// `None` means this helper was started without its GPU channels, and
    /// every method refuses by name.
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    helper_process_exchange_client: Option<Arc<HelperProcessGpuExchangeClient>>,
}

#[pymethods]
impl PythonGpuContextFullAccess {
    /// Acquire a pixel buffer through the privileged path.
    #[pyo3(signature = (width, height, format = "bgra"))]
    fn acquire_pixel_buffer(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        format: &str,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let pixel_format = parse_pixel_format_name(format)?;
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let checked_out = exchange_client.acquire_pixel_buffer(
                python,
                width,
                height,
                pixel_format.wire_name(),
            )?;
            return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
                checked_out,
            ));
        }
        let _ = (python, width, height, pixel_format);
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Acquire a pooled texture through the privileged path.
    ///
    /// Refused for the same reason the limited capability refuses it: a
    /// pool texture recycles per frame, and it is not registered for
    /// cross-process import.
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer for textures"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter names are the API; stubtest compares them"
    )]
    fn acquire_texture(
        &self,
        width: u32,
        height: u32,
        format: &str,
        usage: Vec<String>,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        Err(PyRuntimeError::new_err(
            "device textures are not reachable from a Python processor: a pool texture is \
             not registered for cross-process import. `acquire_pixel_buffer` is the \
             CPU-reachable path; device-side tensors ride the device-export staging path",
        ))
    }

    /// Run `privileged_callback` with a temporary full-access GPU capability.
    ///
    /// Refused, and it is the shape rather than the reach that refuses it.
    /// Every method on this capability already escalates on its own, so
    /// the privileged *operations* are all here. What the callback adds
    /// is an atomic scope — the engine's escalate gate held across the
    /// whole closure, nothing else in the runtime escalating meanwhile —
    /// and that cannot cross a process boundary: emulating it would run
    /// each statement in its own gate scope with other processors
    /// interleaving, keeping the spelling and silently dropping the
    /// guarantee it exists for.
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer for escalation"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter name is the API; stubtest compares it"
    )]
    fn escalate(&self, privileged_callback: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        Err(escalate_scope_cannot_cross_the_process_boundary_error(
            "ctx.gpu_limited_access",
        ))
    }

    /// Export a DMA-BUF file descriptor for `surface`, for native code that
    /// speaks DMA-BUF — EGL, a V4L2 output device, another process.
    ///
    /// Returns `(fd, byte_size)`. **The caller owns the fd** and must close
    /// it, or hand it to something that takes ownership.
    ///
    /// Answered without leaving this process: the fds arrived here over
    /// SCM_RIGHTS when the surface was checked out, and they are the same
    /// ones a host-side export would mint.
    #[cfg(target_os = "linux")]
    #[expect(
        clippy::unused_self,
        reason = "the surface carries the fds; the capability is the door"
    )]
    fn export_dma_buf(
        &self,
        python: Python<'_>,
        surface: &PythonGpuSurfaceHandle,
    ) -> PyResult<(i32, u64)> {
        let owned_memory = surface.owned_memory()?;
        python.detach(|| owned_memory.export_dma_buf())
    }

    /// Import a DMA-BUF file descriptor as a surface this graph can read.
    #[cfg(target_os = "linux")]
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer for imports"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter names are the API; stubtest compares them"
    )]
    #[pyo3(signature = (fd, width, height, format = "bgra", byte_size = None))]
    fn import_dma_buf(
        &self,
        fd: i32,
        width: u32,
        height: u32,
        format: &str,
        byte_size: Option<u64>,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        Err(PyRuntimeError::new_err(
            "importing a foreign DMA-BUF is not reachable from a Python processor yet: the \
             surface registry a graph reads lives in the app process, and handing it an fd \
             needs a wire that carries one. Exporting works — `export_dma_buf` answers from \
             this process",
        ))
    }

    /// Block until the GPU device is idle.
    fn wait_device_idle(&self, python: Python<'_>) -> PyResult<()> {
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            return exchange_client.wait_device_idle(python);
        }
        let _ = python;
        Err(gpu_unreachable_from_a_helper_process_error())
    }
}

// =============================================================================
// Runtime context views
// =============================================================================

/// Privileged runtime context passed to `setup` / `teardown` / `start` / `stop`.
///
/// Built in the helper process the processor runs in — there is no engine in
/// that process to borrow a view from, so everything a hook reads is either
/// local or was passed down by the parent.
#[pyclass(name = "RuntimeContextFullAccess", module = "streamlib", frozen)]
pub(crate) struct PythonRuntimeContextFullAccess {
    runtime_id: String,
    processor_id: String,
    configuration: serde_json::Value,
    link_input_data_reader: Py<PythonLinkInputDataReader>,
    link_output_data_writer: Py<PythonLinkOutputDataWriter>,
    gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    gpu_full_access_context: Py<PythonGpuContextFullAccess>,
    /// This helper's own record of what its parent last announced, shared with
    /// the limited-access view derived from this one.
    pause_state_announced_by_parent: Arc<AtomicBool>,
}

#[pymethods]
impl PythonRuntimeContextFullAccess {
    /// The context a helper process hands its own processor's privileged
    /// hooks.
    ///
    /// `escalate_request_to_parent` is the bridge's blocking round trip; with
    /// it and the surface-share socket the parent's env names, the GPU
    /// surface works here — without either, GPU calls refuse by name.
    #[staticmethod]
    #[pyo3(signature = (configuration, link_data_access, runtime_id, processor_id, escalate_request_to_parent = None))]
    fn open_for_helper_process(
        python: Python<'_>,
        configuration: &Bound<'_, PyAny>,
        link_data_access: &Bound<'_, PythonProcessorLinkDataAccess>,
        runtime_id: String,
        processor_id: String,
        escalate_request_to_parent: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<Self> {
        let link_data_access = link_data_access.clone().unbind();
        let helper_process_exchange_client = match (
            escalate_request_to_parent,
            std::env::var("STREAMLIB_SURFACE_SOCKET").ok(),
        ) {
            (Some(requester), Some(surface_socket_path)) => {
                Some(Arc::new(HelperProcessGpuExchangeClient::new(
                    requester.clone().unbind(),
                    surface_socket_path.into(),
                )))
            }
            _ => None,
        };
        Ok(Self {
            runtime_id,
            processor_id,
            configuration: python_object_to_json_value(configuration)?,
            link_input_data_reader: Py::new(
                python,
                PythonLinkInputDataReader {
                    link_data_access: link_data_access.clone_ref(python),
                },
            )?,
            link_output_data_writer: Py::new(
                python,
                PythonLinkOutputDataWriter {
                    link_data_access: link_data_access.clone_ref(python),
                },
            )?,
            gpu_limited_access_context: Py::new(
                python,
                PythonGpuContextLimitedAccess::new_for_helper_process(
                    helper_process_exchange_client.clone(),
                ),
            )?,
            gpu_full_access_context: Py::new(
                python,
                PythonGpuContextFullAccess {
                    helper_process_exchange_client,
                },
            )?,
            pause_state_announced_by_parent: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The limited-access view of the same processor — same configuration,
    /// same links, same pause state.
    ///
    /// A helper builds both views once and hands each hook the one its phase
    /// calls for.
    fn limited_access_view_for_helper_process(
        &self,
        python: Python<'_>,
    ) -> PyResult<PythonRuntimeContextLimitedAccess> {
        Ok(PythonRuntimeContextLimitedAccess {
            runtime_id: self.runtime_id.clone(),
            processor_id: self.processor_id.clone(),
            configuration: self.configuration.clone(),
            link_input_data_reader: self.link_input_data_reader.clone_ref(python),
            link_output_data_writer: self.link_output_data_writer.clone_ref(python),
            gpu_limited_access_context: self.gpu_limited_access_context.clone_ref(python),
            pause_state_announced_by_parent: Arc::clone(&self.pause_state_announced_by_parent),
        })
    }

    /// Record the pause state the parent just announced, so `is_paused` and
    /// `should_process` can answer without an engine to ask.
    fn note_pause_state_from_parent(&self, paused: bool) {
        self.pause_state_announced_by_parent
            .store(paused, Ordering::Relaxed);
    }

    /// The processor's configuration, as the dict it was added with.
    #[getter]
    fn config<'py>(&self, python: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        configuration_as_python_dict(python, &self.configuration)
    }

    /// Current monotonic time in nanoseconds (raw `CLOCK_MONOTONIC`).
    #[getter]
    fn time(&self) -> u64 {
        monotonic_clock_now_ns()
    }

    #[getter]
    fn inputs(&self, python: Python<'_>) -> Py<PythonLinkInputDataReader> {
        self.link_input_data_reader.clone_ref(python)
    }

    #[getter]
    fn outputs(&self, python: Python<'_>) -> Py<PythonLinkOutputDataWriter> {
        self.link_output_data_writer.clone_ref(python)
    }

    #[getter]
    fn gpu_limited_access(&self, python: Python<'_>) -> Py<PythonGpuContextLimitedAccess> {
        self.gpu_limited_access_context.clone_ref(python)
    }

    #[getter]
    fn gpu_full_access(&self, python: Python<'_>) -> Py<PythonGpuContextFullAccess> {
        self.gpu_full_access_context.clone_ref(python)
    }

    #[getter]
    fn runtime_id(&self) -> String {
        self.runtime_id.clone()
    }

    #[getter]
    fn processor_id(&self) -> String {
        self.processor_id.clone()
    }

    /// Whether this processor is currently paused.
    fn is_paused(&self) -> bool {
        self.pause_state_announced_by_parent.load(Ordering::Relaxed)
    }

    /// Whether processing should proceed (not paused).
    fn should_process(&self) -> bool {
        !self.pause_state_announced_by_parent.load(Ordering::Relaxed)
    }
}

/// Restricted runtime context passed to `process` / `on_pause` / `on_resume`.
///
/// `gpu_full_access` is deliberately absent — reaching for it raises
/// `AttributeError`, mirroring the Rust capability split.
#[pyclass(name = "RuntimeContextLimitedAccess", module = "streamlib", frozen)]
pub(crate) struct PythonRuntimeContextLimitedAccess {
    runtime_id: String,
    processor_id: String,
    configuration: serde_json::Value,
    link_input_data_reader: Py<PythonLinkInputDataReader>,
    link_output_data_writer: Py<PythonLinkOutputDataWriter>,
    gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    /// Shared with the full-access view this one was derived from.
    pause_state_announced_by_parent: Arc<AtomicBool>,
}

#[pymethods]
impl PythonRuntimeContextLimitedAccess {
    /// The processor's configuration, as the dict it was added with.
    #[getter]
    fn config<'py>(&self, python: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        configuration_as_python_dict(python, &self.configuration)
    }

    /// Current monotonic time in nanoseconds (raw `CLOCK_MONOTONIC`).
    #[getter]
    fn time(&self) -> u64 {
        monotonic_clock_now_ns()
    }

    #[getter]
    fn inputs(&self, python: Python<'_>) -> Py<PythonLinkInputDataReader> {
        self.link_input_data_reader.clone_ref(python)
    }

    #[getter]
    fn outputs(&self, python: Python<'_>) -> Py<PythonLinkOutputDataWriter> {
        self.link_output_data_writer.clone_ref(python)
    }

    #[getter]
    fn gpu_limited_access(&self, python: Python<'_>) -> Py<PythonGpuContextLimitedAccess> {
        self.gpu_limited_access_context.clone_ref(python)
    }

    #[getter]
    fn runtime_id(&self) -> String {
        self.runtime_id.clone()
    }

    #[getter]
    fn processor_id(&self) -> String {
        self.processor_id.clone()
    }

    /// Whether this processor is currently paused.
    fn is_paused(&self) -> bool {
        self.pause_state_announced_by_parent.load(Ordering::Relaxed)
    }

    /// Whether processing should proceed (not paused).
    fn should_process(&self) -> bool {
        !self.pause_state_announced_by_parent.load(Ordering::Relaxed)
    }
}

fn configuration_as_python_dict<'py>(
    python: Python<'py>,
    configuration: &serde_json::Value,
) -> PyResult<Bound<'py, PyAny>> {
    if configuration.is_null() {
        return Ok(PyDict::new(python).into_any());
    }
    json_value_to_python_object(python, configuration)
}

// =============================================================================
// Typed port access
// =============================================================================

/// A processor's input ports, as `ctx.inputs`.
#[pyclass(name = "LinkInputDataReader", module = "streamlib", frozen)]
pub(crate) struct PythonLinkInputDataReader {
    link_data_access: Py<PythonProcessorLinkDataAccess>,
}

#[pymethods]
impl PythonLinkInputDataReader {
    /// The next bag on `port_name`, or `None` when the mailbox is empty.
    fn read<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.link_data_access
            .get()
            .read_from_input_port(python, port_name)
    }

    /// The next bag with its stamp, or `(None, None)` when empty.
    fn read_with_timestamp<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
    ) -> PyResult<(Option<Bound<'py, PyAny>>, Option<i64>)> {
        self.link_data_access
            .get()
            .read_from_input_port_with_timestamp(python, port_name)
    }

    /// Whether a bag is waiting on `port_name`, without consuming it.
    fn has_data(&self, python: Python<'_>, port_name: &str) -> PyResult<bool> {
        self.link_data_access
            .get()
            .input_port_has_data(python, port_name)
    }
}

/// A processor's output ports, as `ctx.outputs`.
#[pyclass(name = "LinkOutputDataWriter", module = "streamlib", frozen)]
pub(crate) struct PythonLinkOutputDataWriter {
    link_data_access: Py<PythonProcessorLinkDataAccess>,
}

#[pymethods]
impl PythonLinkOutputDataWriter {
    /// Publish one bag to every downstream link on `port_name`.
    #[pyo3(signature = (port_name, bag, timestamp_ns = None))]
    fn write(
        &self,
        python: Python<'_>,
        port_name: &str,
        bag: &Bound<'_, PyAny>,
        timestamp_ns: Option<i64>,
    ) -> PyResult<()> {
        self.link_data_access
            .get()
            .write_to_output_port(python, port_name, bag, timestamp_ns)
    }
}

// =============================================================================
// Python-string <-> engine-enum format vocabularies
// =============================================================================

/// Parse a Python-facing format string, mapping the refusal into the
/// `ValueError` Python expects.
pub(crate) fn parse_pixel_format_name(name: &str) -> PyResult<PixelFormat> {
    PixelFormat::parse_wire_name(name).map_err(PyValueError::new_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every format string the old SDK accepted still parses, and every
    /// engine variant renders back to the string it parses from.
    #[test]
    fn pixel_format_names_round_trip() {
        for format in [
            PixelFormat::Bgra32,
            PixelFormat::Rgba32,
            PixelFormat::Argb32,
            PixelFormat::Rgba64,
            PixelFormat::Nv12VideoRange,
            PixelFormat::Nv12FullRange,
            PixelFormat::Uyvy422,
            PixelFormat::Yuyv422,
            PixelFormat::Gray8,
        ] {
            assert_eq!(parse_pixel_format_name(format.wire_name()).unwrap(), format);
        }
        assert_eq!(
            parse_pixel_format_name("bgra").unwrap(),
            PixelFormat::Bgra32,
            "the old SDK's default mnemonic must keep working"
        );
        assert!(parse_pixel_format_name("sepia").is_err());
    }
}
