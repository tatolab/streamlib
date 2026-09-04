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
use pyo3::types::{PyBytes, PyDict, PyList};
use streamlib::sdk::rhi::PixelFormat;
use streamlib_adapter_cuda::dlpack::DeviceType;

use crate::python_bag_conversion::{json_value_to_python_object, python_object_to_json_value};
use crate::python_gpu_surface_pixel_exchange::{
    CpuAccessGate, GpuSurfaceOwnedMemory, HOST_VISIBLE_DLPACK_DEVICE, device_export_available,
    exchange_shape_for_max_version, host_visible_dlpack_capsule,
};
#[cfg(target_os = "linux")]
use crate::python_gpu_surface_pixel_exchange::{
    PreparedDeviceExport, StagedWriteBackSource, device_dlpack_capsule, imported_device_for,
    map_the_cpu_staging_without_reading_a_frame_in, prepare_device_export,
    read_the_frame_into_its_cpu_staging,
};
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
#[cfg(target_os = "linux")]
use crate::python_helper_process_pixel_exchange::{
    HelperAcquiredTexture, HelperCheckedOutSurface, HelperProcessGraphicsDraw,
    HelperProcessGraphicsKernelRegistration, HelperProcessRayTracingKernelRegistration,
    HelperSurfaceCheckOutLeaseDebt,
};
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;
use crate::python_processor_owned_window::PythonProcessorOwnedWindow;

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

