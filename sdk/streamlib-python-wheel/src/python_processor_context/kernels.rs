// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;
#[cfg(target_os = "linux")]
use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;
#[cfg(target_os = "linux")]
use pyo3::types::PyList;

use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
#[cfg(target_os = "linux")]
use crate::python_helper_process_pixel_exchange::HelperProcessGraphicsDraw;

#[cfg(target_os = "linux")]
use super::gpu_surface_handle::PythonGpuSurfaceHandle;
#[cfg(target_os = "linux")]
use super::kernel_wire_encoding::{
    encode_lowercase_hex, require_declared_push_constant_size, supplied_kernel_bindings_to_wire,
};
use super::{gpu_unreachable_from_a_helper_process_error, left_by_a_propagating_exception};

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
pub(super) fn reflected_binding_names(reflected: &[ReflectedKernelBinding]) -> Vec<String> {
    reflected
        .iter()
        .map(|binding| binding.name.clone())
        .collect()
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
    pub(super) kernel_id: String,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    pub(super) push_constant_size: u32,
    /// The caller supplies surfaces by name; which kind each name is, is the
    /// shader's to say, so it is carried rather than guessed per dispatch.
    pub(super) reflected_binding_kinds: Vec<ReflectedKernelBinding>,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    pub(super) helper_process_exchange_client: Arc<HelperProcessGpuExchangeClient>,
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
pub(super) struct KernelDispatchBatchRecording {
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
    pub(super) helper_process_exchange_client: Option<Arc<HelperProcessGpuExchangeClient>>,
    pub(super) recording: Mutex<KernelDispatchBatchRecording>,
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
    pub(super) kernel_id: String,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    pub(super) push_constant_size: u32,
    /// The caller supplies surfaces by name; which kind each name is, is the
    /// shaders' to say, so it is carried rather than guessed per draw.
    pub(super) reflected_binding_kinds: Vec<ReflectedKernelBinding>,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    pub(super) helper_process_exchange_client: Arc<HelperProcessGpuExchangeClient>,
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
    pub(super) kernel_id: String,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    pub(super) push_constant_size: u32,
    /// The caller supplies targets by name; which kind each name is, is the
    /// shaders' to say, and it is also what decides whether a name takes a
    /// surface or an acceleration structure.
    pub(super) reflected_binding_kinds: Vec<ReflectedKernelBinding>,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    pub(super) helper_process_exchange_client: Arc<HelperProcessGpuExchangeClient>,
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
    pub(super) acceleration_structure_id: String,
    /// Which of the two builders minted this, so binding a bottom-level
    /// structure at a trace — or instancing a top-level one — refuses in the
    /// caller's own stack.
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    pub(super) is_top_level: bool,
    pub(super) structure_label: String,
    /// The release this handle owes the engine, paid on drop. `None` only in
    /// tests, which mint a handle without a parent to hand anything back to.
    #[cfg(target_os = "linux")]
    pub(super) helper_process_exchange_client: Option<Arc<HelperProcessGpuExchangeClient>>,
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
pub(super) fn bound_surface_id(name: &str, bound_to: &Bound<'_, PyAny>) -> PyResult<String> {
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
pub(super) fn bound_acceleration_structure_id(
    name: &str,
    bound_to: &Bound<'_, PyAny>,
) -> PyResult<String> {
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
