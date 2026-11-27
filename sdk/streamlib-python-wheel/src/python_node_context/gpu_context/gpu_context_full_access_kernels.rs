// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The graphics and ray-tracing kernels and the acceleration structures
//! `GpuContextFullAccess` creates.

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib_scope::CallTooSlowForAFrame;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_helper_process_pixel_exchange::{
    HelperProcessGraphicsKernelRegistration, HelperProcessRayTracingKernelRegistration,
};
use crate::python_node_context::gpu_unreachable_from_a_helper_process_error;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::python_node_context::kernel_wire_encoding::{
    GRAPHICS_BINDING_KIND_WIRE_NAMES, GRAPHICS_SHADER_STAGE_WIRE_BITS,
    GraphicsPipelineStateArguments, RAY_TRACING_BINDING_KIND_WIRE_NAMES,
    RAY_TRACING_SHADER_STAGE_WIRE_BITS, declared_staged_kernel_bindings_to_wire,
    encode_little_endian_f32_hex, encode_little_endian_u32_hex, encode_lowercase_hex,
    graphics_pipeline_state_to_wire, ray_tracing_shader_groups_to_wire, ray_tracing_stages_to_wire,
    tlas_instances_to_wire,
};
use crate::python_node_context::kernels::{
    PythonAccelerationStructureHandle, PythonGraphicsKernel, PythonRayTracingKernel,
};
use crate::python_node_scope::raised_while_a_frame_runs;

use super::PythonGpuContextFullAccess;

#[pymethods]
impl PythonGpuContextFullAccess {
    /// Build a graphics kernel from Slang source, or from pre-compiled SPIR-V.
    ///
    /// Constructed once in `setup()`, drawn per frame in `process()`. The
    /// engine compiles both stages and reflects them at construction, taking
    /// its binding names from them — those names are what `draw` resolves
    /// against. Re-creating an identical kernel is free of compilation.
    ///
    /// The vertices are the shaders' own: no escalate op mints a vertex or
    /// index buffer, so a vertex stage fabricates its positions from
    /// `SV_VulkanVertexID`, and the pipeline carries no vertex input state. The
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
        raised_while_a_frame_runs(
            CallTooSlowForAFrame::CreatesAKernel,
            "create_graphics_kernel",
        )?;
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

    /// Build a ray-tracing kernel from Slang sources, or from pre-compiled
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
        raised_while_a_frame_runs(
            CallTooSlowForAFrame::CreatesAKernel,
            "create_ray_tracing_kernel",
        )?;
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
}