/// Whether a context manager's `__exit__` was reached by a propagating
/// exception. pyo3 maps Python's `None` to the `None` variant for an
/// `Option` parameter, so `is_some()` is the whole test — spelled once,
/// because a reader should not have to learn pyo3's argument mapping at
/// three call sites.
fn left_by_a_propagating_exception(exception_type: Option<&Bound<'_, PyAny>>) -> bool {
    exception_type.is_some()
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
    /// Whether the readback staging already holds this lock scope's
    /// frame, so the host-side accessors read it in once between `lock()`
    /// and `unlock()` rather than per call.
    ///
    /// Only ever set for a surface the CPU cannot address directly; a
    /// coherent mapping has nothing to read in.
    #[cfg(target_os = "linux")]
    cpu_staging_holds_this_locks_frame: std::sync::atomic::AtomicBool,
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
            cpu_staging_holds_this_locks_frame: std::sync::atomic::AtomicBool::new(false),
            #[cfg(target_os = "linux")]
            natural_dlpack_side_is_device: std::sync::OnceLock::new(),
        }
    }

    /// Open the staged CPU door over this frame, once per lock scope.
    ///
    /// A surface the CPU can already address does nothing — its own
    /// coherent mapping *is* the door. Everything else checks the
    /// engine's host-visible staging out, maps it, and reads this frame's
    /// pixels in; a write lock over a frame that takes an edit also arms
    /// the publish, so the block edge has something to settle.
    ///
    /// Called from the host-side accessors rather than from `lock()`
    /// itself: `lock()` also gates the device side, and reading a frame
    /// into a host staging nobody asked for would cost the device path a
    /// copy per frame.
    #[cfg(target_os = "linux")]
    fn open_the_staged_cpu_door_over_this_frame(
        &self,
        python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> PyResult<()> {
        if !owned_memory.cpu_reach_goes_through_the_export_staging() {
            return Ok(());
        }
        if self
            .cpu_staging_holds_this_locks_frame
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        let opened = (|| -> PyResult<()> {
            if self.cpu_access.is_read_only() {
                read_the_frame_into_its_cpu_staging(python, owned_memory)?;
                return Ok(());
            }
            // Refused before the copy, not after: a scope that already
            // staged an edit through the other door cannot take this one,
            // and finding that out is not worth a round trip and a frame
            // copy first. Nothing is discarded on this path — the arm that
            // refused belongs to the other door.
            owned_memory
                .pending_staged_write_back()
                .arm(StagedWriteBackSource::CpuReadbackStaging)?;
            // Past the arm it is this scope's to settle, so every way out
            // from here drops it: a publish over a staging this scope never
            // filled would copy some earlier frame over the surface.
            let staged = read_the_frame_into_its_cpu_staging(python, owned_memory)
                .inspect_err(|_| owned_memory.pending_staged_write_back().discard())?;
            if !staged.writable {
                // Refused rather than downgraded: nothing here can make the
                // capsule read-only — `__dlpack__` derives that from the
                // lock, which said write — so going on would hand out a
                // writable array whose stores publish nowhere and vanish
                // without an error. The plan's own answer for a texture
                // that cannot take the copy is to refuse the door by name.
                owned_memory.pending_staged_write_back().discard();
                return Err(PyRuntimeError::new_err(format!(
                    "surface {:?} cannot take a write-back, so a write lock over it would hand \
                     out an array whose edits reach no other holder: it is a pooled frame its \
                     producer still owns, or a registered texture without the transfer usage to \
                     take a copy in. Lock it read-only to read these pixels",
                    owned_memory.surface_id_for_a_refusal(),
                )));
            }
            Ok(())
        })();
        if opened.is_err() {
            // The read-in is what makes a later publish legal, so a door
            // that failed to open must not look open to the next accessor.
            self.cpu_staging_holds_this_locks_frame
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        opened
    }

    /// The id and pixel extent a window's `show()` names this surface by.
    ///
    /// Refuses a handle carrying no id for the same reason the getter does:
    /// nothing outside this process can resolve one, a present loop least of
    /// all.
    pub(crate) fn surface_id_and_extent_a_window_can_name(&self) -> PyResult<(String, u32, u32)> {
        Ok((self.surface_id()?, self.surface_width, self.surface_height))
    }

    /// A pooled device texture the parent acquired for this helper.
    ///
    /// It carries the name a kernel dispatch binds and a downstream
    /// processor resolves — the texture's memory is not mapped into this
    /// process, so the CPU accessors reach it over the engine's
    /// host-visible staging instead — and the owned-memory anchor its
    /// device-tensor scope and release debt ride, so a tensor outliving the
    /// handle keeps the pool slot alive.
    #[cfg(target_os = "linux")]
    fn from_helper_acquired_texture(acquired: HelperAcquiredTexture) -> Self {
        let surface_id = acquired.surface_id.clone();
        let (width, height) = (acquired.width, acquired.height);
        let format_wire_name = acquired.format.wire_name().to_string();
        Self::new(
            Some(surface_id.clone()),
            width,
            height,
            format_wire_name,
            GpuSurfaceOwnedMemory::new(
                HelperCheckedOutSurface::AcquiredDeviceTexture(acquired),
                Some(surface_id),
            ),
        )
    }

    /// A surface a helper process checked out of its parent — pixel buffer
    /// or texture, whichever the registration named — behind the same handle
    /// surface the engine path mints.
    #[cfg(target_os = "linux")]
    fn from_helper_checked_out_surface(checked_out: HelperCheckedOutSurface) -> Self {
        let surface_id = checked_out.surface_id().to_string();
        let (width, height) = (checked_out.width(), checked_out.height());
        let format_wire_name = checked_out.format_wire_name().to_string();
        Self::new(
            Some(surface_id.clone()),
            width,
            height,
            format_wire_name,
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

    /// Publish a pending staged write, once, through whichever staging
    /// holds the edit. Shared by `unlock` and `close` so the
    /// context-manager spelling cannot silently drop an edit; a handle
    /// already closed has nothing to publish into and discards instead.
    #[cfg(target_os = "linux")]
    fn publish_pending_staged_write(&self, python: Python<'_>) -> PyResult<()> {
        // Bound before the match, so the guard drops here: the publish
        // crosses to the parent — the same mutex-across-the-GIL hazard
        // `release_owned_engine_value` documents.
        let owned_memory = self.owned_memory.lock().clone();
        match owned_memory {
            Some(owned_memory) => owned_memory
                .pending_staged_write_back()
                .publish_if_armed(python, &owned_memory),
            // A handle already closed dropped its share of the surface, so
            // there is nothing here to publish into — and nothing to
            // discard either: the cell went with the surface.
            None => Ok(()),
        }
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
        // The staging's shape is the answer, so mapping it is enough; a
        // pitch is not a reason to copy a frame.
        map_the_cpu_staging_without_reading_a_frame_in(python, &owned_memory)?;
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
        #[cfg(target_os = "linux")]
        self.open_the_staged_cpu_door_over_this_frame(python, &owned_memory)?;
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
        let publish_outcome = self.publish_pending_staged_write(python);
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

    /// Leaving normally publishes any pending device write via `close`;
    /// leaving by a propagating exception discards it first — the write
    /// did not finish, and the surface keeps the frame it already held.
    /// The scope still closes, and `False` never suppresses the raise.
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
        if left_by_a_propagating_exception(exception_type)
            && let Some(owned_memory) = self.owned_memory.lock().clone()
        {
            owned_memory.pending_staged_write_back().discard();
        }
        #[cfg(not(target_os = "linux"))]
        let _ = exception_type;
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
            // The gate serves both sides, and neither is refused here for
            // want of a host mapping: a surface the CPU cannot address
            // directly reaches its pixels through the engine's staging,
            // which the first host-side accessor opens, and its device
            // export rides this same lock.
            if !owned_memory.cpu_reach_goes_through_the_export_staging() {
                owned_memory.host_visible_pixel_plane()?;
            }
            // A fresh scope: whatever the staging holds is a previous
            // one's read-in, and this scope owes its own.
            #[cfg(target_os = "linux")]
            self.cpu_staging_holds_this_locks_frame
                .store(false, std::sync::atomic::Ordering::SeqCst);
            self.cpu_access.lock_for(read_only);
            Ok(())
        })
    }

    /// Close CPU access, publishing any pending staged write back into the
    /// surface first — through whichever staging holds the edit.
    /// Idempotent.
    fn unlock(&self, python: Python<'_>) -> PyResult<()> {
        // The gate opens whether or not the publish succeeded — a
        // surface left locked after a failed publish would refuse
        // every later access with a message about locking, hiding
        // the real failure this raises.
        #[cfg(target_os = "linux")]
        let publish_outcome = self.publish_pending_staged_write(python);
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
                self.open_the_staged_cpu_door_over_this_frame(python, &owned_memory)?;
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
            // Armed before the capsule is minted: a scope that already
            // staged an edit through the CPU door is refused here rather
            // than handed a second writable view over other memory.
            if writable_export {
                owned_memory
                    .pending_staged_write_back()
                    .arm(StagedWriteBackSource::DeviceExportStaging)?;
            }
            let capsule =
                device_dlpack_capsule(python, &owned_memory, prepared, exchange_shape, read_only)?;
            Ok(capsule)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = dl_device;
            host_visible_dlpack_capsule(python, &owned_memory, exchange_shape, read_only)
        }
    }

    /// The scoped device-tensor view over this surface's pixels.
    ///
    /// Entering blits the surface to a linear DLPack view a third-party
    /// GPU package writes in place; leaving normally blits the write
    /// back, ordered by the engine ahead of its next read; leaving by a
    /// propagating exception discards it, and the surface keeps the
    /// frame it already held. Construction does no GPU work — the blit
    /// runs at `__enter__`.
    ///
    /// Independent of `lock()` by design: entering the scope *is* the
    /// write declaration, structurally, so it neither requires nor
    /// consults the CPU access gate — that gate belongs to the
    /// handle-level `lock()` + `__dlpack__` spelling. A surface whose
    /// export cannot take a write-back refuses at `__enter__` rather
    /// than discarding edits silently.
    fn as_device_tensor(&self) -> PyResult<PythonGpuSurfaceDeviceTensorScope> {
        Ok(PythonGpuSurfaceDeviceTensorScope::over(
            self.owned_memory()?,
        ))
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
// The scoped device-tensor view
// =============================================================================

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
    fn over(owned_memory: Arc<GpuSurfaceOwnedMemory>) -> Self {
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

/// A raw OPAQUE_FD texture handle: the allocation's memory fd plus the
/// allocation-stable shape a foreign Vulkan or CUDA external-memory import
/// must reproduce.
///
/// Deliberately outside the `GpuSurface*` family prefix: the object names
/// an allocation, never a frame-bearing surface — the surface-id lifetime
/// guarantees end at export.
#[pyclass(name = "OpaqueFdTextureExport", module = "streamlib", frozen)]
pub(crate) struct PythonOpaqueFdTextureExport {
    exported_memory_fd: i32,
    allocation_byte_size: u64,
    width: u32,
    height: u32,
    format_wire_name: &'static str,
    vk_image_creation_recipe: ExportedVkImageCreationRecipe,
    dedicated_allocation: bool,
    export_contract: OpaqueFdExportContract,
}

/// The `VkImageCreateInfo` recipe an OPAQUE_FD export carries — the shape
/// a conforming foreign re-import must reproduce byte-for-byte. Declared
/// once and held by value by every owner between the wire parse and the
/// Python object, so a field added here reaches all of them.
#[derive(Clone, Copy)]
pub(crate) struct ExportedVkImageCreationRecipe {
    pub(crate) vk_image_tiling: i32,
    pub(crate) vk_image_usage_flags: u32,
    pub(crate) vk_image_mip_levels: u32,
    pub(crate) vk_image_array_layers: u32,
    pub(crate) vk_image_samples: i32,
}

/// The allocation-binding half of the raw-handle export contract: the
/// exporter's memory type index and device UUID travel together — an
/// OPAQUE_FD registration carries both or its checkout is refused, so
/// one-without-the-other is unrepresentable.
#[derive(Clone, Copy)]
pub(crate) struct OpaqueFdExportContract {
    pub(crate) vk_memory_type_index: u32,
    pub(crate) exporting_device_uuid: [u8; 16],
}

#[cfg(target_os = "linux")]
impl From<crate::python_helper_process_pixel_exchange::OpaqueFdTextureExportDescription>
    for PythonOpaqueFdTextureExport
{
    fn from(
        description: crate::python_helper_process_pixel_exchange::OpaqueFdTextureExportDescription,
    ) -> Self {
        use std::os::unix::io::IntoRawFd;
        Self {
            exported_memory_fd: description.exported_memory_fd.into_raw_fd(),
            allocation_byte_size: description.allocation_byte_size,
            width: description.width,
            height: description.height,
            format_wire_name: description.format_wire_name,
            vk_image_creation_recipe: description.vk_image_creation_recipe,
            dedicated_allocation: description.dedicated_allocation,
            export_contract: description.export_contract,
        }
    }
}

#[pymethods]
impl PythonOpaqueFdTextureExport {
    /// The exported memory fd. The caller owns it: a successful foreign
    /// import adopts it — never close it after one; always close it after
    /// a failed one.
    #[getter]
    fn fd(&self) -> i32 {
        self.exported_memory_fd
    }

    /// Byte size of the whole `VkDeviceMemory` at offset zero — what the
    /// foreign import states, never a tight width x height x bpp figure.
    #[getter]
    fn allocation_byte_size(&self) -> u64 {
        self.allocation_byte_size
    }

    /// Texture width in pixels.
    #[getter]
    fn width(&self) -> u32 {
        self.width
    }

    /// Texture height in pixels.
    #[getter]
    fn height(&self) -> u32 {
        self.height
    }

    /// The engine's format name for the texture, e.g. `"rgba16_float"`.
    #[getter]
    fn format(&self) -> &'static str {
        self.format_wire_name
    }

    /// Raw `VkImageTiling` the exporter created the image with.
    #[getter]
    fn vk_image_tiling(&self) -> i32 {
        self.vk_image_creation_recipe.vk_image_tiling
    }

    /// Raw `VkImageUsageFlags` bitfield the exporter created the image with.
    #[getter]
    fn vk_image_usage_flags(&self) -> u32 {
        self.vk_image_creation_recipe.vk_image_usage_flags
    }

    /// `VkImageCreateInfo::mipLevels` of the exporter's image.
    #[getter]
    fn vk_image_mip_levels(&self) -> u32 {
        self.vk_image_creation_recipe.vk_image_mip_levels
    }

    /// `VkImageCreateInfo::arrayLayers` of the exporter's image.
    #[getter]
    fn vk_image_array_layers(&self) -> u32 {
        self.vk_image_creation_recipe.vk_image_array_layers
    }

    /// Raw `VkSampleCountFlagBits` of the exporter's image.
    #[getter]
    fn vk_image_samples(&self) -> i32 {
        self.vk_image_creation_recipe.vk_image_samples
    }

    /// Whether the allocation is dedicated — always true for this flavour;
    /// omitting the importer-side dedicated chain is undefined behaviour,
    /// not leniency.
    #[getter]
    fn dedicated_allocation(&self) -> bool {
        self.dedicated_allocation
    }

    /// The exporter's Vulkan memory type index, for the importer-side
    /// `vkAllocateMemory(VkImportMemoryFdInfoKHR)`.
    #[getter]
    fn vk_memory_type_index(&self) -> u32 {
        self.export_contract.vk_memory_type_index
    }

    /// The exporting device's `VkPhysicalDeviceIDProperties::deviceUUID`,
    /// 16 bytes — an OPAQUE_FD is device-bound.
    #[getter]
    fn exporting_device_uuid<'py>(&self, python: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(python, &self.export_contract.exporting_device_uuid)
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
                HelperCheckedOutSurface::PixelBuffer(checked_out),
            ));
        }
        let _ = (python, width, height, pixel_format);
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Acquire a pooled device texture, named by the surface id the engine
    /// minted for it.
    ///
    /// The id is the whole handle: a kernel dispatch binds it, and a
    /// downstream processor resolves it. `copy_src` and `copy_dst` ride
    /// every request, so the CPU doors reach the pixels over the surface's
    /// host-visible staging with no transfer usage spelled here.
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

    /// Whether an edit written back into this surface publishes at all —
    /// the engine's one answer for every write door: a write-back belongs
    /// to a pooled frame whose allocation is its only backing, or to a
    /// registered texture that takes a recorded copy in; a frame backed by
    /// neither answers `False`.
    /// `writable()` refuses on this answer; `cpu()` hands its array out
    /// read-only on it.
    fn surface_can_take_write_back(&self, python: Python<'_>, surface_id: &str) -> PyResult<bool> {
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            return exchange_client.surface_can_take_write_back(python, surface_id);
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
                HelperCheckedOutSurface::PixelBuffer(checked_out),
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

    /// Request a window this processor owns, presented by the engine.
    ///
    /// Constructed once in `setup()`, named frames per frame in `process()`.
    /// The window lives in the app process on its own present loop, so it
    /// keeps its frame rate whatever this processor's pace is, and naming no
    /// frame leaves the last one up.
    ///
    /// Raises when the process can get no window at all — no display server,
    /// or a window event pump that has already failed — rather than handing
    /// back a window that would show nothing. An author for whom the window is
    /// optional writes the `try/except`.
    #[pyo3(signature = (title, width = 1280, height = 720))]
    fn create_window(
        &self,
        python: Python<'_>,
        title: &str,
        width: u32,
        height: u32,
    ) -> PyResult<PythonProcessorOwnedWindow> {
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let window_id =
                exchange_client.create_processor_owned_window(python, title, width, height)?;
            return Ok(PythonProcessorOwnedWindow::over_the_minted_window(
                window_id,
                title.to_string(),
                Arc::clone(exchange_client),
            ));
        }
        let _ = (python, title, width, height);
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

    /// Build a graphics kernel from GLSL source, or from pre-compiled SPIR-V.
    ///
    /// Constructed once in `setup()`, drawn per frame in `process()`. The
    /// engine compiles both stages and reflects them at construction, taking
    /// its binding names from them — those names are what `draw` resolves
    /// against. Re-creating an identical kernel is free of compilation.
    ///
    /// The vertices are the shaders' own: no escalate op mints a vertex or
    /// index buffer, so a vertex stage fabricates its positions from
    /// `gl_VertexIndex`, and the pipeline carries no vertex input state. The
    /// pass attaches colour targets only, so there is no depth state either.
    #[pyo3(signature = (
        color_attachment_formats,
        vertex_source = None,
        vertex_spirv = None,
        vertex_entry_point = "main",
        fragment_source = None,
        fragment_spirv = None,
        fragment_entry_point = "main",
        push_constant_size = 0,
        bindings = None,
        label = "",
        topology = "triangle_list",
        polygon_mode = "fill",
        cull_mode = "none",
        front_face = "counter_clockwise",
        line_width = 1.0,
        color_write_channels = "rgba",
        color_blend = None,
        dynamic_state = "viewport_scissor",
    ))]
    #[expect(
        clippy::too_many_arguments,
        reason = "the pipeline state is keyword arguments mirroring the wire's own flat shape"
    )]
    fn create_graphics_kernel(
        &self,
        python: Python<'_>,
        color_attachment_formats: Vec<String>,
        vertex_source: Option<&str>,
        vertex_spirv: Option<&[u8]>,
        vertex_entry_point: &str,
        fragment_source: Option<&str>,
        fragment_spirv: Option<&[u8]>,
        fragment_entry_point: &str,
        push_constant_size: u32,
        bindings: Option<&Bound<'_, PyDict>>,
        label: &str,
        topology: &str,
        polygon_mode: &str,
        cull_mode: &str,
        front_face: &str,
        line_width: f32,
        color_write_channels: &str,
        color_blend: Option<&Bound<'_, PyDict>>,
        dynamic_state: &str,
    ) -> PyResult<PythonGraphicsKernel> {
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let declared = declared_staged_kernel_bindings_to_wire(
                python,
                bindings,
                "graphics binding kind",
                GRAPHICS_BINDING_KIND_WIRE_NAMES,
                "graphics stage",
                GRAPHICS_SHADER_STAGE_WIRE_BITS,
            )?;
            let pipeline_state = graphics_pipeline_state_to_wire(
                python,
                &GraphicsPipelineStateArguments {
                    color_attachment_formats: &color_attachment_formats,
                    topology,
                    polygon_mode,
                    cull_mode,
                    front_face,
                    line_width,
                    color_write_channels,
                    color_blend,
                    dynamic_state,
                },
            )?;
            // Neither and both are refused engine-side, in the one place the
            // rule is written; forwarding both fields keeps the wheel from
            // becoming a second spelling of it that can drift.
            let vertex_spirv_hex = vertex_spirv.map(encode_lowercase_hex).unwrap_or_default();
            let fragment_spirv_hex = fragment_spirv.map(encode_lowercase_hex).unwrap_or_default();
            let (kernel_id, reflected_binding_kinds) = exchange_client.register_graphics_kernel(
                python,
                &HelperProcessGraphicsKernelRegistration {
                    label,
                    vertex_source: vertex_source.unwrap_or_default(),
                    vertex_spirv_hex: &vertex_spirv_hex,
                    vertex_entry_point,
                    fragment_source: fragment_source.unwrap_or_default(),
                    fragment_spirv_hex: &fragment_spirv_hex,
                    fragment_entry_point,
                    push_constant_size,
                    declared_bindings: &declared,
                    pipeline_state: &pipeline_state,
                },
            )?;
            return Ok(PythonGraphicsKernel {
                kernel_id,
                push_constant_size,
                reflected_binding_kinds,
                helper_process_exchange_client: Arc::clone(exchange_client),
            });
        }
        let _ = (
            python,
            color_attachment_formats,
            vertex_source,
            vertex_spirv,
            vertex_entry_point,
            fragment_source,
            fragment_spirv,
            fragment_entry_point,
            push_constant_size,
            bindings,
            label,
            topology,
            polygon_mode,
            cull_mode,
            front_face,
            line_width,
            color_write_channels,
            color_blend,
            dynamic_state,
        );
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Build a ray-tracing kernel from GLSL sources, or from pre-compiled
    /// SPIR-V.
    ///
    /// `stages` is one mapping per shader module — `{"stage": "ray_gen",
    /// "source": …}` — and `groups` says how the shader binding table is laid
    /// out over them, each group naming its modules by index into `stages`.
    /// Two modules can fill the same stage, which is why a group points at an
    /// index rather than a name.
    #[pyo3(signature = (
        stages,
        groups,
        max_recursion_depth = 1,
        push_constant_size = 0,
        bindings = None,
        label = "",
    ))]
    #[expect(
        clippy::too_many_arguments,
        reason = "each is one field of the registration the wire carries"
    )]
    fn create_ray_tracing_kernel(
        &self,
        python: Python<'_>,
        stages: &Bound<'_, PyAny>,
        groups: &Bound<'_, PyAny>,
        max_recursion_depth: u32,
        push_constant_size: u32,
        bindings: Option<&Bound<'_, PyDict>>,
        label: &str,
    ) -> PyResult<PythonRayTracingKernel> {
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let wire_stages = ray_tracing_stages_to_wire(python, stages)?;
            let wire_groups = ray_tracing_shader_groups_to_wire(python, groups, wire_stages.len())?;
            let declared = declared_staged_kernel_bindings_to_wire(
                python,
                bindings,
                "ray-tracing binding kind",
                RAY_TRACING_BINDING_KIND_WIRE_NAMES,
                "ray-tracing stage",
                RAY_TRACING_SHADER_STAGE_WIRE_BITS,
            )?;
            let (kernel_id, reflected_binding_kinds) = exchange_client
                .register_ray_tracing_kernel(
                    python,
                    &HelperProcessRayTracingKernelRegistration {
                        label,
                        stages: &wire_stages,
                        groups: &wire_groups,
                        declared_bindings: &declared,
                        max_recursion_depth,
                        push_constant_size,
                    },
                )?;
            return Ok(PythonRayTracingKernel {
                kernel_id,
                push_constant_size,
                reflected_binding_kinds,
                helper_process_exchange_client: Arc::clone(exchange_client),
            });
        }
        let _ = (
            python,
            stages,
            groups,
            max_recursion_depth,
            push_constant_size,
            bindings,
            label,
        );
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Build a bottom-level acceleration structure over triangle geometry.
    ///
    /// `vertices` is `[x, y, z, x, y, z, …]` and `indices` is three per
    /// triangle. The returned handle is what `build_tlas` places in a scene.
    #[pyo3(signature = (vertices, indices, label = ""))]
    fn build_triangles_blas(
        &self,
        python: Python<'_>,
        vertices: Vec<f32>,
        indices: Vec<u32>,
        label: &str,
    ) -> PyResult<PythonAccelerationStructureHandle> {
        if !vertices.len().is_multiple_of(3) {
            return Err(PyValueError::new_err(format!(
                "{} vertex floats were supplied; a vertex is three of them, interleaved as \
                 [x, y, z, x, y, z, …]",
                vertices.len()
            )));
        }
        if !indices.len().is_multiple_of(3) {
            return Err(PyValueError::new_err(format!(
                "{} indices were supplied; a triangle is three of them",
                indices.len()
            )));
        }
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let acceleration_structure_id = exchange_client.register_acceleration_structure_blas(
                python,
                label,
                &encode_little_endian_f32_hex(&vertices),
                &encode_little_endian_u32_hex(&indices),
            )?;
            return Ok(PythonAccelerationStructureHandle {
                acceleration_structure_id,
                is_top_level: false,
                structure_label: label.to_string(),
                helper_process_exchange_client: Some(Arc::clone(exchange_client)),
            });
        }
        let _ = (python, vertices, indices, label);
        Err(gpu_unreachable_from_a_helper_process_error())
    }

    /// Build the top-level acceleration structure a trace binds, over
    /// already-built bottom-level ones.
    ///
    /// Each instance is a mapping naming its `blas` and, optionally, the
    /// row-major 3×4 `transform` that places it, its 8-bit `mask`, its 24-bit
    /// `custom_index`, its `sbt_record_offset` and its geometry `flags`.
    /// The structure keeps every bottom-level one it references alive.
    #[pyo3(signature = (instances, label = ""))]
    fn build_tlas(
        &self,
        python: Python<'_>,
        instances: &Bound<'_, PyAny>,
        label: &str,
    ) -> PyResult<PythonAccelerationStructureHandle> {
        #[cfg(target_os = "linux")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let wire_instances = tlas_instances_to_wire(python, instances)?;
            let acceleration_structure_id = exchange_client.register_acceleration_structure_tlas(
                python,
                label,
                &wire_instances,
            )?;
            return Ok(PythonAccelerationStructureHandle {
                acceleration_structure_id,
                is_top_level: true,
                structure_label: label.to_string(),
                helper_process_exchange_client: Some(Arc::clone(exchange_client)),
            });
        }
        let _ = (python, instances, label);
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

    /// Export the OPAQUE_FD texture handle for `surface`, for native code
    /// that runs its own Vulkan or CUDA external-memory import against
    /// the allocation.
    ///
    /// Returns a [`PythonOpaqueFdTextureExport`]. **The caller owns the
    /// fd** — a successful foreign import adopts it; close it after a
    /// failed one. Consume it as an image: a linear mapping over
    /// OPTIMAL-tiled memory yields block-linear bytes, never pixels.
    ///
    /// Answered without leaving this process: the fd arrived here over
    /// SCM_RIGHTS when the surface was checked out.
    #[cfg(target_os = "linux")]
    #[expect(
        clippy::unused_self,
        reason = "the surface carries the fds; the capability is the door"
    )]
    fn export_opaque_fd(
        &self,
        python: Python<'_>,
        surface: &PythonGpuSurfaceHandle,
    ) -> PyResult<PythonOpaqueFdTextureExport> {
        let owned_memory = surface.owned_memory()?;
        Ok(python.detach(|| owned_memory.export_opaque_fd())?.into())
    }

    /// Import a foreign DMA-BUF file descriptor as a surface this graph can
    /// resolve. The caller keeps ownership of `fd` — the kernel dups it on
    /// the SCM_RIGHTS crossing — and may close it once this returns.
    #[cfg(target_os = "linux")]
    #[pyo3(signature = (fd, width, height, format = "bgra", byte_size = None))]
    fn import_dma_buf(
        &self,
        python: Python<'_>,
        fd: i32,
        width: u32,
        height: u32,
        format: &str,
        byte_size: Option<u64>,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let pixel_format = parse_pixel_format_name(format)?;
        if pixel_format.plane_count() != 1 {
            return Err(PyValueError::new_err(format!(
                "import_dma_buf adopts one plane behind one fd; {format:?} carries \
                 {} planes",
                pixel_format.plane_count()
            )));
        }
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            // A tight single plane when the caller states no size — the
            // exporter's own byte size is the honest input whenever padding
            // is in play, because a stride cannot be conjured from an fd.
            let plane_byte_size = byte_size.unwrap_or_else(|| {
                u64::from(width)
                    * u64::from(pixel_format.bits_per_pixel().div_ceil(8))
                    * u64::from(height)
            });
            let checked_out = exchange_client.import_foreign_dma_buf(
                python,
                fd,
                width,
                height,
                pixel_format,
                plane_byte_size,
            )?;
            return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
                HelperCheckedOutSurface::PixelBuffer(checked_out),
            ));
        }
        Err(gpu_unreachable_from_a_helper_process_error())
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
                    // Child-scoped, never the node's own runtime id — the
                    // service's crash watchdog sweeps registrations by
                    // runtime id, and this child's crash must sweep only
                    // this child's adoptions.
                    format!("helper:{processor_id}"),
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

    /// The next bag on `port_name` with the link it arrived on, or `None`.
    ///
    /// Any number of links may enter one input port, and each is one producer.
    /// This is how a many-input processor tells them apart: the name is the
    /// source channel the link subscribed to, which the engine knows and a
    /// producer cannot misstate.
    #[pyo3(signature = (port_name, *, into = None))]
    fn read_from_inbound_link<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, String)>> {
        self.link_data_access
            .get()
            .read_from_input_port_naming_its_inbound_link(
                python,
                port_name,
                into,
                Some(self.gpu_limited_access_context.bind(python)),
            )
    }

    /// The next bag on `port_name` with its link and its timestamp, or `None`.
    ///
    /// What a many-track sink needs to restate a producer's own timing: the
    /// link names the producer and the stamp is the one that producer wrote,
    /// which is the source frame's instant rather than the moment of the read.
    #[pyo3(signature = (port_name, *, into = None))]
    fn read_from_inbound_link_with_timestamp<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, String, i64)>> {
        self.link_data_access
            .get()
            .read_from_input_port_naming_its_inbound_link_and_timestamp(
                python,
                port_name,
                into,
                Some(self.gpu_limited_access_context.bind(python)),
            )
    }

    /// Every link feeding `port_name`, in wiring order.
    ///
    /// Readable in `setup()`, which is how a sink learns how many producers it
    /// owes before the first bag arrives. A port nothing is connected to lists
    /// none.
    fn inbound_link_names(&self, port_name: &str) -> PyResult<Vec<String>> {
        self.link_data_access
            .get()
            .inbound_links_of_input_port(port_name)
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

/// A geometry blob as the wire carries it: little-endian `f32`s, lowercase hex.
#[cfg(target_os = "linux")]
fn encode_little_endian_f32_hex(values: &[f32]) -> String {
    encode_lowercase_hex(
        &values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<u8>>(),
    )
}

/// An index blob as the wire carries it: little-endian `u32`s, lowercase hex.
#[cfg(target_os = "linux")]
fn encode_little_endian_u32_hex(values: &[u32]) -> String {
    encode_lowercase_hex(
        &values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<u8>>(),
    )
}

/// One word of a fixed wire vocabulary, or the refusal naming the whole set.
///
/// Every enum the escalate wire spells travels as a string the host parses, so
/// checking the spelling here is what keeps a typo on the caller's own stack
/// rather than arriving as an escalate failure a round trip later.
#[cfg(target_os = "linux")]
fn parse_wire_vocabulary_word(
    vocabulary_label: &str,
    supplied: &str,
    accepted: &[&'static str],
) -> PyResult<&'static str> {
    accepted
        .iter()
        .find(|known| **known == supplied)
        .copied()
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown {vocabulary_label} {supplied:?}; the accepted spellings are {}",
                accepted.join(", ")
            ))
        })
}

