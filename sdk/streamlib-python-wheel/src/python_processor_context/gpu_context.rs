// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_helper_process_pixel_exchange::HelperCheckedOutSurface;
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_helper_process_pixel_exchange::{
    HelperProcessGraphicsKernelRegistration, HelperProcessRayTracingKernelRegistration,
};
use crate::python_processor_owned_window::PythonProcessorOwnedWindow;

#[cfg(not(target_os = "linux"))]
use super::fd_shaped_raw_handle_is_linux_only_error;
use super::format_vocabulary::{parse_pixel_format_name, parse_texture_format_name};
use super::gpu_surface_check_out_lease::{
    PythonGpuSurfaceCheckOutLease, PythonOpaqueFdTextureExport,
};
use super::gpu_surface_handle::PythonGpuSurfaceHandle;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::kernel_wire_encoding::{
    GRAPHICS_BINDING_KIND_WIRE_NAMES, GRAPHICS_SHADER_STAGE_WIRE_BITS,
    GraphicsPipelineStateArguments, RAY_TRACING_BINDING_KIND_WIRE_NAMES,
    RAY_TRACING_SHADER_STAGE_WIRE_BITS, declared_compute_bindings_to_wire,
    declared_staged_kernel_bindings_to_wire, encode_little_endian_f32_hex,
    encode_little_endian_u32_hex, encode_lowercase_hex, graphics_pipeline_state_to_wire,
    ray_tracing_shader_groups_to_wire, ray_tracing_stages_to_wire, tlas_instances_to_wire,
};
use super::kernels::{
    PythonAccelerationStructureHandle, PythonComputeKernel, PythonGraphicsKernel,
    PythonKernelDispatchBatch, PythonRayTracingKernel,
};
use super::{
    escalate_scope_cannot_cross_the_process_boundary_error,
    gpu_unreachable_from_a_helper_process_error,
};

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
    pub(super) fn new_for_helper_process(
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    /// every request, so the CPU doors reach the pixels — over the surface's
    /// host-visible staging on Linux, through its IOSurface on macOS — with
    /// no transfer usage spelled here.
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
        #[cfg(target_os = "macos")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let acquired =
                exchange_client.acquire_texture(python, width, height, texture_format, &usage)?;
            return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
                HelperCheckedOutSurface::Texture(acquired),
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
    pub(super) helper_process_exchange_client: Option<Arc<HelperProcessGpuExchangeClient>>,
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        #[cfg(target_os = "macos")]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            let acquired =
                exchange_client.acquire_texture(python, width, height, texture_format, &usage)?;
            return Ok(PythonGpuSurfaceHandle::from_helper_checked_out_surface(
                HelperCheckedOutSurface::Texture(acquired),
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
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

    /// Refuses by name: DMA-BUF is a Linux handle.
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer off Linux"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter name is the API; stubtest compares it"
    )]
    fn export_dma_buf(&self, surface: &PythonGpuSurfaceHandle) -> PyResult<(i32, u64)> {
        Err(fd_shaped_raw_handle_is_linux_only_error("export_dma_buf"))
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

    /// Refuses by name: OPAQUE_FD is a Linux handle.
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer off Linux"
    )]
    #[expect(
        unused_variables,
        reason = "the Python-visible parameter name is the API; stubtest compares it"
    )]
    fn export_opaque_fd(
        &self,
        surface: &PythonGpuSurfaceHandle,
    ) -> PyResult<PythonOpaqueFdTextureExport> {
        Err(fd_shaped_raw_handle_is_linux_only_error("export_opaque_fd"))
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

    /// Refuses by name: DMA-BUF is a Linux handle.
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "the refusal is this capability's whole answer off Linux"
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
        Err(fd_shaped_raw_handle_is_linux_only_error("import_dma_buf"))
    }

    /// Block until the GPU device is idle.
    fn wait_device_idle(&self, python: Python<'_>) -> PyResult<()> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Some(exchange_client) = &self.helper_process_exchange_client {
            return exchange_client.wait_device_idle(python);
        }
        let _ = python;
        Err(gpu_unreachable_from_a_helper_process_error())
    }
}
