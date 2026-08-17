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
use pyo3::types::{PyDict, PyList};
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
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
#[cfg(target_os = "linux")]
use crate::python_helper_process_pixel_exchange::{
    HelperAcquiredTexture, HelperCheckedOutPixelSurface, HelperSurfaceCheckOutLeaseDebt,
    HelperSurfaceReleaseDebt,
};
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
    /// Set when this handle names a device texture the engine allocated but
    /// whose memory was never mapped into this process. It holds the pool-slot
    /// debt directly, because there is no owned memory here to hold it.
    #[cfg(target_os = "linux")]
    device_texture_without_a_local_mapping: Mutex<Option<HelperSurfaceReleaseDebt>>,
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
            #[cfg(target_os = "linux")]
            device_texture_without_a_local_mapping: Mutex::new(None),
        }
    }

    /// A pooled device texture the parent acquired for this helper.
    ///
    /// It carries the name a kernel dispatch binds and a downstream processor
    /// resolves, and nothing else: the texture's memory is not mapped into
    /// this process, so every pixel accessor refuses by saying so.
    #[cfg(target_os = "linux")]
    fn from_helper_acquired_texture(acquired: HelperAcquiredTexture) -> Self {
        Self {
            minted_surface_id: Some(acquired.surface_id),
            surface_width: acquired.width,
            surface_height: acquired.height,
            surface_format_name: acquired.format_name,
            owned_memory: Mutex::new(None),
            cpu_access: CpuAccessGate::new_unlocked(),
            device_write_pending: std::sync::atomic::AtomicBool::new(false),
            natural_dlpack_side_is_device: std::sync::OnceLock::new(),
            device_texture_without_a_local_mapping: Mutex::new(Some(acquired.release_to_parent)),
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
        #[cfg(target_os = "linux")]
        {
            let released_slot = self.device_texture_without_a_local_mapping.lock().take();
            drop(released_slot);
        }
    }

    /// Borrow the shared memory anchor, or fail if the handle is closed.
    fn owned_memory(&self) -> PyResult<Arc<GpuSurfaceOwnedMemory>> {
        #[cfg(target_os = "linux")]
        if self.device_texture_without_a_local_mapping.lock().is_some() {
            return Err(PyRuntimeError::new_err(
                "this surface is a device texture whose memory is not mapped into this process: \
                 its pixels are reachable to a kernel dispatch, which binds it by surface id, \
                 not to this process directly",
            ));
        }
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
// Surface checkout lease
// =============================================================================

/// A claim on a published surface, held for exactly as long as this object is.
///
/// While a claim is outstanding the pool never rehands that surface's slot to
/// its producer, and dropping this object is the release — there is nothing to
/// call. Ownership being the whole protocol is what lets any object that holds
/// one in a field inherit the behaviour: the frame stops moving while the
/// object that named it lives.
///
/// Claims are counted, so holding one and resolving the same surface for its
/// pixels are independent — neither releases the other's.
#[pyclass(name = "GpuSurfaceCheckOutLease", module = "streamlib", frozen)]
pub(crate) struct PythonGpuSurfaceCheckOutLease {
    claimed_surface_id: String,
    /// Settled by its own `Drop`; nothing reads it, and that is the point.
    #[cfg(target_os = "linux")]
    #[expect(dead_code, reason = "the field is the claim; its Drop is the release")]
    release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt,
}

#[pymethods]
impl PythonGpuSurfaceCheckOutLease {
    /// The surface this claim holds still.
    #[getter]
    fn surface_id(&self) -> String {
        self.claimed_surface_id.clone()
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

    /// Acquire a pooled device texture, named by the surface id the engine
    /// minted for it.
    ///
    /// The id is the whole handle: a kernel dispatch binds it, and a
    /// downstream processor resolves it. The texture's memory is not mapped
    /// into this process, so its pixels are not addressable here.
    fn acquire_texture(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        format: &str,
        usage: Vec<String>,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let texture_format = parse_texture_format_name(format)?;
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let acquired =
                exchange_client.acquire_texture(python, width, height, texture_format, &usage)?;
            return Ok(PythonGpuSurfaceHandle::from_helper_acquired_texture(
                acquired,
            ));
        }
        let _ = (python, width, height, texture_format, usage);
        Err(gpu_unreachable_from_a_helper_process_error())
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

    /// Claim a published surface against producer reuse until the returned
    /// lease is dropped.
    ///
    /// The cheap half of [`resolve_surface`]: it holds the frame still without
    /// importing its memory, so an object that wants only the pixels it was
    /// handed to stay put can keep the lease in a field and let its own
    /// lifetime do the releasing.
    ///
    /// [`resolve_surface`]: PythonGpuContextLimitedAccess::resolve_surface
    fn claim_surface_against_producer_reuse(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<PythonGpuSurfaceCheckOutLease> {
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let claimed = python
                .detach(|| exchange_client.claim_surface_against_producer_reuse(surface_id))?;
            return Ok(PythonGpuSurfaceCheckOutLease {
                claimed_surface_id: surface_id.to_string(),
                release_check_out_to_surface_share: claimed,
            });
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

    /// Acquire a pooled device texture through the privileged path.
    fn acquire_texture(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        format: &str,
        usage: Vec<String>,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let texture_format = parse_texture_format_name(format)?;
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let acquired =
                exchange_client.acquire_texture(python, width, height, texture_format, &usage)?;
            return Ok(PythonGpuSurfaceHandle::from_helper_acquired_texture(
                acquired,
            ));
        }
        let _ = (python, width, height, texture_format, usage);
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Build a compute kernel from GLSL source, or from pre-compiled SPIR-V.
    ///
    /// Constructed once in `setup()`, dispatched per frame in `process()`.
    /// The engine compiles `source` and reflects the shader at construction,
    /// taking its binding names from it — those names are what `dispatch`
    /// resolves against. Re-creating an identical kernel is free of
    /// compilation.
    #[pyo3(signature = (source = None, spirv = None, push_constant_size = 0, bindings = None, entry_point = "main"))]
    fn create_compute_kernel(
        &self,
        python: Python<'_>,
        source: Option<&str>,
        spirv: Option<&[u8]>,
        push_constant_size: u32,
        bindings: Option<&Bound<'_, PyDict>>,
        entry_point: &str,
    ) -> PyResult<PythonComputeKernel> {
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let declared = declared_compute_bindings_to_wire(python, bindings)?;
            // Neither and both are refused engine-side, in the one place the
            // rule is written; forwarding both fields keeps the wheel from
            // becoming a second spelling of it that can drift.
            let spirv_hex = spirv.map(encode_lowercase_hex).unwrap_or_default();
            let (kernel_id, reflected_binding_kinds) = exchange_client.register_compute_kernel(
                python,
                source.unwrap_or_default(),
                &spirv_hex,
                entry_point,
                push_constant_size,
                declared.as_any(),
            )?;
            return Ok(PythonComputeKernel {
                kernel_id,
                push_constant_size,
                reflected_binding_kinds,
                helper_process_exchange_client: Arc::clone(exchange_client),
            });
        }
        let _ = (
            python,
            source,
            spirv,
            push_constant_size,
            bindings,
            entry_point,
        );
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Open a scope that records several dispatches and runs them as one.
    ///
    /// The Python equivalent of the engine's command-recorder flow, and the
    /// reason dispatch has two entry points in both languages: `kernel.dispatch()`
    /// for a single pass, this for several. Multi-pass work costs one round
    /// trip, one submission and one stall instead of N of each; leaving the
    /// scope returns with every write visible, same as a single dispatch.
    fn kernel_dispatch_batch(&self) -> PythonKernelDispatchBatch {
        PythonKernelDispatchBatch {
            helper_process_exchange_client: self
                .helper_process_exchange_client
                .as_ref()
                .map(Arc::clone),
            recording: Mutex::default(),
        }
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
        // Built before the reader, which carries it: a typed read offers this
        // very capability to whatever it constructs.
        let gpu_limited_access_context = Py::new(
            python,
            PythonGpuContextLimitedAccess::new_for_helper_process(
                helper_process_exchange_client.clone(),
            ),
        )?;
        Ok(Self {
            runtime_id,
            processor_id,
            configuration: python_object_to_json_value(configuration)?,
            link_input_data_reader: Py::new(
                python,
                PythonLinkInputDataReader {
                    link_data_access: link_data_access.clone_ref(python),
                    gpu_limited_access_context: gpu_limited_access_context.clone_ref(python),
                },
            )?,
            link_output_data_writer: Py::new(
                python,
                PythonLinkOutputDataWriter {
                    link_data_access: link_data_access.clone_ref(python),
                },
            )?,
            gpu_limited_access_context,
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
///
/// It carries the same GPU capability the context exposes as
/// `ctx.gpu_limited_access` because this is where the two knowledges meet: the
/// consumer names the type it is reading into, and the context holds the route
/// to the engine's surfaces.
#[pyclass(name = "LinkInputDataReader", module = "streamlib", frozen)]
pub(crate) struct PythonLinkInputDataReader {
    link_data_access: Py<PythonProcessorLinkDataAccess>,
    gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
}

#[pymethods]
impl PythonLinkInputDataReader {
    /// The next bag on `port_name`, or `None` when the mailbox is empty.
    ///
    /// `into` is the opt-in strictness dial: a TypedDict casts for free, a
    /// dataclass or pydantic model constructs and validates, and a bag that
    /// does not fit raises here rather than travelling on.
    ///
    /// A constructing target is offered this processor's GPU capability while
    /// it builds — see `gpu_limited_access_of_the_typed_read_in_progress`.
    #[pyo3(signature = (port_name, *, into = None))]
    fn read<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.link_data_access
            .get()
            .read_from_input_port_offering_gpu_access(
                python,
                port_name,
                into,
                Some(self.gpu_limited_access_context.bind(python)),
            )
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

/// Texture formats travel to the parent as their wire spelling, which the host
/// parses. Validating the spelling here keeps the refusal on the caller's own
/// stack rather than arriving as an escalate failure.
const TEXTURE_FORMAT_WIRE_NAMES: &[&str] = &[
    "bgra8_unorm",
    "bgra8_unorm_srgb",
    "r8_unorm",
    "rg8_unorm",
    "rgba8_unorm",
    "rgba8_unorm_srgb",
    "rgba16_float",
    "rgba32_float",
];

fn parse_texture_format_name(name: &str) -> PyResult<&'static str> {
    TEXTURE_FORMAT_WIRE_NAMES
        .iter()
        .find(|known| **known == name)
        .copied()
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown texture format {name:?}; the formats a texture can be acquired in are \
                 {}",
                TEXTURE_FORMAT_WIRE_NAMES.join(", ")
            ))
        })
}

/// Lowercase hex, no `0x`, no separators — the encoding every escalate blob
/// field uses.
#[cfg(target_os = "linux")]
fn encode_lowercase_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut encoded, byte| {
            let _ = write!(encoded, "{byte:02x}");
            encoded
        },
    )
}

/// The binding kinds the wire spells, validated the same way texture formats
/// are so the error text cannot drift from the accepted set.
#[cfg(target_os = "linux")]
const COMPUTE_BINDING_KIND_WIRE_NAMES: &[&str] = &[
    "sampled_image",
    "sampled_texture",
    "storage_buffer",
    "storage_image",
    "uniform_buffer",
];

#[cfg(target_os = "linux")]
fn parse_compute_binding_kind(kind: &str) -> PyResult<&'static str> {
    COMPUTE_BINDING_KIND_WIRE_NAMES
        .iter()
        .find(|known| **known == kind)
        .copied()
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown binding kind {kind:?}; a compute binding is one of {}",
                COMPUTE_BINDING_KIND_WIRE_NAMES.join(", ")
            ))
        })
}