/// The binding kinds a compute kernel's wire spells.
#[cfg(target_os = "linux")]
const COMPUTE_BINDING_KIND_WIRE_NAMES: &[&str] = &[
    "sampled_image",
    "sampled_texture",
    "storage_buffer",
    "storage_image",
    "uniform_buffer",
];

/// The binding kinds a graphics kernel's wire spells. No `sampled_image`: the
/// graphics pipeline has no samplerless-texture descriptor.
#[cfg(target_os = "linux")]
const GRAPHICS_BINDING_KIND_WIRE_NAMES: &[&str] = &[
    "sampled_texture",
    "storage_buffer",
    "storage_image",
    "uniform_buffer",
];

/// The binding kinds a ray-tracing kernel's wire spells.
#[cfg(target_os = "linux")]
const RAY_TRACING_BINDING_KIND_WIRE_NAMES: &[&str] = &[
    ACCELERATION_STRUCTURE_BINDING_KIND_WIRE_NAME,
    "sampled_texture",
    "storage_buffer",
    "storage_image",
    "uniform_buffer",
];

/// The one binding kind whose value is an acceleration structure rather than a
/// surface, which is why the dispatch path branches on it by name.
#[cfg(target_os = "linux")]
const ACCELERATION_STRUCTURE_BINDING_KIND_WIRE_NAME: &str = "acceleration_structure";

