// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The capability-typed runtime contexts handed to Python lifecycle hooks.
//!
//! The engine hands hooks lifetime-bound views (`&RuntimeContextFullAccess`)
//! that a Python object cannot hold. The bridge is a lease: the host erases
//! the borrow behind a pointer, installs it before invoking the hook, and
//! revokes it after — the revoke's write-lock acquisition blocks until every
//! in-flight reader finishes, so the pointer provably never outlives the
//! engine borrow. Lock/GIL discipline, kept everywhere in this file: lease
//! installs and revokes happen with no GIL attached, and every reader takes
//! the guard inside a `python.detach(..)` closure and releases it before
//! re-attaching — never hold a lease guard while attached to or waiting for
//! the GIL.

use std::ptr::NonNull;
use std::sync::{Arc, OnceLock};

use parking_lot::{Mutex, RwLock};
use pyo3::exceptions::{
    PyBufferError, PyNotImplementedError, PyRuntimeError, PyTypeError, PyValueError,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib::sdk::context::{
    GpuContextFullAccess, GpuContextLimitedAccess, PooledTextureHandle, RuntimeContextFullAccess,
    RuntimeContextLimitedAccess, SurfaceStore, TexturePoolDescriptor,
};
use streamlib::sdk::rhi::{
    PixelBuffer, PixelBufferPoolId, PixelFormat, TextureFormat, TextureUsages,
};
use streamlib_adapter_cuda::dlpack::DeviceType;

use crate::python_bag_conversion::json_value_to_python_object;
use crate::python_gpu_surface_pixel_exchange::{
    CpuAccessGate, DeviceExchangeBuffer, GpuSurfaceOwnedMemory, GpuSurfaceOwnedValue,
    HOST_VISIBLE_DLPACK_DEVICE, exchange_shape_for_max_version, host_visible_dlpack_capsule,
    is_device_exchange, pixel_buffer_bytes_per_row,
};
#[cfg(target_os = "linux")]
use crate::python_gpu_surface_pixel_exchange::{device_dlpack_capsule, imported_device_for};
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

// =============================================================================
// The lease: a lifetime-erased view pointer behind a lock
// =============================================================================

struct FullAccessRuntimeContextViewPointer(NonNull<RuntimeContextFullAccess<'static>>);

// SAFETY: the engine view is Send + Sync; the pointer is dereferenced only
// under the lease's read guard, and the host's revoke (a write-lock
// acquisition) blocks until every reader is done, so the deref never outlives
// the engine borrow the pointer erased.
unsafe impl Send for FullAccessRuntimeContextViewPointer {}
unsafe impl Sync for FullAccessRuntimeContextViewPointer {}

impl FullAccessRuntimeContextViewPointer {
    fn from_engine_view(view: &RuntimeContextFullAccess<'_>) -> Self {
        Self(NonNull::from(view).cast())
    }
}

struct LimitedAccessRuntimeContextViewPointer(NonNull<RuntimeContextLimitedAccess<'static>>);

// SAFETY: same argument as `FullAccessRuntimeContextViewPointer`.
unsafe impl Send for LimitedAccessRuntimeContextViewPointer {}
unsafe impl Sync for LimitedAccessRuntimeContextViewPointer {}

impl LimitedAccessRuntimeContextViewPointer {
    fn from_engine_view(view: &RuntimeContextLimitedAccess<'_>) -> Self {
        Self(NonNull::from(view).cast())
    }
}

type FullAccessRuntimeContextViewLease = Arc<RwLock<Option<FullAccessRuntimeContextViewPointer>>>;
type LimitedAccessRuntimeContextViewLease =
    Arc<RwLock<Option<LimitedAccessRuntimeContextViewPointer>>>;

fn expired_context_error() -> PyErr {
    PyRuntimeError::new_err(
        "this capability is only valid during the lifecycle hook or escalate callback that \
         received it",
    )
}

fn context_not_yet_activated_error() -> PyErr {
    PyRuntimeError::new_err(
        "this context becomes usable once the processor's first lifecycle hook runs",
    )
}

/// Run `read_view` against the leased full-access view, detached from the GIL.
fn read_full_access_view<T: Send>(
    python: Python<'_>,
    lease: &FullAccessRuntimeContextViewLease,
    read_view: impl FnOnce(&RuntimeContextFullAccess<'static>) -> PyResult<T> + Send,
) -> PyResult<T> {
    python.detach(|| {
        let guard = lease.read();
        let Some(view_pointer) = guard.as_ref() else {
            return Err(expired_context_error());
        };
        // SAFETY: the pointer is present only between the host's install and
        // revoke; revoke blocks on this read guard, so the borrow is live.
        read_view(unsafe { view_pointer.0.as_ref() })
    })
}

/// Run `read_view` against the leased limited-access view, detached from the GIL.
fn read_limited_access_view<T: Send>(
    python: Python<'_>,
    lease: &LimitedAccessRuntimeContextViewLease,
    read_view: impl FnOnce(&RuntimeContextLimitedAccess<'static>) -> PyResult<T> + Send,
) -> PyResult<T> {
    python.detach(|| {
        let guard = lease.read();
        let Some(view_pointer) = guard.as_ref() else {
            return Err(expired_context_error());
        };
        // SAFETY: same protocol as `read_full_access_view`.
        read_view(unsafe { view_pointer.0.as_ref() })
    })
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
        }
    }

    fn from_acquired_pixel_buffer(
        minted_surface_id: String,
        surface_store_owing_a_release: Option<SurfaceStore>,
        pixel_buffer: PixelBuffer,
    ) -> Self {
        let (width, height, format) = (
            pixel_buffer.width,
            pixel_buffer.height,
            pixel_buffer.format(),
        );
        Self::new(
            Some(minted_surface_id.clone()),
            width,
            height,
            pixel_format_name(format).to_string(),
            GpuSurfaceOwnedMemory::new(
                GpuSurfaceOwnedValue::PixelBuffer(pixel_buffer),
                surface_store_owing_a_release,
                Some(minted_surface_id),
            ),
        )
    }

    fn from_resolved_pixel_buffer(surface_id: String, pixel_buffer: PixelBuffer) -> Self {
        let (width, height, format) = (
            pixel_buffer.width,
            pixel_buffer.height,
            pixel_buffer.format(),
        );
        Self::new(
            Some(surface_id),
            width,
            height,
            pixel_format_name(format).to_string(),
            // A resolved handle owes no release: it never checked the buffer
            // in, so releasing would evict somebody else's surface.
            GpuSurfaceOwnedMemory::new(GpuSurfaceOwnedValue::PixelBuffer(pixel_buffer), None, None),
        )
    }

    fn from_pooled_texture(pooled_texture: PooledTextureHandle) -> Self {
        let (width, height, format) = (
            pooled_texture.width(),
            pooled_texture.height(),
            pooled_texture.format(),
        );
        Self::new(
            None,
            width,
            height,
            texture_format_name(format).to_string(),
            GpuSurfaceOwnedMemory::new(
                GpuSurfaceOwnedValue::PooledTexture(pooled_texture),
                None,
                None,
            ),
        )
    }

    /// Drop this handle's share of the owned memory. The engine resource goes
    /// away once the last outstanding DLPack capsule does too; idempotent.
    fn release_owned_engine_value(&self) {
        drop(self.owned_memory.lock().take());
    }

    /// Borrow the shared memory anchor, or fail if the handle is closed.
    fn owned_memory(&self) -> PyResult<Arc<GpuSurfaceOwnedMemory>> {
        self.owned_memory.lock().clone().ok_or_else(|| {
            PyRuntimeError::new_err("this surface is closed; acquire or resolve it again")
        })
    }
}

impl Drop for PythonGpuSurfaceHandle {
    /// Covers a handle the author never closed. Runs attached (pyclass
    /// deallocation) — acceptable because the engine takes no GIL, so the
    /// release cannot deadlock; `close()` remains the detached fast path.
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
                "this pooled texture carries no surface id: publishing one registers it with the \
                 surface store, which a texture acquire does not do",
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
        python.detach(|| pixel_buffer_bytes_per_row(owned_memory.host_mapped_pixel_buffer()?))
    }

    /// Base address of the host mapping, or `0` when the surface is not
    /// locked. Callers that want a typed view use `as_numpy` or `__dlpack__`;
    /// this is the escape hatch for building one by hand.
    #[getter]
    fn base_address(&self, python: Python<'_>) -> PyResult<usize> {
        if !self.cpu_access.is_locked() {
            return Ok(0);
        }
        let owned_memory = self.owned_memory()?;
        python.detach(|| {
            Ok(owned_memory
                .host_mapped_pixel_buffer()?
                .plane_base_address(0) as usize)
        })
    }

    /// Release the underlying GPU resource. Idempotent.
    fn close(&self, python: Python<'_>) {
        // Releasing can return a slot to a pool under engine locks and talk to
        // the surface-share daemon — detached, like every potentially-blocking
        // engine call.
        python.detach(|| {
            self.cpu_access.unlock();
            self.release_owned_engine_value();
        });
    }

    fn __enter__(python_self: PyRef<'_, Self>) -> PyRef<'_, Self> {
        python_self
    }

    #[pyo3(signature = (*_exception_details))]
    fn __exit__(&self, python: Python<'_>, _exception_details: &Bound<'_, PyAny>) -> bool {
        self.close(python);
        false
    }

    /// Open CPU access to the pixels, waiting for the producer to finish
    /// writing them first.
    #[pyo3(signature = (read_only = true))]
    fn lock(&self, python: Python<'_>, read_only: bool) -> PyResult<()> {
        let owned_memory = self.owned_memory()?;
        python.detach(|| -> PyResult<()> {
            // A device-only surface has no CPU mapping to open, but its
            // export gate is the same one — so the lock is recorded and
            // the host accessors refuse on their own terms.
            if !is_device_exchange(&owned_memory)
                && owned_memory
                    .host_mapped_pixel_buffer()?
                    .plane_base_address(0)
                    .is_null()
            {
                return Err(PyBufferError::new_err(
                    "surface has no host mapping; it is a DEVICE_LOCAL allocation",
                ));
            }
            self.cpu_access.lock_for(read_only);
            Ok(())
        })
    }

    /// Close CPU access. Idempotent.
    #[pyo3(signature = (read_only = true))]
    fn unlock(&self, python: Python<'_>, read_only: bool) {
        let _ = read_only;
        python.detach(|| self.cpu_access.unlock());
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
        #[cfg(target_os = "linux")]
        if is_device_exchange(&owned_memory) {
            let device = python.detach(|| imported_device_for(&owned_memory))?;
            return Ok((device.device_type as i32, device.device_id));
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (python, &owned_memory);
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
            return Err(PyValueError::new_err(
                "this surface exports in place; ask the consumer to copy the tensor instead",
            ));
        }
        self.cpu_access.require_locked()?;
        let owned_memory = self.owned_memory()?;
        let exchange_shape = exchange_shape_for_max_version(max_version);
        let read_only = self.cpu_access.is_read_only();

        // `dl_device` is the consumer's request for a particular side of a
        // surface that has two. Absent means "wherever you naturally live",
        // which for an exchange buffer is the device it was allocated for.
        let wants_host = match dl_device {
            Some((device_type, _)) => device_type == DeviceType::Cpu as i32,
            None => !is_device_exchange(&owned_memory),
        };
        if wants_host {
            return host_visible_dlpack_capsule(python, &owned_memory, exchange_shape, read_only);
        }
        #[cfg(target_os = "linux")]
        {
            device_dlpack_capsule(python, &owned_memory, exchange_shape, read_only)
        }
        #[cfg(not(target_os = "linux"))]
        Err(PyValueError::new_err(
            "device export is available on Linux only",
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
                if from_dlpack_failure.is_instance_of::<PyTypeError>(python) {
                    return PyRuntimeError::new_err(
                        "as_numpy needs numpy 2.1 or newer, whose `from_dlpack` accepts a \
                         `device` request; older numpy cannot ask for the host side of a \
                         surface",
                    );
                }
                from_dlpack_failure
            })
    }
}

// =============================================================================
// GPU capability views
// =============================================================================

fn gpu_operation_error(failure: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(failure.to_string())
}

/// The surface id a freshly-acquired pixel buffer travels under: the
/// surface-share check-in when the store exists (so any process can resolve
/// it), the pool id otherwise — the same rule the subprocess escalate
/// handler applied.
///
/// A check-in comes back with the store that performed it, because it parks a
/// strong clone of the buffer there — the handle owes that store a release.
fn mint_pixel_buffer_surface_id(
    surface_store: Option<SurfaceStore>,
    pool_id: &PixelBufferPoolId,
    pixel_buffer: &PixelBuffer,
) -> PyResult<(String, Option<SurfaceStore>)> {
    match surface_store {
        Some(store) => {
            let minted_surface_id = store.check_in(pixel_buffer).map_err(gpu_operation_error)?;
            Ok((minted_surface_id, Some(store)))
        }
        None => Ok((pool_id.as_str().to_string(), None)),
    }
}

/// Non-allocating GPU capability, valid for the whole processor life.
///
/// Holds an owned clone of the engine's `GpuContextLimitedAccess` — the one
/// capability the engine documents as stash-safe — populated at the first
/// lifecycle hook.
#[pyclass(name = "GpuContextLimitedAccess", module = "streamlib", frozen)]
pub(crate) struct PythonGpuContextLimitedAccess {
    owned_engine_view: OnceLock<GpuContextLimitedAccess>,
}

impl PythonGpuContextLimitedAccess {
    fn new_unprimed() -> Self {
        Self {
            owned_engine_view: OnceLock::new(),
        }
    }

    fn prime_from_engine_view(&self, engine_view: GpuContextLimitedAccess) {
        let _ = self.owned_engine_view.set(engine_view);
    }

    fn engine_view(&self) -> PyResult<&GpuContextLimitedAccess> {
        self.owned_engine_view
            .get()
            .ok_or_else(context_not_yet_activated_error)
    }
}

#[pymethods]
impl PythonGpuContextLimitedAccess {
    /// Acquire a pixel buffer from the pre-reserved pool.
    #[pyo3(signature = (width, height, format = "bgra"))]
    fn acquire_pixel_buffer(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        format: &str,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let pixel_format = parse_pixel_format_name(format)?;
        python.detach(|| {
            let engine_view = self.engine_view()?;
            let (pool_id, pixel_buffer) = engine_view
                .acquire_pixel_buffer(width, height, pixel_format)
                .map_err(gpu_operation_error)?;
            let (minted_surface_id, surface_store_owing_a_release) =
                mint_pixel_buffer_surface_id(engine_view.surface_store(), &pool_id, &pixel_buffer)?;
            Ok(PythonGpuSurfaceHandle::from_acquired_pixel_buffer(
                minted_surface_id,
                surface_store_owing_a_release,
                pixel_buffer,
            ))
        })
    }

    /// Acquire a pooled texture from the pre-reserved pool.
    fn acquire_texture(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        format: &str,
        usage: Vec<String>,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let texture_format = parse_texture_format_name(format)?;
        let texture_usages = parse_texture_usage_names(&usage)?;
        python.detach(|| {
            let engine_view = self.engine_view()?;
            let descriptor = TexturePoolDescriptor::new(width, height, texture_format)
                .with_usage(texture_usages);
            let pooled_texture = engine_view
                .acquire_texture(&descriptor)
                .map_err(gpu_operation_error)?;
            Ok(PythonGpuSurfaceHandle::from_pooled_texture(pooled_texture))
        })
    }

    /// Run `privileged_callback` with a temporary full-access GPU capability.
    ///
    /// The in-process door for one-shot privileged construction from a worker
    /// thread — the pattern every native capture processor uses: stash this
    /// object in `setup`, spawn a thread in `start`, escalate exactly once for
    /// resource construction, run per-frame work on the limited surface. The
    /// engine's escalate gate serializes all escalations runtime-wide and
    /// waits for device idle afterwards, so this is for setup-shaped moments,
    /// never per-frame — and it must never nest: an escalate inside an
    /// escalate on one thread is a same-thread gate re-entry, which the
    /// engine refuses by construction.
    ///
    /// Returns whatever the callback returns. The capability object handed to
    /// the callback expires when the callback does — stashing it and calling
    /// it later raises rather than granting privileged access forever.
    fn escalate(
        &self,
        python: Python<'_>,
        privileged_callback: &Bound<'_, PyAny>,
    ) -> PyResult<Py<PyAny>> {
        let engine_view = self.engine_view()?.clone();
        let held_callback = privileged_callback.clone().unbind();
        // Attaching while the escalate gate is held cannot deadlock here:
        // every gate-entering call in this crate detaches before reaching the
        // engine, so no thread ever waits on the gate while holding the GIL.
        python.detach(move || {
            let escalate_outcome: Result<PyResult<Py<PyAny>>, _> =
                engine_view.escalate(|gpu_full_access| {
                    let callback_lease: EscalatedGpuFullAccessViewLease =
                        Arc::new(RwLock::new(Some(EscalatedGpuFullAccessViewPointer(
                            NonNull::from(gpu_full_access),
                        ))));
                    let callback_outcome = Python::attach(|python| {
                        let escalated_capability = Py::new(
                            python,
                            PythonGpuContextFullAccess {
                                gpu_full_access_source: GpuFullAccessSource::EscalateCallback(
                                    Arc::clone(&callback_lease),
                                ),
                            },
                        )?;
                        held_callback.call1(python, (escalated_capability,))
                    });
                    // Revoked detached, blocking until every in-flight reader
                    // finishes — the pointer never outlives the closure's
                    // borrow. A callback failure still reaches this line.
                    *callback_lease.write() = None;
                    Ok(callback_outcome)
                });
            match escalate_outcome {
                Ok(callback_outcome) => callback_outcome,
                Err(escalate_failure) => Err(gpu_operation_error(escalate_failure)),
            }
        })
    }

    /// Resolve a surface id another processor published into a handle.
    fn resolve_surface(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        python.detach(|| {
            let engine_view = self.engine_view()?;
            let pixel_buffer = engine_view
                .resolve_pixel_buffer_by_surface_id(surface_id)
                .map_err(gpu_operation_error)?;
            Ok(PythonGpuSurfaceHandle::from_resolved_pixel_buffer(
                surface_id.to_string(),
                pixel_buffer,
            ))
        })
    }
}

struct EscalatedGpuFullAccessViewPointer(NonNull<GpuContextFullAccess>);

// SAFETY: same protocol as the runtime-context view pointers — dereferenced
// only under its lease's read guard, revoked (write-lock, blocking on
// readers) before the engine borrow it erased ends.
unsafe impl Send for EscalatedGpuFullAccessViewPointer {}
unsafe impl Sync for EscalatedGpuFullAccessViewPointer {}

type EscalatedGpuFullAccessViewLease = Arc<RwLock<Option<EscalatedGpuFullAccessViewPointer>>>;

/// Where a full-access GPU capability object borrows its engine handle from.
///
/// One pyclass for both doors so the op surface never forks: a full-phase
/// hook's `ctx.gpu_full_access` and the object an `escalate` callback
/// receives are the same class, differing only in which lease scopes them.
enum GpuFullAccessSource {
    /// `ctx.gpu_full_access` in a setup/teardown/start/stop hook — scoped to
    /// that hook's runtime-view lease.
    FullAccessHook(FullAccessRuntimeContextViewLease),
    /// The handle an [`escalate`](PythonGpuContextLimitedAccess::escalate)
    /// callback receives — scoped to the escalate closure.
    EscalateCallback(EscalatedGpuFullAccessViewLease),
}

/// Run `use_gpu_full_access` against whichever lease backs this capability,
/// detached from the GIL.
fn read_gpu_full_access<T: Send>(
    python: Python<'_>,
    source: &GpuFullAccessSource,
    use_gpu_full_access: impl FnOnce(&GpuContextFullAccess) -> PyResult<T> + Send,
) -> PyResult<T> {
    match source {
        GpuFullAccessSource::FullAccessHook(lease) => {
            read_full_access_view(python, lease, |view| {
                use_gpu_full_access(view.gpu_full_access())
            })
        }
        GpuFullAccessSource::EscalateCallback(lease) => python.detach(|| {
            let guard = lease.read();
            let Some(view_pointer) = guard.as_ref() else {
                return Err(expired_context_error());
            };
            // SAFETY: same protocol as `read_full_access_view`.
            use_gpu_full_access(unsafe { view_pointer.0.as_ref() })
        }),
    }
}

/// Privileged GPU capability — a full-access hook's, or an escalate callback's.
#[pyclass(name = "GpuContextFullAccess", module = "streamlib", frozen)]
pub(crate) struct PythonGpuContextFullAccess {
    gpu_full_access_source: GpuFullAccessSource,
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
        read_gpu_full_access(python, &self.gpu_full_access_source, |gpu_full_access| {
            let (pool_id, pixel_buffer) = gpu_full_access
                .acquire_pixel_buffer(width, height, pixel_format)
                .map_err(gpu_operation_error)?;
            let (minted_surface_id, surface_store_owing_a_release) = mint_pixel_buffer_surface_id(
                gpu_full_access.surface_store(),
                &pool_id,
                &pixel_buffer,
            )?;
            Ok(PythonGpuSurfaceHandle::from_acquired_pixel_buffer(
                minted_surface_id,
                surface_store_owing_a_release,
                pixel_buffer,
            ))
        })
    }

    /// Acquire a pooled texture through the privileged path.
    fn acquire_texture(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        format: &str,
        usage: Vec<String>,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let texture_format = parse_texture_format_name(format)?;
        let texture_usages = parse_texture_usage_names(&usage)?;
        read_gpu_full_access(python, &self.gpu_full_access_source, |gpu_full_access| {
            let descriptor = TexturePoolDescriptor::new(width, height, texture_format)
                .with_usage(texture_usages);
            let pooled_texture = gpu_full_access
                .acquire_texture(&descriptor)
                .map_err(gpu_operation_error)?;
            Ok(PythonGpuSurfaceHandle::from_pooled_texture(pooled_texture))
        })
    }

    /// Acquire a buffer whose memory external device code can import.
    ///
    /// The engine allocates it OPAQUE_FD-exportable — the handle flavour CUDA
    /// imports — rather than from the ordinary pixel-buffer pool, whose
    /// allocations are DMA-BUF-flavoured and have no OPAQUE_FD export path
    /// under VMA's per-pool memory configuration. `device_local=False` keeps a
    /// host mapping alongside, so the same bytes serve numpy and CUDA.
    ///
    /// Privileged and setup-shaped: allocate once and keep the handle, never
    /// per frame.
    #[cfg(target_os = "linux")]
    #[pyo3(signature = (width, height, format = "bgra", device_local = true))]
    fn acquire_device_exchange_buffer(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        format: &str,
        device_local: bool,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let pixel_format = parse_pixel_format_name(format)?;
        let bytes_per_pixel = device_exchange_bytes_per_pixel(pixel_format)?;
        let byte_size = u64::from(width) * u64::from(height) * u64::from(bytes_per_pixel);
        if byte_size == 0 {
            return Err(PyValueError::new_err(
                "width and height must both be greater than zero",
            ));
        }
        read_gpu_full_access(python, &self.gpu_full_access_source, |gpu_full_access| {
            let storage_buffer = gpu_full_access
                .create_opaque_fd_export_buffer(byte_size, device_local)
                .map_err(gpu_operation_error)?;
            // One allocation, two views: the pixel-shaped wrapper shares the
            // storage buffer's `Arc<HostVulkanBuffer>` rather than copying.
            let pixel_buffer = gpu_full_access
                .wrap_storage_buffer_as_pixel_buffer(
                    &storage_buffer,
                    width,
                    height,
                    bytes_per_pixel,
                    pixel_format,
                )
                .map_err(gpu_operation_error)?;
            // Exported here because exporting is privileged and `__dlpack__`
            // is not. The fd is consumed by the first CUDA import.
            let (opaque_fd, exported_size, vulkan_device_uuid) = gpu_full_access
                .export_storage_buffer_opaque_fd(&storage_buffer)
                .map_err(gpu_operation_error)?;
            Ok(PythonGpuSurfaceHandle::new(
                None,
                width,
                height,
                pixel_format_name(pixel_format).to_string(),
                GpuSurfaceOwnedMemory::new(
                    GpuSurfaceOwnedValue::DeviceExchange(DeviceExchangeBuffer::new(
                        pixel_buffer,
                        device_local,
                        opaque_fd,
                        exported_size,
                        vulkan_device_uuid,
                    )),
                    None,
                    None,
                ),
            ))
        })
    }

    /// Export a DMA-BUF file descriptor for `surface`, for native code that
    /// speaks DMA-BUF — EGL, a V4L2 output device, another process.
    ///
    /// Returns `(fd, byte_size)`. **The caller owns the fd** and must close
    /// it, or hand it to something that takes ownership. Only a surface from
    /// the ordinary pixel-buffer pool can answer: a device-exchange buffer is
    /// OPAQUE_FD-flavoured and has no DMA-BUF export path.
    #[cfg(target_os = "linux")]
    fn export_dma_buf(
        &self,
        python: Python<'_>,
        surface: &PythonGpuSurfaceHandle,
    ) -> PyResult<(i32, u64)> {
        let owned_memory = surface.owned_memory()?;
        read_gpu_full_access(python, &self.gpu_full_access_source, |gpu_full_access| {
            let pixel_buffer = owned_memory.dma_buf_exportable_pixel_buffer()?;
            gpu_full_access
                .export_pixel_buffer_dma_buf_fd(pixel_buffer)
                .map_err(gpu_operation_error)
        })
    }

    /// Import a DMA-BUF file descriptor as a surface this graph can read.
    ///
    /// **Takes ownership of `fd` on success** — the driver adopts it, and it
    /// must not be closed afterwards. On failure the fd is still the
    /// caller's.
    #[cfg(target_os = "linux")]
    #[pyo3(signature = (fd, width, height, format = "bgra"))]
    fn import_dma_buf(
        &self,
        python: Python<'_>,
        fd: i32,
        width: u32,
        height: u32,
        format: &str,
    ) -> PyResult<PythonGpuSurfaceHandle> {
        let pixel_format = parse_pixel_format_name(format)?;
        let bytes_per_pixel = device_exchange_bytes_per_pixel(pixel_format)?;
        let byte_size = u64::from(width) * u64::from(height) * u64::from(bytes_per_pixel);
        if byte_size == 0 {
            return Err(PyValueError::new_err(
                "width and height must both be greater than zero",
            ));
        }
        read_gpu_full_access(python, &self.gpu_full_access_source, |gpu_full_access| {
            let storage_buffer = gpu_full_access
                .import_dma_buf_storage_buffer(fd, byte_size)
                .map_err(gpu_operation_error)?;
            let pixel_buffer = gpu_full_access
                .wrap_storage_buffer_as_pixel_buffer(
                    &storage_buffer,
                    width,
                    height,
                    bytes_per_pixel,
                    pixel_format,
                )
                .map_err(gpu_operation_error)?;
            Ok(PythonGpuSurfaceHandle::new(
                None,
                width,
                height,
                pixel_format_name(pixel_format).to_string(),
                GpuSurfaceOwnedMemory::new(
                    GpuSurfaceOwnedValue::PixelBuffer(pixel_buffer),
                    None,
                    None,
                ),
            ))
        })
    }

    /// Block until the GPU device is idle.
    fn wait_device_idle(&self, python: Python<'_>) -> PyResult<()> {
        read_gpu_full_access(python, &self.gpu_full_access_source, |gpu_full_access| {
            gpu_full_access
                .wait_device_idle()
                .map_err(gpu_operation_error)
        })
    }
}

// =============================================================================
// Runtime context views
// =============================================================================

/// Privileged runtime context passed to `setup` / `teardown` / `start` / `stop`.
#[pyclass(name = "RuntimeContextFullAccess", module = "streamlib", frozen)]
pub(crate) struct PythonRuntimeContextFullAccess {
    full_access_view_lease: FullAccessRuntimeContextViewLease,
    cached_runtime_id: OnceLock<String>,
    cached_processor_id: OnceLock<Option<String>>,
    configuration: serde_json::Value,
    link_input_data_reader: Py<PythonLinkInputDataReader>,
    link_output_data_writer: Py<PythonLinkOutputDataWriter>,
    gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    gpu_full_access_context: Py<PythonGpuContextFullAccess>,
}

impl PythonRuntimeContextFullAccess {
    pub(crate) fn create_for_processor(
        python: Python<'_>,
        configuration: serde_json::Value,
        link_data_access: &Py<PythonProcessorLinkDataAccess>,
    ) -> PyResult<Self> {
        let full_access_view_lease: FullAccessRuntimeContextViewLease = Arc::new(RwLock::new(None));
        Ok(Self {
            gpu_full_access_context: Py::new(
                python,
                PythonGpuContextFullAccess {
                    gpu_full_access_source: GpuFullAccessSource::FullAccessHook(Arc::clone(
                        &full_access_view_lease,
                    )),
                },
            )?,
            gpu_limited_access_context: Py::new(
                python,
                PythonGpuContextLimitedAccess::new_unprimed(),
            )?,
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
            full_access_view_lease,
            cached_runtime_id: OnceLock::new(),
            cached_processor_id: OnceLock::new(),
            configuration,
        })
    }

    /// Called by the host with no GIL attached, before the hook is invoked.
    pub(crate) fn install_view_lease_and_prime_caches(&self, view: &RuntimeContextFullAccess<'_>) {
        self.cached_runtime_id.get_or_init(|| view.runtime_id());
        self.cached_processor_id.get_or_init(|| view.processor_id());
        self.gpu_limited_access_context
            .get()
            .prime_from_engine_view(view.gpu_limited_access().clone());
        *self.full_access_view_lease.write() =
            Some(FullAccessRuntimeContextViewPointer::from_engine_view(view));
    }

    /// Called by the host with no GIL attached, after the hook returned.
    /// Blocks until every in-flight lease reader finishes.
    pub(crate) fn revoke_view_lease(&self) {
        *self.full_access_view_lease.write() = None;
    }
}

#[pymethods]
impl PythonRuntimeContextFullAccess {
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
    fn runtime_id(&self) -> PyResult<String> {
        self.cached_runtime_id
            .get()
            .cloned()
            .ok_or_else(context_not_yet_activated_error)
    }

    #[getter]
    fn processor_id(&self) -> PyResult<Option<String>> {
        self.cached_processor_id
            .get()
            .cloned()
            .ok_or_else(context_not_yet_activated_error)
    }

    /// Whether this processor is currently paused.
    fn is_paused(&self, python: Python<'_>) -> PyResult<bool> {
        read_full_access_view(python, &self.full_access_view_lease, |view| {
            Ok(view.is_paused())
        })
    }

    /// Whether processing should proceed (not paused).
    fn should_process(&self, python: Python<'_>) -> PyResult<bool> {
        read_full_access_view(python, &self.full_access_view_lease, |view| {
            Ok(view.should_process())
        })
    }
}

/// Restricted runtime context passed to `process` / `on_pause` / `on_resume`.
///
/// `gpu_full_access` is deliberately absent — reaching for it raises
/// `AttributeError`, mirroring the Rust capability split.
#[pyclass(name = "RuntimeContextLimitedAccess", module = "streamlib", frozen)]
pub(crate) struct PythonRuntimeContextLimitedAccess {
    limited_access_view_lease: LimitedAccessRuntimeContextViewLease,
    cached_runtime_id: OnceLock<String>,
    cached_processor_id: OnceLock<Option<String>>,
    configuration: serde_json::Value,
    link_input_data_reader: Py<PythonLinkInputDataReader>,
    link_output_data_writer: Py<PythonLinkOutputDataWriter>,
    gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
}

impl PythonRuntimeContextLimitedAccess {
    pub(crate) fn create_for_processor(
        python: Python<'_>,
        configuration: serde_json::Value,
        link_data_access: &Py<PythonProcessorLinkDataAccess>,
    ) -> PyResult<Self> {
        Ok(Self {
            limited_access_view_lease: Arc::new(RwLock::new(None)),
            cached_runtime_id: OnceLock::new(),
            cached_processor_id: OnceLock::new(),
            configuration,
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
                PythonGpuContextLimitedAccess::new_unprimed(),
            )?,
        })
    }

    /// Called by the host with no GIL attached, before the hook is invoked.
    pub(crate) fn install_view_lease_and_prime_caches(
        &self,
        view: &RuntimeContextLimitedAccess<'_>,
    ) {
        self.cached_runtime_id.get_or_init(|| view.runtime_id());
        self.cached_processor_id.get_or_init(|| view.processor_id());
        self.gpu_limited_access_context
            .get()
            .prime_from_engine_view(view.gpu_limited_access().clone());
        *self.limited_access_view_lease.write() = Some(
            LimitedAccessRuntimeContextViewPointer::from_engine_view(view),
        );
    }

    /// Called by the host with no GIL attached, after the hook returned.
    /// Blocks until every in-flight lease reader finishes.
    pub(crate) fn revoke_view_lease(&self) {
        *self.limited_access_view_lease.write() = None;
    }
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
    fn runtime_id(&self) -> PyResult<String> {
        self.cached_runtime_id
            .get()
            .cloned()
            .ok_or_else(context_not_yet_activated_error)
    }

    #[getter]
    fn processor_id(&self) -> PyResult<Option<String>> {
        self.cached_processor_id
            .get()
            .cloned()
            .ok_or_else(context_not_yet_activated_error)
    }

    /// Whether this processor is currently paused.
    fn is_paused(&self, python: Python<'_>) -> PyResult<bool> {
        read_limited_access_view(python, &self.limited_access_view_lease, |view| {
            Ok(view.is_paused())
        })
    }

    /// Whether processing should proceed (not paused).
    fn should_process(&self, python: Python<'_>) -> PyResult<bool> {
        read_limited_access_view(python, &self.limited_access_view_lease, |view| {
            Ok(view.should_process())
        })
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

/// The same vocabulary the subprocess escalate handler accepted, so a
/// processor migrated from the old SDK keeps its format strings.
fn parse_pixel_format_name(name: &str) -> PyResult<PixelFormat> {
    let normalized = name.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "bgra" | "bgra32" => Ok(PixelFormat::Bgra32),
        "rgba" | "rgba32" => Ok(PixelFormat::Rgba32),
        "argb" | "argb32" => Ok(PixelFormat::Argb32),
        "rgba64" => Ok(PixelFormat::Rgba64),
        "nv12" | "nv12_video_range" => Ok(PixelFormat::Nv12VideoRange),
        "nv12_full_range" => Ok(PixelFormat::Nv12FullRange),
        "uyvy" | "uyvy422" => Ok(PixelFormat::Uyvy422),
        "yuyv" | "yuyv422" => Ok(PixelFormat::Yuyv422),
        "gray" | "gray8" => Ok(PixelFormat::Gray8),
        unknown => Err(PyValueError::new_err(format!(
            "unknown pixel format {unknown:?}"
        ))),
    }
}

fn pixel_format_name(format: PixelFormat) -> &'static str {
    match format {
        PixelFormat::Bgra32 => "bgra32",
        PixelFormat::Rgba32 => "rgba32",
        PixelFormat::Argb32 => "argb32",
        PixelFormat::Rgba64 => "rgba64",
        PixelFormat::Nv12VideoRange => "nv12_video_range",
        PixelFormat::Nv12FullRange => "nv12_full_range",
        PixelFormat::Uyvy422 => "uyvy422",
        PixelFormat::Yuyv422 => "yuyv422",
        PixelFormat::Gray8 => "gray8",
        PixelFormat::Unknown => "unknown",
    }
}

fn parse_texture_format_name(name: &str) -> PyResult<TextureFormat> {
    let normalized = name.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "rgba8_unorm" => Ok(TextureFormat::Rgba8Unorm),
        "rgba8_unorm_srgb" => Ok(TextureFormat::Rgba8UnormSrgb),
        "bgra8_unorm" => Ok(TextureFormat::Bgra8Unorm),
        "bgra8_unorm_srgb" => Ok(TextureFormat::Bgra8UnormSrgb),
        "rgba16_float" => Ok(TextureFormat::Rgba16Float),
        "rgba32_float" => Ok(TextureFormat::Rgba32Float),
        "nv12" => Ok(TextureFormat::Nv12),
        unknown => Err(PyValueError::new_err(format!(
            "unknown texture format {unknown:?}"
        ))),
    }
}

