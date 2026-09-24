// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::{PyBufferError, PyNotImplementedError, PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use streamlib_adapter_cuda::dlpack::DeviceType;

#[cfg(target_os = "linux")]
use crate::python_gpu_surface_pixel_exchange::device_export_available;
use crate::python_gpu_surface_pixel_exchange::{
    CpuAccessGate, GpuSurfaceOwnedMemory, HOST_VISIBLE_DLPACK_DEVICE, dlpack_device_as_python_pair,
    exchange_shape_for_max_version, host_visible_dlpack_capsule,
    map_the_cpu_staging_without_reading_a_frame_in,
};
#[cfg(target_os = "macos")]
use crate::python_gpu_surface_pixel_exchange::{METAL_DLPACK_DEVICE, metal_dlpack_capsule};
#[cfg(target_os = "linux")]
use crate::python_gpu_surface_pixel_exchange::{
    StagedWriteBackSource, device_dlpack_capsule, imported_device_for, prepare_device_export,
    read_the_frame_into_its_cpu_staging,
};
#[cfg(target_os = "linux")]
use crate::python_helper_process_pixel_exchange::HelperAcquiredTexture;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_helper_process_pixel_exchange::HelperCheckedOutSurface;
#[cfg(target_os = "macos")]
use crate::python_metal_framework_queue_synchronization::drain_torch_mps_queue_if_imported;