/// The stage bits a graphics binding declaration may name. Host counterpart:
/// `GraphicsShaderStageFlags`.
#[cfg(target_os = "linux")]
const GRAPHICS_SHADER_STAGE_WIRE_BITS: &[(&str, u32)] = &[("vertex", 1), ("fragment", 2)];

/// The stage bits a ray-tracing binding declaration may name. Host
/// counterpart: `RayTracingShaderStageFlags`.
#[cfg(target_os = "linux")]
const RAY_TRACING_SHADER_STAGE_WIRE_BITS: &[(&str, u32)] = &[
    ("ray_gen", 1),
    ("miss", 2),
    ("closest_hit", 4),
    ("any_hit", 8),
    ("intersection", 16),
    ("callable", 32),
];

/// The stages a ray-tracing kernel's shader modules may fill.
#[cfg(target_os = "linux")]
const RAY_TRACING_SHADER_STAGE_WIRE_NAMES: &[&str] = &[
    "any_hit",
    "callable",
    "closest_hit",
    "intersection",
    "miss",
    "ray_gen",
];

/// The shader-group kinds a ray-tracing kernel's binding table is built from.
#[cfg(target_os = "linux")]
const RAY_TRACING_GROUP_KIND_WIRE_NAMES: &[&str] = &["general", "procedural_hit", "triangles_hit"];

/// What a shader group's stage index carries when the group names no stage
/// there. Every stage-index field is present on the wire, so absent needs a
/// value; host counterpart: `RAY_TRACING_STAGE_INDEX_NONE`.
#[cfg(target_os = "linux")]
const RAY_TRACING_STAGE_INDEX_NONE: u32 = u32::MAX;

/// Turn a sequence of spelled-out names into the bitmask the wire carries.
///
/// Every bitmask the escalate wire carries — a binding's stage visibility, a
/// TLAS instance's geometry flags — is spelled here rather than handed over as
/// a raw integer, so a caller never writes a bit position. An empty sequence is
/// an empty mask, which for stages asserts nothing and lets reflection stand.
#[cfg(target_os = "linux")]
fn named_bits_to_wire_bitmask(
    vocabulary_label: &str,
    named: &Bound<'_, PyAny>,
    bit_vocabulary: &[(&'static str, u32)],
) -> PyResult<u32> {
    let accepted: Vec<&'static str> = bit_vocabulary.iter().map(|(name, _)| *name).collect();
    let mut mask = 0u32;
    for name in named.try_iter()? {
        let name: String = name?.extract()?;
        let named = parse_wire_vocabulary_word(vocabulary_label, &name, &accepted)?;
        mask |= bit_vocabulary
            .iter()
            .find(|(candidate, _)| *candidate == named)
            .map_or(0, |(_, bit)| *bit);
    }
    Ok(mask)
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
            entry.set_item(
                "kind",
                parse_wire_vocabulary_word(
                    "compute binding kind",
                    &kind,
                    COMPUTE_BINDING_KIND_WIRE_NAMES,
                )?,
            )?;
            wire.append(entry)?;
        }
    }
    Ok(wire)
}