/// Turn `{name: kind}` into the wire's declaration array.
#[cfg(target_os = "linux")]
fn declared_compute_bindings_to_wire<'py>(
    python: Python<'py>,
    declared: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyList>> {
    let wire = PyList::empty(python);
    if let Some(declared) = declared {
        for (name, kind) in declared.iter() {
            let name: String = name.extract()?;
            let kind: String = kind.extract()?;
            let entry = PyDict::new(python);
            entry.set_item("name", name)?;
            entry.set_item("kind", parse_compute_binding_kind(&kind)?)?;
            wire.append(entry)?;
        }
    }
    Ok(wire)
}

/// One binding of a registered kernel as reflection found it: the shader's
/// name and the wire spelling of its kind.
pub(crate) struct ReflectedComputeBinding {
    pub(crate) name: String,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    pub(crate) kind: String,
}

/// A compute kernel the engine built and holds, dispatched by name.
///
/// Constructed in `setup()` where the capability is Full; dispatched per frame
/// in `process()`. No kernel handle string, fence, timeline or slot number
/// reaches Python — the object is the handle.
///
/// Defined on every platform so the stub's surface is honest everywhere;
/// off Linux it is unconstructible, because `create_compute_kernel` refuses
/// before reaching it.
#[pyclass(name = "ComputeKernel", module = "streamlib", frozen)]
pub(crate) struct PythonComputeKernel {
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    kernel_id: String,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    push_constant_size: u32,
    /// The caller supplies surfaces by name; which kind each name is, is the
    /// shader's to say, so it is carried rather than guessed per dispatch.
    reflected_binding_kinds: Vec<ReflectedComputeBinding>,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    helper_process_exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

#[pymethods]
impl PythonComputeKernel {
    /// The shader's own names for this kernel's bindings, in slot order.
    #[getter]
    fn binding_names(&self) -> Vec<String> {
        self.reflected_binding_kinds
            .iter()
            .map(|binding| binding.name.clone())
            .collect()
    }