use super::gpu_surface_device_tensor_scope::PythonGpuSurfaceDeviceTensorScope;
use super::left_by_a_propagating_exception;

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
    /// Whether this lock scope handed out a writable Metal capsule, whose
    /// stores the framework's queue must retire before the unlock returns.
    #[cfg(target_os = "macos")]
    a_writable_metal_capsule_went_out_this_lock_scope: std::sync::atomic::AtomicBool,
    /// Which DLPack side this handle serves when the consumer expresses
    /// no preference — decided once, so `__dlpack_device__` and
    /// `__dlpack__` cannot disagree across calls.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
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
            #[cfg(target_os = "macos")]
            a_writable_metal_capsule_went_out_this_lock_scope: std::sync::atomic::AtomicBool::new(
                false,
            ),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    fn open_the_cpu_door_over_this_frame(
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

    /// Open the CPU door over this frame, once per lock scope: take the
    /// IOSurface's lock with the intent `lock()` declared — what keeps the
    /// host view coherent on a discrete-GPU Mac.
    ///
    /// Taken here rather than in `lock()`, so a scope that reaches the pixels
    /// only through a Metal capsule never holds a CPU lock over GPU work.
    #[cfg(target_os = "macos")]
    fn open_the_cpu_door_over_this_frame(
        &self,
        _python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> PyResult<()> {
        owned_memory.lock_the_iosurface_for_cpu_access_once(self.cpu_access.is_read_only())
    }

    /// Settle what this lock scope left pending, before its gate opens.
    ///
    /// On Linux that publishes a staged edit through whichever staging holds
    /// it. On macOS it retires the stores a writable Metal capsule took, then
    /// lets the IOSurface's CPU lock go; both run whatever the other
    /// answered, and the first failure is returned with the second logged.
    #[cfg(target_os = "linux")]
    fn settle_this_lock_scopes_pending_writes(&self, python: Python<'_>) -> PyResult<()> {
        self.publish_pending_staged_write(python)
    }

    #[cfg(target_os = "macos")]
    fn settle_this_lock_scopes_pending_writes(&self, python: Python<'_>) -> PyResult<()> {
        let metal_writes_retired = if self
            .a_writable_metal_capsule_went_out_this_lock_scope
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            drain_torch_mps_queue_if_imported(python)
        } else {
            Ok(())
        };
        // Bound before the match, so the guard drops before the unlock.
        let owned_memory = self.owned_memory.lock().clone();
        let iosurface_cpu_lock_released = match owned_memory {
            Some(owned_memory) => owned_memory.unlock_the_iosurface_after_cpu_access(),
            None => Ok(()),
        };
        match (metal_writes_retired, iosurface_cpu_lock_released) {
            (Err(retire_failure), Err(unlock_failure)) => {
                tracing::warn!(
                    "releasing surface {:?}'s IOSurface CPU lock also failed: {unlock_failure}",
                    self.minted_surface_id
                );
                Err(retire_failure)
            }
            (metal_writes_retired, iosurface_cpu_lock_released) => {
                metal_writes_retired.and(iosurface_cpu_lock_released)
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn settle_this_lock_scopes_pending_writes(&self, _python: Python<'_>) -> PyResult<()> {
        Ok(())
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
    pub(super) fn from_helper_acquired_texture(acquired: HelperAcquiredTexture) -> Self {
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn from_helper_checked_out_surface(checked_out: HelperCheckedOutSurface) -> Self {
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
    pub(super) fn owned_memory(&self) -> PyResult<Arc<GpuSurfaceOwnedMemory>> {
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

    /// Decide — once per handle — whether the no-preference DLPack side is
    /// Metal: it is wherever the helper's device exports a no-copy
    /// `MTLBuffer` over the surface's IOSurface.
    #[cfg(target_os = "macos")]
    fn natural_side_is_device(
        &self,
        _python: Python<'_>,
        owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> bool {
        *self
            .natural_dlpack_side_is_device
            .get_or_init(|| owned_memory.metal_buffer_over_the_iosurface_pages().is_ok())
    }

    /// No surface exchange exists here, so every handle serves its host side.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn natural_side_is_device(
        &self,
        _python: Python<'_>,
        _owned_memory: &Arc<GpuSurfaceOwnedMemory>,
    ) -> bool {
        false
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
    pub(super) fn surface_id(&self) -> PyResult<String> {
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.open_the_cpu_door_over_this_frame(python, &owned_memory)?;
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
        let settle_outcome = self.settle_this_lock_scopes_pending_writes(python);
        python.detach(|| {
            self.cpu_access.unlock();
            self.release_owned_engine_value();
        });
        settle_outcome?;
        Ok(())
    }

    fn __enter__(python_self: PyRef<'_, Self>) -> PyRef<'_, Self> {
        python_self
    }

    /// Leaving normally publishes any pending device write via `close`;
    /// leaving by a propagating exception discards a staged one first — the
    /// write did not finish, and the surface keeps the frame it already held.
    /// A write that lands in the surface itself (a macOS Metal capsule) has
    /// nothing to discard, so its queued stores still retire before the
    /// scope closes. `False` never suppresses the raise, and a close that
    /// fails under a raise is logged rather than replacing it.
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
        let closed = self.close(python);
        if left_by_a_propagating_exception(exception_type) {
            if let Err(close_failure) = closed {
                tracing::warn!(
                    "closing surface {:?} under a propagating exception failed: {close_failure}",
                    self.minted_surface_id
                );
            }
            return Ok(false);
        }
        closed?;
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
        // A lock over a lock closes the first scope's doors: its Metal writes
        // retire and its IOSurface lock goes, so this scope opens its own.
        #[cfg(target_os = "macos")]
        self.settle_this_lock_scopes_pending_writes(python)?;
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

    /// Close CPU access, publishing any pending write first — through
    /// whichever staging holds the edit on Linux, by retiring the Metal
    /// frameworks' queues on macOS. Idempotent.
    fn unlock(&self, python: Python<'_>) -> PyResult<()> {
        // The gate opens whether or not the publish succeeded — a
        // surface left locked after a failed publish would refuse
        // every later access with a message about locking, hiding
        // the real failure this raises.
        let settle_outcome = self.settle_this_lock_scopes_pending_writes(python);
        python.detach(|| self.cpu_access.unlock());
        settle_outcome?;
        Ok(())
    }

    /// The DLPack device this surface's tensors live on.
    ///
    /// On Linux a device-exchange surface answers with the CUDA device its
    /// memory is imported onto, performing the import if it has not happened
    /// yet — the driver's classification of the pointer is what distinguishes
    /// true device memory from a downgrade to pinned host memory, and
    /// guessing here would contradict the capsule `__dlpack__` goes on to
    /// hand back. On macOS it answers `kDLMetal` wherever the surface's
    /// IOSurface exports a no-copy `MTLBuffer`.
    fn __dlpack_device__(&self, python: Python<'_>) -> PyResult<(i32, i32)> {
        let owned_memory = self.owned_memory()?;
        // Routed through the same once-per-handle decision `__dlpack__`
        // serves, so a probe failure here (answered CPU) cannot be
        // followed by a successful device capsule there.
        if self.natural_side_is_device(python, &owned_memory) {
            #[cfg(target_os = "linux")]
            {
                return Ok(dlpack_device_as_python_pair(imported_device_for(
                    python,
                    &owned_memory,
                )?));
            }
            #[cfg(target_os = "macos")]
            return Ok(dlpack_device_as_python_pair(METAL_DLPACK_DEVICE));
        }
        Ok(dlpack_device_as_python_pair(HOST_VISIBLE_DLPACK_DEVICE))
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
        // consumer told the device must never be handed a host capsule.
        let wants_host = match dl_device {
            Some((device_type, _)) => device_type == DeviceType::Cpu as i32,
            None => !self.natural_side_is_device(python, &owned_memory),
        };
        if wants_host {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            self.open_the_cpu_door_over_this_frame(python, &owned_memory)?;
            return host_visible_dlpack_capsule(python, &owned_memory, exchange_shape, read_only);
        }
        #[cfg(target_os = "linux")]
        {
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
        #[cfg(target_os = "macos")]
        {
            // A write lands in the surface itself once the framework's queue
            // retires it, so a writable capsule is what obliges the unlock
            // to drain that queue.
            if !read_only {
                self.a_writable_metal_capsule_went_out_this_lock_scope
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            metal_dlpack_capsule(python, &owned_memory, exchange_shape, read_only)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        host_visible_dlpack_capsule(python, &owned_memory, exchange_shape, read_only)
    }

    /// The scoped device-tensor view over this surface's pixels, which a
    /// third-party GPU package writes in place — each floor's write rule is
    /// [`PythonGpuSurfaceDeviceTensorScope`]'s. Construction does no GPU
    /// work; entering does.
    ///
    /// Independent of `lock()` by design: entering the scope *is* the
    /// write declaration, structurally, so it neither requires nor
    /// consults the CPU access gate — that gate belongs to the
    /// handle-level `lock()` + `__dlpack__` spelling.
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