/// Turn `{name: kind}` or `{name: (kind, stages)}` into the wire's declaration
/// array, for a kernel kind whose bindings carry a stage mask.
///
/// Graphics and ray tracing differ only in which two vocabularies they name,
/// which is also why the host reconciles both through one function.
#[cfg(target_os = "linux")]
fn declared_staged_kernel_bindings_to_wire<'py>(
    python: Python<'py>,
    declared: Option<&Bound<'py, PyDict>>,
    binding_kind_label: &str,
    binding_kind_vocabulary: &[&'static str],
    stage_label: &str,
    stage_bits: &[(&'static str, u32)],
) -> PyResult<Bound<'py, PyList>> {
    let wire = PyList::empty(python);
    let Some(declared) = declared else {
        return Ok(wire);
    };
    for (name, declaration) in declared.iter() {
        let name: String = name.extract()?;
        let (kind, stages) = match declaration.extract::<String>() {
            Ok(kind) => (kind, 0),
            Err(_) => {
                let (kind, named_stages) = declaration
                    .extract::<(String, Bound<'_, PyAny>)>()
                    .map_err(|_| {
                        PyTypeError::new_err(format!(
                            "binding {name:?} must be declared as a kind, or as a (kind, stages) \
                             pair naming the stages that read it"
                        ))
                    })?;
                (
                    kind,
                    named_bits_to_wire_bitmask(stage_label, &named_stages, stage_bits)?,
                )
            }
        };
        let entry = PyDict::new(python);
        entry.set_item("name", name)?;
        entry.set_item(
            "kind",
            parse_wire_vocabulary_word(binding_kind_label, &kind, binding_kind_vocabulary)?,
        )?;
        entry.set_item("stages", stages)?;
        wire.append(entry)?;
    }
    Ok(wire)
}

/// One entry of a list-of-mappings argument, refused by name when it is not a
/// mapping.
#[cfg(target_os = "linux")]
fn mapping_argument_entry<'py>(
    argument_label: &str,
    index: usize,
    entry: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyDict>> {
    entry
        .cast::<PyDict>()
        .cloned()
        .map_err(|_| PyTypeError::new_err(format!("{argument_label} {index} must be a dict")))
}

/// Refuse a mapping carrying a key this argument does not accept.
///
/// A misspelled key would otherwise travel as an absent one the wire fills with
/// a default, which is the silently-wrong-result shape.
#[cfg(target_os = "linux")]
fn refuse_unaccepted_mapping_keys(
    mapping_label: &str,
    mapping: &Bound<'_, PyDict>,
    accepted: &[&str],
) -> PyResult<()> {
    for key in mapping.keys() {
        let key: String = key.extract()?;
        if !accepted.contains(&key.as_str()) {
            return Err(PyValueError::new_err(format!(
                "{mapping_label} was given an unknown key {key:?}; it accepts {}",
                accepted.join(", ")
            )));
        }
    }
    Ok(())
}

/// The `u32` at `key`, or `None` when the mapping does not carry it.
#[cfg(target_os = "linux")]
fn optional_u32_in(mapping: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<u32>> {
    match mapping.get_item(key)? {
        Some(value) => Ok(Some(value.extract()?)),
        None => Ok(None),
    }
}

/// The string at `key`, or `None` when the mapping does not carry it.
#[cfg(target_os = "linux")]
fn optional_string_in(mapping: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    match mapping.get_item(key)? {
        Some(value) => Ok(Some(value.extract()?)),
        None => Ok(None),
    }
}

/// The primitive topologies a graphics pipeline can assemble.
#[cfg(target_os = "linux")]
const GRAPHICS_TOPOLOGY_WIRE_NAMES: &[&str] = &[
    "line_list",
    "line_strip",
    "point_list",
    "triangle_fan",
    "triangle_list",
    "triangle_strip",
];

#[cfg(target_os = "linux")]
const GRAPHICS_POLYGON_MODE_WIRE_NAMES: &[&str] = &["fill", "line", "point"];

#[cfg(target_os = "linux")]
const GRAPHICS_CULL_MODE_WIRE_NAMES: &[&str] = &["back", "front", "front_and_back", "none"];

#[cfg(target_os = "linux")]
const GRAPHICS_FRONT_FACE_WIRE_NAMES: &[&str] = &["clockwise", "counter_clockwise"];

#[cfg(target_os = "linux")]
const GRAPHICS_DYNAMIC_STATE_WIRE_NAMES: &[&str] = &["none", "viewport_scissor"];

#[cfg(target_os = "linux")]
const COLOR_BLEND_FACTOR_WIRE_NAMES: &[&str] = &[
    "constant_alpha",
    "constant_color",
    "dst_alpha",
    "dst_color",
    "one",
    "one_minus_constant_alpha",
    "one_minus_constant_color",
    "one_minus_dst_alpha",
    "one_minus_dst_color",
    "one_minus_src_alpha",
    "one_minus_src_color",
    "src_alpha",
    "src_alpha_saturate",
    "src_color",
    "zero",
];

#[cfg(target_os = "linux")]
const COLOR_BLEND_OP_WIRE_NAMES: &[&str] = &["add", "max", "min", "reverse_subtract", "subtract"];

/// The keys the `color_blend` argument accepts, each defaulting to the
/// conventional source-alpha-over blend when the mapping omits it.
#[cfg(target_os = "linux")]
const COLOR_BLEND_ARGUMENT_KEYS: &[&str] = &[
    "alpha_op",
    "color_op",
    "dst_alpha_factor",
    "dst_color_factor",
    "src_alpha_factor",
    "src_color_factor",
];

/// The colour channels a draw writes, as the bitmask the wire carries.
#[cfg(target_os = "linux")]
fn color_write_channels_to_wire(channels: &str) -> PyResult<u32> {
    let mut mask = 0u32;
    for channel in channels.chars() {
        mask |= match channel {
            'r' => 1,
            'g' => 2,
            'b' => 4,
            'a' => 8,
            _ => {
                return Err(PyValueError::new_err(format!(
                    "unknown colour channel {channel:?} in {channels:?}; a write mask names some \
                     of \"rgba\""
                )));
            }
        };
    }
    Ok(mask)
}

/// The fixed-function state and attachment formats `create_graphics_kernel` was
/// asked for.
#[cfg(target_os = "linux")]
struct GraphicsPipelineStateArguments<'a, 'py> {
    color_attachment_formats: &'a [String],
    topology: &'a str,
    polygon_mode: &'a str,
    cull_mode: &'a str,
    front_face: &'a str,
    line_width: f32,
    color_write_channels: &'a str,
    color_blend: Option<&'a Bound<'py, PyDict>>,
    dynamic_state: &'a str,
}

/// Flatten the pipeline state into the one-level document the wire carries.
///
/// Every field is present because the wire is flat — JSON has no sum types —
/// and the flags decide which ones mean anything. Three groups are pinned here
/// rather than offered as arguments, because a caller could only ever set them
/// to a shape that fails:
/// - `multisample_samples`, since the host builds single-sampled pipelines only.
/// - the vertex-input arrays, since no escalate op mints a vertex buffer for a
///   draw to pull through them.
/// - the depth fields, since the offscreen pass a draw runs attaches colour
///   targets only.
#[cfg(target_os = "linux")]
fn graphics_pipeline_state_to_wire<'py>(
    python: Python<'py>,
    state: &GraphicsPipelineStateArguments<'_, '_>,
) -> PyResult<Bound<'py, PyDict>> {
    if let Some(color_blend) = state.color_blend {
        refuse_unaccepted_mapping_keys("color_blend", color_blend, COLOR_BLEND_ARGUMENT_KEYS)?;
    }
    let blend_word = |key: &str,
                      when_absent: &'static str,
                      vocabulary: &[&'static str]|
     -> PyResult<&'static str> {
        let Some(color_blend) = state.color_blend else {
            return Ok(when_absent);
        };
        match optional_string_in(color_blend, key)? {
            Some(spelled) => parse_wire_vocabulary_word(key, &spelled, vocabulary),
            None => Ok(when_absent),
        }
    };

    let color_formats = PyList::empty(python);
    for format in state.color_attachment_formats {
        color_formats.append(parse_texture_format_name(format)?)?;
    }

    let wire = PyDict::new(python);
    wire.set_item("attachment_color_formats", color_formats)?;
    wire.set_item(
        "topology",
        parse_wire_vocabulary_word("topology", state.topology, GRAPHICS_TOPOLOGY_WIRE_NAMES)?,
    )?;
    wire.set_item(
        "rasterization_polygon_mode",
        parse_wire_vocabulary_word(
            "polygon mode",
            state.polygon_mode,
            GRAPHICS_POLYGON_MODE_WIRE_NAMES,
        )?,
    )?;
    wire.set_item(
        "rasterization_cull_mode",
        parse_wire_vocabulary_word("cull mode", state.cull_mode, GRAPHICS_CULL_MODE_WIRE_NAMES)?,
    )?;
    wire.set_item(
        "rasterization_front_face",
        parse_wire_vocabulary_word(
            "front face",
            state.front_face,
            GRAPHICS_FRONT_FACE_WIRE_NAMES,
        )?,
    )?;
    wire.set_item("rasterization_line_width", state.line_width)?;
    wire.set_item("multisample_samples", 1u32)?;
    wire.set_item("vertex_input_bindings", PyList::empty(python))?;
    wire.set_item("vertex_input_attributes", PyList::empty(python))?;
    wire.set_item("depth_stencil_enabled", false)?;
    wire.set_item("depth_write", false)?;
    wire.set_item("depth_compare_op", "always")?;
    wire.set_item(
        "color_write_mask",
        color_write_channels_to_wire(state.color_write_channels)?,
    )?;
    wire.set_item("color_blend_enabled", state.color_blend.is_some())?;
    wire.set_item(
        "color_blend_src_color_factor",
        blend_word(
            "src_color_factor",
            "src_alpha",
            COLOR_BLEND_FACTOR_WIRE_NAMES,
        )?,
    )?;
    wire.set_item(
        "color_blend_dst_color_factor",
        blend_word(
            "dst_color_factor",
            "one_minus_src_alpha",
            COLOR_BLEND_FACTOR_WIRE_NAMES,
        )?,
    )?;
    wire.set_item(
        "color_blend_color_op",
        blend_word("color_op", "add", COLOR_BLEND_OP_WIRE_NAMES)?,
    )?;
    wire.set_item(
        "color_blend_src_alpha_factor",
        blend_word("src_alpha_factor", "one", COLOR_BLEND_FACTOR_WIRE_NAMES)?,
    )?;
    wire.set_item(
        "color_blend_dst_alpha_factor",
        blend_word(
            "dst_alpha_factor",
            "one_minus_src_alpha",
            COLOR_BLEND_FACTOR_WIRE_NAMES,
        )?,
    )?;
    wire.set_item(
        "color_blend_alpha_op",
        blend_word("alpha_op", "add", COLOR_BLEND_OP_WIRE_NAMES)?,
    )?;
    wire.set_item(
        "dynamic_state",
        parse_wire_vocabulary_word(
            "dynamic state",
            state.dynamic_state,
            GRAPHICS_DYNAMIC_STATE_WIRE_NAMES,
        )?,
    )?;
    Ok(wire)
}

/// The keys one entry of the `stages` argument accepts.
#[cfg(target_os = "linux")]
const RAY_TRACING_STAGE_ARGUMENT_KEYS: &[&str] = &["entry_point", "source", "spirv", "stage"];

/// Turn `stages=[…]` into the wire's shader-stage array.
///
/// `source` and `spirv` both travel: exactly-one-of is refused host-side, in
/// the one place that rule is written.
#[cfg(target_os = "linux")]
fn ray_tracing_stages_to_wire<'py>(
    python: Python<'py>,
    stages: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyList>> {
    let wire = PyList::empty(python);
    for (index, stage) in stages.try_iter()?.enumerate() {
        let stage = mapping_argument_entry("stage", index, &stage?)?;
        refuse_unaccepted_mapping_keys(
            &format!("stage {index}"),
            &stage,
            RAY_TRACING_STAGE_ARGUMENT_KEYS,
        )?;
        let named_stage = optional_string_in(&stage, "stage")?.ok_or_else(|| {
            PyValueError::new_err(format!(
                "stage {index} names no `stage`; every shader module says which stage it fills"
            ))
        })?;
        let spirv: Vec<u8> = match stage.get_item("spirv")? {
            Some(blob) => blob.extract()?,
            None => Vec::new(),
        };
        let entry = PyDict::new(python);
        entry.set_item(
            "stage",
            parse_wire_vocabulary_word(
                "ray-tracing stage",
                &named_stage,
                RAY_TRACING_SHADER_STAGE_WIRE_NAMES,
            )?,
        )?;
        entry.set_item(
            "source",
            optional_string_in(&stage, "source")?.unwrap_or_default(),
        )?;
        entry.set_item("spv_hex", encode_lowercase_hex(&spirv))?;
        entry.set_item(
            "entry_point",
            optional_string_in(&stage, "entry_point")?.unwrap_or_else(|| "main".to_string()),
        )?;
        wire.append(entry)?;
    }
    Ok(wire)
}

/// The keys one entry of the `groups` argument accepts.
#[cfg(target_os = "linux")]
const RAY_TRACING_GROUP_ARGUMENT_KEYS: &[&str] = &[
    "any_hit_stage",
    "closest_hit_stage",
    "general_stage",
    "intersection_stage",
    "kind",
];

/// Turn `groups=[…]` into the wire's shader-group array.
///
/// A group names its stages by index into the `stages` argument — the shader
/// binding table is built in this order, and two modules can fill the same
/// stage, so there is no name to use instead. Absent indices become the wire's
/// sentinel here rather than in the caller's source.
#[cfg(target_os = "linux")]
fn ray_tracing_shader_groups_to_wire<'py>(
    python: Python<'py>,
    groups: &Bound<'_, PyAny>,
    stage_count: usize,
) -> PyResult<Bound<'py, PyList>> {
    let wire = PyList::empty(python);
    for (index, group) in groups.try_iter()?.enumerate() {
        let group = mapping_argument_entry("group", index, &group?)?;
        refuse_unaccepted_mapping_keys(
            &format!("group {index}"),
            &group,
            RAY_TRACING_GROUP_ARGUMENT_KEYS,
        )?;
        let kind = optional_string_in(&group, "kind")?
            .ok_or_else(|| PyValueError::new_err(format!("group {index} names no `kind`")))?;
        let kind = parse_wire_vocabulary_word(
            "shader group kind",
            &kind,
            RAY_TRACING_GROUP_KIND_WIRE_NAMES,
        )?;

        let named_stage = |key: &str| -> PyResult<Option<u32>> {
            let Some(stage_index) = optional_u32_in(&group, key)? else {
                return Ok(None);
            };
            if stage_index as usize >= stage_count {
                return Err(PyValueError::new_err(format!(
                    "group {index} names {key} {stage_index}, and only {stage_count} shader \
                     module(s) were supplied"
                )));
            }
            Ok(Some(stage_index))
        };
        let general = named_stage("general_stage")?;
        let closest_hit = named_stage("closest_hit_stage")?;
        let any_hit = named_stage("any_hit_stage")?;
        let intersection = named_stage("intersection_stage")?;

        match kind {
            "general" if general.is_none() => {
                return Err(PyValueError::new_err(format!(
                    "group {index} is `general` and names no `general_stage`; a general group is \
                     the one ray-gen, miss or callable module it points at"
                )));
            }
            "triangles_hit" if closest_hit.is_none() && any_hit.is_none() => {
                return Err(PyValueError::new_err(format!(
                    "group {index} is `triangles_hit` and names neither `closest_hit_stage` nor \
                     `any_hit_stage`; a hit group needs at least one of them"
                )));
            }
            "procedural_hit" if intersection.is_none() => {
                return Err(PyValueError::new_err(format!(
                    "group {index} is `procedural_hit` and names no `intersection_stage`, which \
                     is the module a procedural group intersects with"
                )));
            }
            _ => {}
        }

        let entry = PyDict::new(python);
        entry.set_item("kind", kind)?;
        entry.set_item(
            "general_stage",
            general.unwrap_or(RAY_TRACING_STAGE_INDEX_NONE),
        )?;
        entry.set_item(
            "closest_hit_stage",
            closest_hit.unwrap_or(RAY_TRACING_STAGE_INDEX_NONE),
        )?;
        entry.set_item(
            "any_hit_stage",
            any_hit.unwrap_or(RAY_TRACING_STAGE_INDEX_NONE),
        )?;
        entry.set_item(
            "intersection_stage",
            intersection.unwrap_or(RAY_TRACING_STAGE_INDEX_NONE),
        )?;
        wire.append(entry)?;
    }
    Ok(wire)
}