    /// Dispatch this kernel, binding each of the shader's declared resources
    /// by name.
    ///
    /// Bindings never persist on the kernel, so every dispatch supplies all of
    /// them: there is no implicit default and no value carried over from the
    /// previous frame. Returns when the GPU work has retired and the writes
    /// are visible.
    #[pyo3(signature = (bindings, group_count, push_constants = None))]
    fn dispatch(
        &self,
        python: Python<'_>,
        bindings: &Bound<'_, PyDict>,
        group_count: (u32, u32, u32),
        push_constants: Option<&[u8]>,
    ) -> PyResult<()> {
        #[cfg(target_os = "linux")]
        {
            let (wire_bindings, push_constants_hex) =
                self.validated_wire_dispatch(python, bindings, push_constants)?;
            self.helper_process_exchange_client.run_compute_kernel(
                python,
                &self.kernel_id,
                wire_bindings.as_any(),
                &push_constants_hex,
                group_count,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (python, bindings, group_count, push_constants);
            Err(gpu_unreachable_from_a_helper_process_error())
        }
    }
}

impl PythonComputeKernel {
    /// This dispatch's bindings as the wire carries them, plus its
    /// hex-encoded push constants.
    ///
    /// Shared by the two entry points a dispatch has — on its own, and inside
    /// a batch — so a mistake is refused identically either way, in the
    /// caller's own stack rather than a round trip later.
    #[cfg(target_os = "linux")]
    fn validated_wire_dispatch<'py>(
        &self,
        python: Python<'py>,
        bindings: &Bound<'py, PyDict>,
        push_constants: Option<&[u8]>,
    ) -> PyResult<(Bound<'py, PyList>, String)> {
        let push_constants = push_constants.unwrap_or_default();
        if push_constants.len() != self.push_constant_size as usize {
            return Err(PyValueError::new_err(format!(
                "this kernel declares {} push-constant bytes but {} were supplied",
                self.push_constant_size,
                push_constants.len()
            )));
        }

        let wire_bindings = PyList::empty(python);
        for (name, bound_to) in bindings.iter() {
            let name: String = name.extract()?;
            let kind = self.reflected_kind_of(&name)?.to_string();
            let entry = PyDict::new(python);
            entry.set_item("target_id", bound_surface_id(&name, &bound_to)?)?;
            entry.set_item("name", name)?;
            entry.set_item("kind", kind)?;
            wire_bindings.append(entry)?;
        }
        Ok((wire_bindings, encode_lowercase_hex(push_constants)))
    }