fn texture_format_name(format: TextureFormat) -> &'static str {
    match format {
        TextureFormat::Rgba8Unorm => "rgba8_unorm",
        TextureFormat::Rgba8UnormSrgb => "rgba8_unorm_srgb",
        TextureFormat::Bgra8Unorm => "bgra8_unorm",
        TextureFormat::Bgra8UnormSrgb => "bgra8_unorm_srgb",
        TextureFormat::Rgba16Float => "rgba16_float",
        TextureFormat::Rgba32Float => "rgba32_float",
        TextureFormat::Nv12 => "nv12",
    }
}

/// Bytes per pixel for the formats a device-exchange buffer can carry.
///
/// Restricted to the single-plane formats deliberately: the allocation is a
/// flat buffer sized `width * height * bytes_per_pixel`, and a multi-plane
/// format has no single such size.
fn device_exchange_bytes_per_pixel(format: PixelFormat) -> PyResult<u32> {
    match format {
        PixelFormat::Bgra32 | PixelFormat::Rgba32 | PixelFormat::Argb32 => Ok(4),
        PixelFormat::Rgba64 => Ok(8),
        PixelFormat::Gray8 => Ok(1),
        PixelFormat::Yuyv422 | PixelFormat::Uyvy422 => Ok(2),
        multi_plane_or_unknown => Err(PyValueError::new_err(format!(
            "pixel format {multi_plane_or_unknown:?} has no flat device-exchange size: a \
             device-exchange buffer is one linear allocation, which a multi-plane format is not"
        ))),
    }
}