/// The keys one entry of `build_tlas`'s `instances` argument accepts.
#[cfg(target_os = "linux")]
const TLAS_INSTANCE_ARGUMENT_KEYS: &[&str] = &[
    "blas",
    "custom_index",
    "flags",
    "mask",
    "sbt_record_offset",
    "transform",
];

/// The `VkGeometryInstanceFlagsKHR` bits an instance can name, spelled rather
/// than passed as a raw mask.
#[cfg(target_os = "linux")]
const GEOMETRY_INSTANCE_FLAG_WIRE_BITS: &[(&str, u32)] = &[
    ("triangle_facing_cull_disable", 1),
    ("triangle_flip_facing", 2),
    ("force_opaque", 4),
    ("force_no_opaque", 8),
];

/// Row-major 3×4 identity — where an instance that names no transform sits.
#[cfg(target_os = "linux")]
const IDENTITY_TLAS_INSTANCE_TRANSFORM: [f32; 12] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0,
];

/// The widest value an instance's 24-bit `gl_InstanceCustomIndexEXT` can carry.
/// The host masks the high byte off silently, so it is refused here.
#[cfg(target_os = "linux")]
const WIDEST_TLAS_INSTANCE_CUSTOM_INDEX: u32 = 0x00ff_ffff;

/// Turn `instances=[…]` into the wire's TLAS instance array.
#[cfg(target_os = "linux")]
fn tlas_instances_to_wire<'py>(
    python: Python<'py>,
    instances: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyList>> {
    let wire = PyList::empty(python);
    for (index, instance) in instances.try_iter()?.enumerate() {
        let instance = mapping_argument_entry("instance", index, &instance?)?;
        refuse_unaccepted_mapping_keys(
            &format!("instance {index}"),
            &instance,
            TLAS_INSTANCE_ARGUMENT_KEYS,
        )?;
        let named_blas = instance.get_item("blas")?.ok_or_else(|| {
            PyValueError::new_err(format!(
                "instance {index} names no `blas`; an instance places one bottom-level structure \
                 in the scene"
            ))
        })?;
        let bottom_level = named_blas
            .extract::<PyRef<'_, PythonAccelerationStructureHandle>>()
            .map_err(|_| {
                PyTypeError::new_err(format!(
                    "instance {index}'s `blas` must be the handle `build_triangles_blas` returned"
                ))
            })?;
        if bottom_level.is_top_level {
            return Err(PyValueError::new_err(format!(
                "instance {index}'s `blas` is a top-level structure; an instance places a \
                 bottom-level one, and the top-level structure is what a trace binds"
            )));
        }
        let transform: Vec<f32> = match instance.get_item("transform")? {
            Some(transform) => transform.extract()?,
            None => IDENTITY_TLAS_INSTANCE_TRANSFORM.to_vec(),
        };
        if transform.len() != 12 {
            return Err(PyValueError::new_err(format!(
                "instance {index}'s transform has {} floats; it is a row-major 3×4 affine, so \
                 exactly 12",
                transform.len()
            )));
        }
        let mask = optional_u32_in(&instance, "mask")?.unwrap_or(0xff);
        if mask > 0xff {
            return Err(PyValueError::new_err(format!(
                "instance {index}'s mask is {mask}; a visibility mask is 8-bit, and a ray hits \
                 the instance when `mask & cull_mask` is non-zero"
            )));
        }
        let custom_index = optional_u32_in(&instance, "custom_index")?.unwrap_or(0);
        if custom_index > WIDEST_TLAS_INSTANCE_CUSTOM_INDEX {
            return Err(PyValueError::new_err(format!(
                "instance {index}'s custom_index is {custom_index}; it reaches hit shaders as a \
                 24-bit `gl_InstanceCustomIndexEXT`, so anything above \
                 {WIDEST_TLAS_INSTANCE_CUSTOM_INDEX} would arrive truncated"
            )));
        }
        let flags = match instance.get_item("flags")? {
            Some(named_flags) => named_bits_to_wire_bitmask(
                "geometry instance flag",
                &named_flags,
                GEOMETRY_INSTANCE_FLAG_WIRE_BITS,
            )?,
            None => 0,
        };

        let entry = PyDict::new(python);
        entry.set_item("blas_id", bottom_level.acceleration_structure_id.as_str())?;
        entry.set_item("transform", transform)?;
        entry.set_item("mask", mask)?;
        entry.set_item("custom_index", custom_index)?;
        entry.set_item(
            "sbt_record_offset",
            optional_u32_in(&instance, "sbt_record_offset")?.unwrap_or(0),
        )?;
        entry.set_item("flags", flags)?;
        wire.append(entry)?;
    }
    Ok(wire)
}

/// One binding of a registered kernel as reflection found it: the shaders'
/// name and the wire spelling of its kind.
///
/// One type for all three pipeline kinds, because a register response carries
/// the same two fields whichever op asked for it.
pub(crate) struct ReflectedKernelBinding {
    pub(crate) name: String,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    pub(crate) kind: String,
}

/// The shaders' own names for a kernel's bindings, in slot order.
fn reflected_binding_names(reflected: &[ReflectedKernelBinding]) -> Vec<String> {
    reflected
        .iter()
        .map(|binding| binding.name.clone())
        .collect()
}