    /// The kind the shader declares this name as.
    ///
    /// An unknown name is refused here rather than sent — the round trip would
    /// refuse it too, but the caller's own stack is where the mistake is.
    #[cfg(target_os = "linux")]
    fn reflected_kind_of(&self, name: &str) -> PyResult<&str> {
        self.reflected_binding_kinds
            .iter()
            .find(|binding| binding.name == name)
            .map(|binding| binding.kind.as_str())
            .ok_or_else(|| {
                PyValueError::new_err(format!(
                    "no binding named {name:?}; this shader declares {}",
                    self.binding_names()
                        .iter()
                        .map(|declared| format!("{declared:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
    }
}

/// One recorded dispatch: the wire entry to send, and the kernel it names.
struct RecordedKernelDispatch {
    /// Kept beside the entry rather than read back out of it, so refusing a
    /// repeated kernel cannot drift from the entry it refuses against.
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    kernel_id: String,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    wire_entry: Py<PyDict>,
}

/// What a batch has accumulated, and where its scope stands.
#[derive(Default)]
struct KernelDispatchBatchRecording {
    /// One entry per `dispatch()`, in the order they will run.
    dispatches: Vec<RecordedKernelDispatch>,
    /// Set by `__enter__`. Dispatching into a batch that was never entered
    /// would accumulate work no `__exit__` will ever send — the silently
    /// discarded GPU work the ADR rejected an explicit `publish()` over.
    entered: bool,
    /// Set on leaving the scope, however it was left. A batch is not
    /// reusable: the dispatches it holds have already run.
    closed: bool,
}

/// Several dispatches recorded as one: one submission, one stall.
///
/// A two-pass filter dispatching on its own pays the round trip, the
/// submission and the fence wait twice; inside this scope it pays each once.
/// Leaving the scope normally runs the batch — leaving it by a raise runs
/// nothing, because half of a multi-pass filter is not what the author wrote,
/// and publishing a half-processed frame surfaces as corrupt pixels somewhere
/// downstream rather than at the `raise`.
///
/// Nothing about the synchronous contract changes: the scope returns when the
/// GPU work has retired and the writes are visible, and no fence or timeline
/// value reaches Python.
#[pyclass(name = "KernelDispatchBatch", module = "streamlib", frozen)]
pub(crate) struct PythonKernelDispatchBatch {
    /// `None` means this helper was started without its GPU channels.
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    helper_process_exchange_client: Option<Arc<HelperProcessGpuExchangeClient>>,
    recording: Mutex<KernelDispatchBatchRecording>,
}

#[pymethods]
impl PythonKernelDispatchBatch {
    fn __enter__(python_self: PyRef<'_, Self>) -> PyRef<'_, Self> {
        python_self.recording.lock().entered = true;
        python_self
    }

    /// Run everything recorded, unless the block was left by a raise.
    ///
    /// Returns `False` always: discarding the batch never suppresses the
    /// exception that discarded it.
    #[pyo3(signature = (exception_type = None, exception = None, traceback = None))]
    fn __exit__(
        &self,
        python: Python<'_>,
        exception_type: Option<&Bound<'_, PyAny>>,
        exception: Option<&Bound<'_, PyAny>>,
        traceback: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        let _ = (exception, traceback);
        let left_by_a_raise = exception_type.is_some_and(|raised| !raised.is_none());
        let recorded = {
            let mut recording = self.recording.lock();
            recording.closed = true;
            std::mem::take(&mut recording.dispatches)
        };
        if left_by_a_raise || recorded.is_empty() {
            return Ok(false);
        }
        self.run(python, recorded)?;
        Ok(false)
    }

    /// Add a dispatch to this batch.
    ///
    /// The receiver is explicit because a batch dispatches several kernels;
    /// `kernel.dispatch(...)` names its own. Bindings are checked here, so a
    /// name the shader does not declare or a wrong push-constant size refuses
    /// at this line rather than when the scope closes.
    ///
    /// One kernel may appear only once per batch: a kernel owns a single
    /// descriptor set, so binding it again would hand its earlier dispatch
    /// these bindings.
    #[pyo3(signature = (kernel, bindings, group_count, push_constants = None))]
    fn dispatch(
        &self,
        python: Python<'_>,
        kernel: &PythonComputeKernel,
        bindings: &Bound<'_, PyDict>,
        group_count: (u32, u32, u32),
        push_constants: Option<&[u8]>,
    ) -> PyResult<()> {
        #[cfg(target_os = "linux")]
        {
            // Scope state first, and the lock dropped before validation: a
            // batch nobody entered or already ran collects nothing, so a
            // binding mistake must not mask either — and validation calls back
            // into the interpreter over caller-supplied objects, which is not
            // something to do holding a non-reentrant lock.
            {
                let recording = self.recording.lock();
                if recording.closed {
                    return Err(PyRuntimeError::new_err(
                        "this batch has already run; open a new `kernel_dispatch_batch()` \
                         scope for the next one",
                    ));
                }
                if !recording.entered {
                    return Err(PyRuntimeError::new_err(
                        "this batch was never entered, so nothing would ever run it; use it \
                         as `with ctx.gpu_full_access.kernel_dispatch_batch() as batch:`",
                    ));
                }
            }

            let (wire_bindings, push_constants_hex) =
                kernel.validated_wire_dispatch(python, bindings, push_constants)?;

            let mut recording = self.recording.lock();
            // Re-checked, not assumed from the first look: validating the
            // bindings ran user code, and CPython can switch threads inside
            // it, so another thread may have left the scope and taken the
            // recorded dispatches meanwhile. Pushing onto a spent batch would
            // return Ok for work that never reaches the GPU.
            if recording.closed {
                return Err(PyRuntimeError::new_err(
                    "this batch has already run; open a new `kernel_dispatch_batch()` scope \
                     for the next one",
                ));
            }
            if let Some(earlier) = recording
                .dispatches
                .iter()
                .position(|already| already.kernel_id == kernel.kernel_id)
            {
                return Err(PyValueError::new_err(format!(
                    "this kernel is already dispatch {earlier} of this batch; a kernel owns \
                     one descriptor set, so dispatching it again here would give dispatch \
                     {earlier} these bindings. Build a second kernel, or use a second batch"
                )));
            }

            let wire_entry =
                crate::python_helper_process_pixel_exchange::compute_dispatch_wire_entry(
                    python,
                    &kernel.kernel_id,
                    wire_bindings.as_any(),
                    &push_constants_hex,
                    group_count,
                )?;
            recording.dispatches.push(RecordedKernelDispatch {
                kernel_id: kernel.kernel_id.clone(),
                wire_entry: wire_entry.unbind(),
            });
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (python, kernel, bindings, group_count, push_constants);
            Err(gpu_unreachable_from_a_helper_process_error())
        }
    }
}

impl PythonKernelDispatchBatch {
    /// Send everything recorded as one op.
    fn run(&self, python: Python<'_>, recorded: Vec<RecordedKernelDispatch>) -> PyResult<()> {
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let dispatches = PyList::new(
                python,
                recorded.iter().map(|entry| entry.wire_entry.bind(python)),
            )?;
            return exchange_client.run_compute_kernel_batch(python, dispatches.as_any());
        }
        let _ = (python, recorded);
        Err(gpu_unreachable_from_a_helper_process_error())
    }
}

/// The surface id a value bound at `name` names.
#[cfg(target_os = "linux")]
fn bound_surface_id(name: &str, bound_to: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Ok(handle) = bound_to.extract::<PyRef<'_, PythonGpuSurfaceHandle>>() {
        return handle.surface_id();
    }
    bound_to.extract::<String>().map_err(|_| {
        PyTypeError::new_err(format!(
            "binding {name:?} must be a GpuSurfaceHandle or a surface id string"
        ))
    })
}

/// The typed cast's claim, over a real link and a real surface-share service.
///
/// What is proven here is the seam, not a type: a bag crosses a wired link, the
/// read constructs a frame class **the wheel does not ship**, and that class
/// pins its surface for exactly as long as it lives. If this only worked for
/// `VideoFrame` the pattern would be a private handshake, so the target here is
/// deliberately somebody else's.
#[cfg(all(test, target_os = "linux"))]
mod typed_read_claim_tests {
    use super::*;
    use crate::python_bag_conversion::gpu_limited_access_of_the_typed_read_in_progress;
    use crate::python_class_from_source_for_tests::class_from_source_in_namespace;
    use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
    use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;
    use pyo3::types::{IntoPyDict, PyDict};

    const OUTPUT_PORT: &str = "frames_to_downstream";
    const INPUT_PORT: &str = "frames_from_upstream";

    /// A frame class somebody else could write today, using only what the
    /// wheel exports: it asks the read in progress for the GPU capability and
    /// keeps the claim in a field. Nothing marks it, and nothing registers it.
    const FRAME_CLASS_THE_WHEEL_DOES_NOT_SHIP: &str = "\
class FrameSomebodyElseWrote:
    def __init__(self, surface_id, **rest_of_the_bag):
        self.surface_id = surface_id
        gpu_limited_access = gpu_limited_access_of_the_typed_read_in_progress()
        self.claim = (
            None
            if gpu_limited_access is None
            else gpu_limited_access.claim_surface_against_producer_reuse(surface_id)
        )
";

    /// One link, wired to itself, plus a reader carrying a capability that
    /// reaches `share`.
    ///
    /// The destination subscribes first: iceoryx2 drops a send with no
    /// subscriber attached. Both planes live on the caller's thread because
    /// iceoryx2's ports are `!Send`.
    struct ReadUnderTest {
        source: Py<PythonProcessorLinkDataAccess>,
        reader: PythonLinkInputDataReader,
    }

    /// A data plane the way a helper process builds one — through its own
    /// constructor, which is where its iceoryx2 node comes from.
    fn helper_process_data_plane(python: Python<'_>) -> Py<PythonProcessorLinkDataAccess> {
        python
            .get_type::<PythonProcessorLinkDataAccess>()
            .call0()
            .unwrap()
            .cast_into::<PythonProcessorLinkDataAccess>()
            .unwrap()
            .unbind()
    }

    fn wire_one_link_into_a_reader(
        python: Python<'_>,
        label: &str,
        share: &SurfaceShareUnderTest,
    ) -> ReadUnderTest {
        let unique = format!("castclaim{}_{label}", std::process::id());
        let channel_service_name = format!("{unique}/frames");
        let notify_service_name = format!("{unique}_dest/notify");
        let link_id = format!("L-{unique}");

        let destination = helper_process_data_plane(python);
        destination
            .bind(python)
            .call_method1(
                "wire_input_link",
                (
                    INPUT_PORT,
                    &channel_service_name,
                    &notify_service_name,
                    "read_next_in_order",
                    8,
                    2,
                    1,
                    true,
                    &link_id,
                ),
            )
            .unwrap();
        let source = helper_process_data_plane(python);
        source
            .bind(python)
            .call_method1(
                "wire_output_link",
                (
                    OUTPUT_PORT,
                    &channel_service_name,
                    &notify_service_name,
                    1024,
                    1 << 20,
                    8,
                    2,
                    1,
                    true,
                    &link_id,
                ),
            )
            .unwrap();

        // The capability a helper's context carries: the escalate callable is
        // never reached, because a claim speaks only to the surface socket.
        let exchange_client = Arc::new(HelperProcessGpuExchangeClient::new(
            python.None(),
            share.socket_path.clone(),
        ));
        ReadUnderTest {
            source,
            reader: PythonLinkInputDataReader {
                link_data_access: destination,
                gpu_limited_access_context: Py::new(
                    python,
                    PythonGpuContextLimitedAccess::new_for_helper_process(Some(exchange_client)),
                )
                .unwrap(),
            },
        }
    }

    fn publish_a_frame_bag(python: Python<'_>, link: &ReadUnderTest, surface_id: &str) {
        let bag = PyDict::new(python);
        bag.set_item("surface_id", surface_id).unwrap();
        bag.set_item("width", 32i64).unwrap();
        bag.set_item("height", 32i64).unwrap();
        bag.set_item("timestamp_ns", 1i64).unwrap();
        link.source
            .bind(python)
            .call_method1("write_to_output_port", (OUTPUT_PORT, &bag))
            .unwrap();
    }

    fn frame_class<'py>(python: Python<'py>) -> Bound<'py, PyAny> {
        let namespace = PyDict::new(python);
        namespace
            .set_item(
                "gpu_limited_access_of_the_typed_read_in_progress",
                wrap_pyfunction!(gpu_limited_access_of_the_typed_read_in_progress, python).unwrap(),
            )
            .unwrap();
        class_from_source_in_namespace(
            python,
            FRAME_CLASS_THE_WHEEL_DOES_NOT_SHIP,
            "FrameSomebodyElseWrote",
            &namespace,
        )
    }

    /// The whole contract in one test: the cast claims the frame, the frame's
    /// existence is what holds the claim, and letting the frame go is what
    /// returns the slot to its producer. Nothing is called to release it.
    #[test]
    fn a_frame_read_into_a_type_pins_its_surface_until_the_object_goes_away() {
        let share = SurfaceShareUnderTest::start("typed-read");
        let surface_id = share.publish_one_surface();

        Python::initialize();
        Python::attach(|python| {
            let link = wire_one_link_into_a_reader(python, "held", &share);
            publish_a_frame_bag(python, &link, &surface_id);

            let frame = link
                .reader
                .read(python, INPUT_PORT, Some(&frame_class(python)))
                .expect("the read")
                .expect("the wired input received nothing");
            assert!(
                !frame.getattr("claim").unwrap().is_none(),
                "the read must offer the constructing type a way to claim"
            );
            assert_eq!(
                share.outstanding_claims_on(&surface_id),
                1,
                "a frame the consumer is holding must not be rehanded to its producer"
            );

            drop(frame);
            assert_eq!(
                share.outstanding_claims_on(&surface_id),
                0,
                "the claim releases with the object, without anything being called"
            );
        });
    }

    /// The offer is the read's, not the thread's: the same class constructed
    /// outside a read claims nothing, which is what keeps a hand-rolled bag —
    /// possibly naming no live surface at all — an ordinary construction.
    #[test]
    fn the_same_class_constructed_outside_a_read_claims_nothing() {
        let share = SurfaceShareUnderTest::start("outside");
        let surface_id = share.publish_one_surface();

        Python::initialize();
        Python::attach(|python| {
            let link = wire_one_link_into_a_reader(python, "outside", &share);
            let frame_class = frame_class(python);

            // Once through a read, so the offer has been opened on this thread
            // at least once — a stale offer would show up here.
            publish_a_frame_bag(python, &link, &surface_id);
            let frame = link
                .reader
                .read(python, INPUT_PORT, Some(&frame_class))
                .unwrap()
                .unwrap();
            drop(frame);

            let bag = PyDict::new(python);
            bag.set_item("surface_id", &surface_id).unwrap();
            let built_by_hand = frame_class.call((), Some(&bag)).unwrap();
            assert!(
                built_by_hand.getattr("claim").unwrap().is_none(),
                "construction outside a read is offered nothing"
            );
            assert_eq!(
                share.outstanding_claims_on(&surface_id),
                0,
                "nothing outside a read may claim a producer's slot"
            );
        });
    }

    /// The bare data plane a helper wires by hand holds no context, so a type
    /// it constructs is offered nothing — the claim belongs to the read a
    /// processor actually writes.
    #[test]
    fn the_context_free_data_plane_offers_no_capability() {
        let share = SurfaceShareUnderTest::start("contextfree");
        let surface_id = share.publish_one_surface();

        Python::initialize();
        Python::attach(|python| {
            let link = wire_one_link_into_a_reader(python, "contextfree", &share);
            publish_a_frame_bag(python, &link, &surface_id);

            let frame = link
                .reader
                .link_data_access
                .bind(python)
                .call_method(
                    "read_from_input_port",
                    (INPUT_PORT,),
                    Some(
                        &[("into", frame_class(python))]
                            .into_py_dict(python)
                            .unwrap(),
                    ),
                )
                .expect("the read");
            assert!(
                frame.getattr("claim").unwrap().is_none(),
                "a read with no context has no capability to offer"
            );
            assert_eq!(share.outstanding_claims_on(&surface_id), 0);
        });
    }
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