fn parse_texture_usage_names(names: &[String]) -> PyResult<TextureUsages> {
    if names.is_empty() {
        return Err(PyValueError::new_err(
            "texture usage list must not be empty",
        ));
    }
    let mut usages = TextureUsages::NONE;
    for name in names {
        let normalized = name.trim().to_ascii_lowercase();
        usages |= match normalized.as_str() {
            "copy_src" => TextureUsages::COPY_SRC,
            "copy_dst" => TextureUsages::COPY_DST,
            "texture_binding" => TextureUsages::TEXTURE_BINDING,
            "storage_binding" => TextureUsages::STORAGE_BINDING,
            "render_attachment" => TextureUsages::RENDER_ATTACHMENT,
            unknown => {
                return Err(PyValueError::new_err(format!(
                    "unknown texture usage {unknown:?}"
                )));
            }
        };
    }
    Ok(usages)
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
            assert_eq!(
                parse_pixel_format_name(pixel_format_name(format)).unwrap(),
                format
            );
        }
        assert_eq!(
            parse_pixel_format_name("bgra").unwrap(),
            PixelFormat::Bgra32,
            "the old SDK's default mnemonic must keep working"
        );
        assert!(parse_pixel_format_name("sepia").is_err());
    }

    #[test]
    fn texture_format_names_round_trip() {
        for format in [
            TextureFormat::Rgba8Unorm,
            TextureFormat::Rgba8UnormSrgb,
            TextureFormat::Bgra8Unorm,
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Rgba16Float,
            TextureFormat::Rgba32Float,
            TextureFormat::Nv12,
        ] {
            assert_eq!(
                parse_texture_format_name(texture_format_name(format)).unwrap(),
                format
            );
        }
    }

    #[test]
    fn texture_usage_names_combine_and_refuse_unknowns() {
        let usages =
            parse_texture_usage_names(&["copy_src".to_string(), "render_attachment".to_string()])
                .unwrap();
        assert!(usages.contains(TextureUsages::COPY_SRC));
        assert!(usages.contains(TextureUsages::RENDER_ATTACHMENT));
        assert!(!usages.contains(TextureUsages::COPY_DST));
        assert!(parse_texture_usage_names(&[]).is_err());
        assert!(parse_texture_usage_names(&["paint".to_string()]).is_err());
    }
}