/// The kind the shaders declare `name` as.
///
/// An unknown name is refused here rather than sent — the round trip would
/// refuse it too, but the caller's own stack is where the mistake is.
#[cfg(target_os = "linux")]
fn reflected_kind_of_binding<'a>(
    reflected: &'a [ReflectedKernelBinding],
    name: &str,
) -> PyResult<&'a str> {
    reflected
        .iter()
        .find(|binding| binding.name == name)
        .map(|binding| binding.kind.as_str())
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "no binding named {name:?}; these shaders declare {}",
                reflected_binding_names(reflected)
                    .iter()
                    .map(|declared| format!("{declared:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

/// Refuse a push-constant payload that is not the size the kernel declares.
///
/// The engine reconciles the declared size against reflection at construction,
/// so a kernel that exists agrees with its shaders and this check is the
/// shaders' own.
#[cfg(target_os = "linux")]
fn require_declared_push_constant_size(declared_size: u32, supplied: &[u8]) -> PyResult<()> {
    if supplied.len() != declared_size as usize {
        return Err(PyValueError::new_err(format!(
            "this kernel declares {declared_size} push-constant bytes but {} were supplied",
            supplied.len()
        )));
    }
    Ok(())
}

/// One dispatch's bindings as the wire carries them, each resolved by the kind
/// the shaders declare it.
///
/// `wire_target_field_name` is the wire's own name for the bound resource —
/// `surface_uuid` on a graphics draw, `target_id` everywhere else. An
/// `acceleration_structure` binding resolves through its own registry rather
/// than through a surface, so it is the one kind that takes a different handle.
#[cfg(target_os = "linux")]
fn supplied_kernel_bindings_to_wire<'py>(
    python: Python<'py>,
    reflected: &[ReflectedKernelBinding],
    supplied: &Bound<'py, PyDict>,
    wire_target_field_name: &str,
) -> PyResult<Bound<'py, PyList>> {
    let wire_bindings = PyList::empty(python);
    for (name, bound_to) in supplied.iter() {
        let name: String = name.extract()?;
        let kind = reflected_kind_of_binding(reflected, &name)?.to_string();
        let target_id = if kind == ACCELERATION_STRUCTURE_BINDING_KIND_WIRE_NAME {
            bound_acceleration_structure_id(&name, &bound_to)?
        } else {
            bound_surface_id(&name, &bound_to)?
        };
        let entry = PyDict::new(python);
        entry.set_item(wire_target_field_name, target_id)?;
        entry.set_item("name", name)?;
        entry.set_item("kind", kind)?;
        wire_bindings.append(entry)?;
    }
    Ok(wire_bindings)
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
    reflected_binding_kinds: Vec<ReflectedKernelBinding>,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    helper_process_exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

#[pymethods]
impl PythonComputeKernel {
    /// The shader's own names for this kernel's bindings, in slot order.
    #[getter]
    fn binding_names(&self) -> Vec<String> {
        reflected_binding_names(&self.reflected_binding_kinds)
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
        require_declared_push_constant_size(self.push_constant_size, push_constants)?;
        let wire_bindings = supplied_kernel_bindings_to_wire(
            python,
            &self.reflected_binding_kinds,
            bindings,
            "target_id",
        )?;
        Ok((wire_bindings, encode_lowercase_hex(push_constants)))
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
        let left_by_a_raise = left_by_a_propagating_exception(exception_type);
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

/// A graphics kernel the engine built and holds, drawn by name.
///
/// Constructed in `setup()` where the capability is Full; drawn per frame in
/// `process()`. No kernel handle string, fence, timeline or descriptor slot
/// number reaches Python — the object is the handle.
///
/// Defined on every platform so the stub's surface is honest everywhere; off
/// Linux it is unconstructible, because `create_graphics_kernel` refuses before
/// reaching it.
#[pyclass(name = "GraphicsKernel", module = "streamlib", frozen)]
pub(crate) struct PythonGraphicsKernel {
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    kernel_id: String,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    push_constant_size: u32,
    /// The caller supplies surfaces by name; which kind each name is, is the
    /// shaders' to say, so it is carried rather than guessed per draw.
    reflected_binding_kinds: Vec<ReflectedKernelBinding>,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    helper_process_exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

#[pymethods]
impl PythonGraphicsKernel {
    /// The shaders' own names for this kernel's bindings, in slot order.
    #[getter]
    fn binding_names(&self) -> Vec<String> {
        reflected_binding_names(&self.reflected_binding_kinds)
    }

    /// Render one offscreen pass into `color_targets`, binding each of the
    /// shaders' declared resources by name.
    ///
    /// Bindings never persist on the kernel, so every draw supplies all of
    /// them. The pass discards each colour target's previous contents and
    /// starts from transparent black. Returns when the GPU work has retired and
    /// the pixels are visible.
    #[pyo3(signature = (
        bindings,
        color_targets,
        extent,
        vertex_count,
        instance_count = 1,
        first_vertex = 0,
        first_instance = 0,
        push_constants = None,
    ))]
    #[expect(
        clippy::too_many_arguments,
        reason = "each is one field of the draw the wire carries; a bundle would hide them"
    )]
    fn draw(
        &self,
        python: Python<'_>,
        bindings: &Bound<'_, PyDict>,
        color_targets: &Bound<'_, PyAny>,
        extent: (u32, u32),
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
        push_constants: Option<&[u8]>,
    ) -> PyResult<()> {
        #[cfg(target_os = "linux")]
        {
            let push_constants = push_constants.unwrap_or_default();
            require_declared_push_constant_size(self.push_constant_size, push_constants)?;
            let target_surface_ids = PyList::empty(python);
            for (index, target) in color_targets.try_iter()?.enumerate() {
                target_surface_ids.append(bound_surface_id(
                    &format!("colour target {index}"),
                    &target?,
                )?)?;
            }
            if target_surface_ids.len() != 1 {
                return Err(PyValueError::new_err(format!(
                    "this draw names {} colour targets; the pipeline is built for exactly one \
                     colour attachment",
                    target_surface_ids.len()
                )));
            }
            let wire_bindings = supplied_kernel_bindings_to_wire(
                python,
                &self.reflected_binding_kinds,
                bindings,
                "surface_uuid",
            )?;
            self.helper_process_exchange_client.run_graphics_draw(
                python,
                &HelperProcessGraphicsDraw {
                    kernel_id: &self.kernel_id,
                    bindings: &wire_bindings,
                    color_target_surface_ids: &target_surface_ids,
                    push_constants_hex: &encode_lowercase_hex(push_constants),
                    vertex_count,
                    instance_count,
                    first_vertex,
                    first_instance,
                    extent_width: extent.0,
                    extent_height: extent.1,
                },
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (
                python,
                bindings,
                color_targets,
                extent,
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
                push_constants,
            );
            Err(gpu_unreachable_from_a_helper_process_error())
        }
    }
}

/// A ray-tracing kernel the engine built and holds, traced by name.
///
/// Constructed in `setup()` where the capability is Full; traced per frame in
/// `process()`. Like the other two kernel objects, nothing about the engine's
/// handle for it reaches Python.
///
/// Defined on every platform so the stub's surface is honest everywhere; off
/// Linux it is unconstructible, because `create_ray_tracing_kernel` refuses
/// before reaching it.
#[pyclass(name = "RayTracingKernel", module = "streamlib", frozen)]
pub(crate) struct PythonRayTracingKernel {
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    kernel_id: String,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    push_constant_size: u32,
    /// The caller supplies targets by name; which kind each name is, is the
    /// shaders' to say, and it is also what decides whether a name takes a
    /// surface or an acceleration structure.
    reflected_binding_kinds: Vec<ReflectedKernelBinding>,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    helper_process_exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

#[pymethods]
impl PythonRayTracingKernel {
    /// The shaders' own names for this kernel's bindings, in slot order.
    #[getter]
    fn binding_names(&self) -> Vec<String> {
        reflected_binding_names(&self.reflected_binding_kinds)
    }

    /// Trace a `(width, height, depth)` grid of rays, binding each of the
    /// shaders' declared resources by name.
    ///
    /// An `acceleration_structure` binding takes the handle `build_tlas`
    /// returned; every other kind takes a surface. Bindings never persist on
    /// the kernel, so every trace supplies all of them. Returns when the GPU
    /// work has retired and the writes are visible.
    #[pyo3(signature = (bindings, grid, push_constants = None))]
    fn trace(
        &self,
        python: Python<'_>,
        bindings: &Bound<'_, PyDict>,
        grid: (u32, u32, u32),
        push_constants: Option<&[u8]>,
    ) -> PyResult<()> {
        #[cfg(target_os = "linux")]
        {
            let push_constants = push_constants.unwrap_or_default();
            require_declared_push_constant_size(self.push_constant_size, push_constants)?;
            let wire_bindings = supplied_kernel_bindings_to_wire(
                python,
                &self.reflected_binding_kinds,
                bindings,
                "target_id",
            )?;
            self.helper_process_exchange_client.run_ray_tracing_kernel(
                python,
                &self.kernel_id,
                &wire_bindings,
                &encode_lowercase_hex(push_constants),
                grid,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (python, bindings, grid, push_constants);
            Err(gpu_unreachable_from_a_helper_process_error())
        }
    }
}

/// An acceleration structure the engine built and holds.
///
/// The object is the handle: a bottom-level structure is placed in a scene by
/// `build_tlas`, and the top-level one it returns is what a trace binds. No id
/// string reaches Python, and nothing publishes an acceleration structure for
/// another processor to resolve.
///
/// Defined on every platform so the stub's surface is honest everywhere; off
/// Linux it is unconstructible, because both builders refuse before reaching
/// it.
#[pyclass(name = "AccelerationStructureHandle", module = "streamlib", frozen)]
pub(crate) struct PythonAccelerationStructureHandle {
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    acceleration_structure_id: String,
    /// Which of the two builders minted this, so binding a bottom-level
    /// structure at a trace — or instancing a top-level one — refuses in the
    /// caller's own stack.
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    is_top_level: bool,
    structure_label: String,
    /// The release this handle owes the engine, paid on drop. `None` only in
    /// tests, which mint a handle without a parent to hand anything back to.
    #[cfg(target_os = "linux")]
    helper_process_exchange_client: Option<Arc<HelperProcessGpuExchangeClient>>,
}

#[cfg(target_os = "linux")]
impl Drop for PythonAccelerationStructureHandle {
    /// The engine holds a structure's device memory for as long as the handle
    /// naming it lives, which is the lifetime a Rust caller's
    /// `VulkanAccelerationStructure` has. A scene keeps every bottom-level
    /// structure it instances alive, so letting go of a BLAS a live TLAS uses
    /// frees nothing until the TLAS goes too.
    fn drop(&mut self) {
        let Some(exchange_client) = self.helper_process_exchange_client.take() else {
            return;
        };
        Python::attach(|python| {
            exchange_client.release_acceleration_structure(python, &self.acceleration_structure_id);
        });
    }
}

#[pymethods]
impl PythonAccelerationStructureHandle {
    /// The name this structure was built under, as it appears in engine logs.
    #[getter]
    fn label(&self) -> String {
        self.structure_label.clone()
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

/// The acceleration structure a value bound at `name` names.
///
/// The only binding kind that is not a surface, and the only one whose handle
/// cannot be spelled as an id string — nothing publishes an acceleration
/// structure for another processor to resolve, so the object a build returned
/// is the whole way to name it.
#[cfg(target_os = "linux")]
fn bound_acceleration_structure_id(name: &str, bound_to: &Bound<'_, PyAny>) -> PyResult<String> {
    let structure = bound_to
        .extract::<PyRef<'_, PythonAccelerationStructureHandle>>()
        .map_err(|_| {
            PyTypeError::new_err(format!(
                "binding {name:?} is an acceleration_structure; bind the handle `build_tlas` \
                 returned"
            ))
        })?;
    if !structure.is_top_level {
        return Err(PyValueError::new_err(format!(
            "binding {name:?} was given a bottom-level structure; a trace binds the top-level one \
             `build_tlas` returned, which is what holds the instances"
        )));
    }
    Ok(structure.acceleration_structure_id.clone())
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
                    &link_id,
                ),
            )
            .unwrap();

        // The capability a helper's context carries: the escalate callable is
        // never reached, because a claim speaks only to the surface socket.
        let exchange_client = Arc::new(HelperProcessGpuExchangeClient::new(
            python.None(),
            share.socket_path.clone(),
            "helper:read-under-test".to_string(),
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

/// What a caller can get wrong building a graphics or ray-tracing kernel,
/// refused before anything is sent.
///
/// Each of these travels as a plain field of a `#[serde(deny_unknown_fields)]`
/// escalate document, so a mistake the wheel forwards comes back as a parse
/// failure naming a wire field the author never wrote. Provable with no GPU:
/// nothing here reaches the exchange client.
#[cfg(all(test, target_os = "linux"))]
mod kernel_argument_tests {
    use super::*;

    fn wire_entries<'py>(wire: &Bound<'py, PyList>) -> Vec<Bound<'py, PyAny>> {
        wire.iter().collect()
    }

    fn wire_text(entry: &Bound<'_, PyAny>, field: &str) -> String {
        entry.get_item(field).unwrap().extract().unwrap()
    }

    fn wire_number(entry: &Bound<'_, PyAny>, field: &str) -> u32 {
        entry.get_item(field).unwrap().extract().unwrap()
    }

    fn bottom_level_structure(python: Python<'_>) -> Py<PythonAccelerationStructureHandle> {
        Py::new(
            python,
            PythonAccelerationStructureHandle {
                acceleration_structure_id: "blas-under-test".to_string(),
                is_top_level: false,
                structure_label: "floor".to_string(),
                helper_process_exchange_client: None,
            },
        )
        .unwrap()
    }

    /// A declaration asserts the kind; naming stages is optional, and naming
    /// none of them asserts nothing so reflection stands.
    #[test]
    fn a_binding_declaration_carries_the_stages_it_names_and_no_others() {
        Python::initialize();
        Python::attach(|python| {
            let declared = PyDict::new(python);
            declared
                .set_item("scene_texture", "sampled_texture")
                .unwrap();
            declared
                .set_item(
                    "output_image",
                    ("storage_image", vec!["vertex", "fragment"]),
                )
                .unwrap();

            let wire = declared_staged_kernel_bindings_to_wire(
                python,
                Some(&declared),
                "graphics binding kind",
                GRAPHICS_BINDING_KIND_WIRE_NAMES,
                "graphics stage",
                GRAPHICS_SHADER_STAGE_WIRE_BITS,
            )
            .unwrap();

            let entries = wire_entries(&wire);
            assert_eq!(wire_text(&entries[0], "name"), "scene_texture");
            assert_eq!(wire_text(&entries[0], "kind"), "sampled_texture");
            assert_eq!(
                wire_number(&entries[0], "stages"),
                0,
                "a declaration that names no stage must assert nothing about stages"
            );
            assert_eq!(wire_number(&entries[1], "stages"), 0b11);
        });
    }

    #[test]
    fn a_binding_kind_the_pipeline_does_not_have_is_refused_naming_the_set() {
        Python::initialize();
        Python::attach(|python| {
            let declared = PyDict::new(python);
            declared.set_item("scene_texture", "sampled_image").unwrap();
            let refusal = declared_staged_kernel_bindings_to_wire(
                python,
                Some(&declared),
                "graphics binding kind",
                GRAPHICS_BINDING_KIND_WIRE_NAMES,
                "graphics stage",
                GRAPHICS_SHADER_STAGE_WIRE_BITS,
            )
            .expect_err("a graphics pipeline has no samplerless-texture descriptor");
            let refusal = refusal.to_string();
            assert!(refusal.contains("sampled_image"), "{refusal}");
            assert!(refusal.contains("sampled_texture"), "{refusal}");
        });
    }

    #[test]
    fn a_stage_no_graphics_pipeline_runs_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let declared = PyDict::new(python);
            declared
                .set_item("output_image", ("storage_image", vec!["ray_gen"]))
                .unwrap();
            let refusal = declared_staged_kernel_bindings_to_wire(
                python,
                Some(&declared),
                "graphics binding kind",
                GRAPHICS_BINDING_KIND_WIRE_NAMES,
                "graphics stage",
                GRAPHICS_SHADER_STAGE_WIRE_BITS,
            )
            .expect_err("a graphics binding cannot be read from a ray-generation stage");
            assert!(refusal.to_string().contains("ray_gen"), "{refusal}");
        });
    }

    /// The wire has no way to omit a stage index, so a group that names none
    /// carries the sentinel — which is the wheel's job, not the author's.
    #[test]
    fn a_shader_group_fills_the_stages_it_does_not_name_with_the_sentinel() {
        Python::initialize();
        Python::attach(|python| {
            let hit_group = PyDict::new(python);
            hit_group.set_item("kind", "triangles_hit").unwrap();
            hit_group.set_item("closest_hit_stage", 1u32).unwrap();
            let groups = PyList::new(python, [hit_group]).unwrap();

            let wire = ray_tracing_shader_groups_to_wire(python, groups.as_any(), 2).unwrap();
            let entries = wire_entries(&wire);
            assert_eq!(wire_number(&entries[0], "closest_hit_stage"), 1);
            assert_eq!(
                wire_number(&entries[0], "any_hit_stage"),
                RAY_TRACING_STAGE_INDEX_NONE
            );
            assert_eq!(
                wire_number(&entries[0], "general_stage"),
                RAY_TRACING_STAGE_INDEX_NONE
            );
        });
    }

    #[test]
    fn a_shader_group_naming_a_module_that_was_not_supplied_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let group = PyDict::new(python);
            group.set_item("kind", "general").unwrap();
            group.set_item("general_stage", 4u32).unwrap();
            let groups = PyList::new(python, [group]).unwrap();

            let refusal = ray_tracing_shader_groups_to_wire(python, groups.as_any(), 2)
                .expect_err("a group cannot point past the modules it was built from");
            assert!(refusal.to_string().contains("general_stage 4"), "{refusal}");
        });
    }

    #[test]
    fn a_general_shader_group_that_names_no_module_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let group = PyDict::new(python);
            group.set_item("kind", "general").unwrap();
            let groups = PyList::new(python, [group]).unwrap();

            let refusal = ray_tracing_shader_groups_to_wire(python, groups.as_any(), 2)
                .expect_err("a general group is the module it points at");
            assert!(refusal.to_string().contains("general_stage"), "{refusal}");
        });
    }

    #[test]
    fn a_misspelled_group_key_is_refused_rather_than_silently_dropped() {
        Python::initialize();
        Python::attach(|python| {
            let group = PyDict::new(python);
            group.set_item("kind", "general").unwrap();
            group.set_item("general_stag", 0u32).unwrap();
            let groups = PyList::new(python, [group]).unwrap();

            let refusal = ray_tracing_shader_groups_to_wire(python, groups.as_any(), 1)
                .expect_err("a misspelled key would otherwise read as an absent one");
            assert!(refusal.to_string().contains("general_stag"), "{refusal}");
        });
    }

    /// An instance that names only its structure sits at the origin, visible
    /// to every cull mask — the placement a caller means by saying nothing.
    #[test]
    fn a_tlas_instance_that_names_only_its_structure_gets_the_conventional_placement() {
        Python::initialize();
        Python::attach(|python| {
            let instance = PyDict::new(python);
            instance
                .set_item("blas", bottom_level_structure(python))
                .unwrap();
            let instances = PyList::new(python, [instance]).unwrap();

            let wire = tlas_instances_to_wire(python, instances.as_any()).unwrap();
            let entries = wire_entries(&wire);
            assert_eq!(wire_text(&entries[0], "blas_id"), "blas-under-test");
            assert_eq!(wire_number(&entries[0], "mask"), 0xff);
            assert_eq!(wire_number(&entries[0], "custom_index"), 0);
            assert_eq!(wire_number(&entries[0], "flags"), 0);
            let transform: Vec<f32> = entries[0].get_item("transform").unwrap().extract().unwrap();
            assert_eq!(transform, IDENTITY_TLAS_INSTANCE_TRANSFORM.to_vec());
        });
    }

    #[test]
    fn a_tlas_instance_transform_that_is_not_a_three_by_four_affine_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let instance = PyDict::new(python);
            instance
                .set_item("blas", bottom_level_structure(python))
                .unwrap();
            instance.set_item("transform", vec![1.0f32; 16]).unwrap();
            let instances = PyList::new(python, [instance]).unwrap();

            let refusal = tlas_instances_to_wire(python, instances.as_any())
                .expect_err("a 4×4 transform is not what VkTransformMatrixKHR carries");
            assert!(refusal.to_string().contains("16 floats"), "{refusal}");
        });
    }

    /// The host masks the high byte off a custom index without saying so, so a
    /// value that would arrive truncated is refused where it was written.
    #[test]
    fn a_tlas_instance_custom_index_wider_than_its_24_bits_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let instance = PyDict::new(python);
            instance
                .set_item("blas", bottom_level_structure(python))
                .unwrap();
            instance.set_item("custom_index", 0x0100_0000u32).unwrap();
            let instances = PyList::new(python, [instance]).unwrap();

            let refusal = tlas_instances_to_wire(python, instances.as_any())
                .expect_err("a 25-bit custom index cannot reach a hit shader intact");
            assert!(refusal.to_string().contains("truncated"), "{refusal}");
        });
    }

    #[test]
    fn a_tlas_instance_naming_a_top_level_structure_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let top_level = Py::new(
                python,
                PythonAccelerationStructureHandle {
                    acceleration_structure_id: "tlas-under-test".to_string(),
                    is_top_level: true,
                    structure_label: "scene".to_string(),
                    helper_process_exchange_client: None,
                },
            )
            .unwrap();
            let instance = PyDict::new(python);
            instance.set_item("blas", top_level).unwrap();
            let instances = PyList::new(python, [instance]).unwrap();

            let refusal = tlas_instances_to_wire(python, instances.as_any())
                .expect_err("a scene cannot instance itself");
            assert!(refusal.to_string().contains("top-level"), "{refusal}");
        });
    }

    /// The other half of the same rule: a trace binds the top-level structure,
    /// and the bottom-level one it was built from is not a scene.
    #[test]
    fn binding_a_bottom_level_structure_at_a_trace_is_refused() {
        Python::initialize();
        Python::attach(|python| {
            let bottom_level = bottom_level_structure(python);
            let refusal =
                bound_acceleration_structure_id("scene", bottom_level.bind(python).as_any())
                    .expect_err("a trace binds the structure `build_tlas` returned");
            assert!(refusal.to_string().contains("bottom-level"), "{refusal}");
        });
    }

    #[test]
    fn a_colour_write_mask_names_its_channels() {
        assert_eq!(color_write_channels_to_wire("rgba").unwrap(), 0b1111);
        assert_eq!(color_write_channels_to_wire("rg").unwrap(), 0b0011);
        assert_eq!(color_write_channels_to_wire("").unwrap(), 0);
        let refusal = color_write_channels_to_wire("rgbx")
            .expect_err("a colour write mask names only rgba channels");
        assert!(refusal.to_string().contains('x'), "{refusal}");
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
            PixelFormat::Rgba16Float,
            PixelFormat::Rgba32Float,
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
