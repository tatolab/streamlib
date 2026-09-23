// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::prelude::*;
use pyo3::types::PyDict;
use pyo3::types::PyList;

use crate::python_helper_process_pixel_exchange::{
    HelperProcessGpuExchangeClient, escalate_round_trip_to_parent,
    hand_a_release_handle_to_the_release_worker,
};
use crate::python_processor_context::ReflectedKernelBinding;

use super::response_field;

/// The binding shape a `register_*_kernel` response carries, in slot order.
///
/// All three register ops answer with the same array — a dispatch resolves by
/// name and only the shaders know which kind each name is — so all three read
/// it here.
fn reflected_kernel_bindings_in(
    response: &Bound<'_, PyAny>,
) -> PyResult<Vec<ReflectedKernelBinding>> {
    let mut reflected = Vec::new();
    for entry in response_field(response, "bindings")?.try_iter()? {
        let entry = entry?;
        reflected.push(ReflectedKernelBinding {
            name: entry.get_item("name")?.extract()?,
            kind: entry.get_item("kind")?.extract()?,
        });
    }
    Ok(reflected)
}

/// Everything a `register_graphics_kernel` carries beyond the op name.
///
/// A struct rather than seven adjacent `&str` arguments: swapping two shader
/// blobs or two entry points compiles clean and lands as a stage that will not
/// link, one round trip away from the call that got it wrong.
pub(crate) struct HelperProcessGraphicsKernelRegistration<'a, 'py> {
    pub(crate) label: &'a str,
    pub(crate) vertex_source: &'a str,
    pub(crate) vertex_spirv_hex: &'a str,
    pub(crate) vertex_entry_point: &'a str,
    pub(crate) fragment_source: &'a str,
    pub(crate) fragment_spirv_hex: &'a str,
    pub(crate) fragment_entry_point: &'a str,
    pub(crate) push_constant_size: u32,
    pub(crate) declared_bindings: &'a Bound<'py, PyList>,
    pub(crate) pipeline_state: &'a Bound<'py, PyDict>,
}

/// One graphics draw as the wire carries it.
///
/// A struct rather than six adjacent `u32` arguments: an extent transposed with
/// an instance count compiles clean and renders the wrong thing.
pub(crate) struct HelperProcessGraphicsDraw<'a, 'py> {
    pub(crate) kernel_id: &'a str,
    pub(crate) bindings: &'a Bound<'py, PyList>,
    pub(crate) color_target_surface_ids: &'a Bound<'py, PyList>,
    pub(crate) push_constants_hex: &'a str,
    pub(crate) vertex_count: u32,
    pub(crate) instance_count: u32,
    pub(crate) first_vertex: u32,
    pub(crate) first_instance: u32,
    pub(crate) extent_width: u32,
    pub(crate) extent_height: u32,
}

/// Everything a `register_ray_tracing_kernel` carries beyond the op name.
pub(crate) struct HelperProcessRayTracingKernelRegistration<'a, 'py> {
    pub(crate) label: &'a str,
    pub(crate) stages: &'a Bound<'py, PyList>,
    pub(crate) groups: &'a Bound<'py, PyList>,
    pub(crate) declared_bindings: &'a Bound<'py, PyList>,
    pub(crate) max_recursion_depth: u32,
    pub(crate) push_constant_size: u32,
}

/// One compute dispatch as the wire carries it.
///
/// The single-dispatch op and one entry of a batch are the same six fields, so
/// they are built here once — `#[serde(deny_unknown_fields)]` host-side means a
/// key that drifted in only one of two builders would be a hard refusal inside
/// a helper child.
pub(crate) fn compute_dispatch_wire_entry<'py>(
    python: Python<'py>,
    kernel_id: &str,
    bindings: &Bound<'_, PyAny>,
    push_constants_hex: &str,
    group_count: (u32, u32, u32),
) -> PyResult<Bound<'py, PyDict>> {
    let entry = PyDict::new(python);
    entry.set_item("kernel_id", kernel_id)?;
    entry.set_item("bindings", bindings)?;
    entry.set_item("push_constants_hex", push_constants_hex)?;
    entry.set_item("group_count_x", group_count.0)?;
    entry.set_item("group_count_y", group_count.1)?;
    entry.set_item("group_count_z", group_count.2)?;
    Ok(entry)
}

impl HelperProcessGpuExchangeClient {
    /// Build a compute kernel in the parent and take back its id plus the
    /// binding shape reflection found.
    ///
    /// The shape comes back because dispatch resolves by name and only the
    /// shader knows which kind each name is — without it this side would have
    /// to guess a kind for every binding it supplies.
    pub(crate) fn register_compute_kernel(
        &self,
        python: Python<'_>,
        source: &str,
        spv_hex: &str,
        entry_point: &str,
        push_constant_size: u32,
        declared_bindings: &Bound<'_, PyAny>,
    ) -> PyResult<(String, Vec<ReflectedKernelBinding>)> {
        let op = PyDict::new(python);
        op.set_item("op", "register_compute_kernel")?;
        op.set_item("source", source)?;
        op.set_item("stage", "compute")?;
        op.set_item("entry_point", entry_point)?;
        op.set_item("spv_hex", spv_hex)?;
        op.set_item("push_constant_size", push_constant_size)?;
        op.set_item("bindings", declared_bindings)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let kernel_id: String = response_field(&response, "handle_id")?.extract()?;
        Ok((kernel_id, reflected_kernel_bindings_in(&response)?))
    }

    /// Dispatch a registered compute kernel with its bindings supplied by name.
    ///
    /// Returns when the parent's dispatch has retired: compute is synchronous
    /// host-side, so the writes are visible on return and no timeline value
    /// crosses back for this side to wait on.
    pub(crate) fn run_compute_kernel(
        &self,
        python: Python<'_>,
        kernel_id: &str,
        bindings: &Bound<'_, PyAny>,
        push_constants_hex: &str,
        group_count: (u32, u32, u32),
    ) -> PyResult<()> {
        let op = compute_dispatch_wire_entry(
            python,
            kernel_id,
            bindings,
            push_constants_hex,
            group_count,
        )?;
        op.set_item("op", "run_compute_kernel")?;
        // Sent as its own op rather than a batch of one: a single dispatch's
        // refusal names the binding, never "dispatch 0 of this batch". The
        // host runs both ops on the same recording machinery.
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }

    /// Run several registered dispatches as one recording in the parent.
    ///
    /// Each entry comes from [`compute_dispatch_wire_entry`], the same builder
    /// the single-dispatch op uses.
    ///
    /// One round trip, not one per dispatch: the whole point is that N passes
    /// cost one submission and one stall, and sending them separately would
    /// pay both N times over before the parent ever saw a batch.
    pub(crate) fn run_compute_kernel_batch(
        &self,
        python: Python<'_>,
        dispatches: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", "run_compute_kernel_batch")?;
        op.set_item("dispatches", dispatches)?;
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }

    /// Build a graphics kernel in the parent and take back its id plus the
    /// binding shape reflection found across both stages.
    pub(crate) fn register_graphics_kernel(
        &self,
        python: Python<'_>,
        registration: &HelperProcessGraphicsKernelRegistration<'_, '_>,
    ) -> PyResult<(String, Vec<ReflectedKernelBinding>)> {
        let op = PyDict::new(python);
        op.set_item("op", "register_graphics_kernel")?;
        op.set_item("label", registration.label)?;
        op.set_item("vertex_source", registration.vertex_source)?;
        op.set_item("vertex_spv_hex", registration.vertex_spirv_hex)?;
        op.set_item("vertex_entry_point", registration.vertex_entry_point)?;
        op.set_item("fragment_source", registration.fragment_source)?;
        op.set_item("fragment_spv_hex", registration.fragment_spirv_hex)?;
        op.set_item("fragment_entry_point", registration.fragment_entry_point)?;
        op.set_item("bindings", registration.declared_bindings)?;
        op.set_item("pipeline_state", registration.pipeline_state)?;
        op.set_item("push_constant_size", registration.push_constant_size)?;
        // An empty stage mask asserts nothing and the host adopts what the
        // shaders reflect, which is the only source this side has for it.
        op.set_item("push_constant_stages", 0u32)?;
        // One descriptor set, drawn at index 0 forever. Dispatch is
        // synchronous, so a ring of sets buys nothing, and its index is exactly
        // the kind of slot number this surface keeps out of Python.
        op.set_item("descriptor_sets_in_flight", 1u32)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let kernel_id: String = response_field(&response, "handle_id")?.extract()?;
        Ok((kernel_id, reflected_kernel_bindings_in(&response)?))
    }

    /// Render one offscreen pass with a registered graphics kernel.
    ///
    /// Returns when the parent's draw has retired: the offscreen render is
    /// synchronous host-side, so the colour target's pixels are visible on
    /// return and no timeline value crosses back for this side to wait on.
    pub(crate) fn run_graphics_draw(
        &self,
        python: Python<'_>,
        draw: &HelperProcessGraphicsDraw<'_, '_>,
    ) -> PyResult<()> {
        let draw_call = PyDict::new(python);
        draw_call.set_item("kind", "draw")?;
        draw_call.set_item("vertex_count", draw.vertex_count)?;
        draw_call.set_item("instance_count", draw.instance_count)?;
        draw_call.set_item("first_vertex", draw.first_vertex)?;
        draw_call.set_item("first_instance", draw.first_instance)?;
        // Present because the wire shape is regular, and ignored host-side for
        // a non-indexed draw — which is the only kind reachable from here,
        // since no escalate op mints an index buffer.
        draw_call.set_item("first_index", 0u32)?;
        draw_call.set_item("index_count", 0u32)?;
        draw_call.set_item("vertex_offset", 0i32)?;

        let op = PyDict::new(python);
        op.set_item("op", "run_graphics_draw")?;
        op.set_item("kernel_id", draw.kernel_id)?;
        op.set_item("bindings", draw.bindings)?;
        op.set_item("color_target_uuids", draw.color_target_surface_ids)?;
        op.set_item("draw", &draw_call)?;
        op.set_item("extent_width", draw.extent_width)?;
        op.set_item("extent_height", draw.extent_height)?;
        op.set_item("frame_index", 0u32)?;
        op.set_item("push_constants_hex", draw.push_constants_hex)?;
        // No escalate op mints a VertexBuffer, so this array is always empty
        // and the shaders fabricate their vertices from `gl_VertexIndex`. The
        // field is required on the wire, and the host refuses a non-empty one.
        op.set_item("vertex_buffers", PyList::empty(python))?;
        // `viewport` / `scissor` are omitted: the host fills them with the
        // render area, which is the whole target this op ever draws into.
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }

    /// Build a ray-tracing kernel in the parent and take back its id plus the
    /// binding shape reflection found across every stage.
    pub(crate) fn register_ray_tracing_kernel(
        &self,
        python: Python<'_>,
        registration: &HelperProcessRayTracingKernelRegistration<'_, '_>,
    ) -> PyResult<(String, Vec<ReflectedKernelBinding>)> {
        let op = PyDict::new(python);
        op.set_item("op", "register_ray_tracing_kernel")?;
        op.set_item("label", registration.label)?;
        op.set_item("stages", registration.stages)?;
        op.set_item("groups", registration.groups)?;
        op.set_item("bindings", registration.declared_bindings)?;
        op.set_item("max_recursion_depth", registration.max_recursion_depth)?;
        op.set_item("push_constant_size", registration.push_constant_size)?;
        // Empty asserts nothing; the host adopts the reflected stage mask.
        op.set_item("push_constant_stages", 0u32)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let kernel_id: String = response_field(&response, "handle_id")?.extract()?;
        Ok((kernel_id, reflected_kernel_bindings_in(&response)?))
    }

    /// Trace one grid with a registered ray-tracing kernel.
    ///
    /// Returns when the parent's trace has retired: the host submits and
    /// waits before answering, so the writes are visible on receipt.
    pub(crate) fn run_ray_tracing_kernel(
        &self,
        python: Python<'_>,
        kernel_id: &str,
        bindings: &Bound<'_, PyList>,
        push_constants_hex: &str,
        grid: (u32, u32, u32),
    ) -> PyResult<()> {
        let op = PyDict::new(python);
        op.set_item("op", "run_ray_tracing_kernel")?;
        op.set_item("kernel_id", kernel_id)?;
        op.set_item("bindings", bindings)?;
        op.set_item("push_constants_hex", push_constants_hex)?;
        op.set_item("width", grid.0)?;
        op.set_item("height", grid.1)?;
        op.set_item("depth", grid.2)?;
        escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        Ok(())
    }

    /// Build a triangle-geometry bottom-level acceleration structure in the
    /// parent and take back the id it registered the result under.
    pub(crate) fn register_acceleration_structure_blas(
        &self,
        python: Python<'_>,
        label: &str,
        vertices_hex: &str,
        indices_hex: &str,
    ) -> PyResult<String> {
        let op = PyDict::new(python);
        op.set_item("op", "register_acceleration_structure_blas")?;
        op.set_item("label", label)?;
        op.set_item("vertices_hex", vertices_hex)?;
        op.set_item("indices_hex", indices_hex)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        response_field(&response, "handle_id")?.extract()
    }

    /// Build a top-level acceleration structure over already-built bottom-level
    /// ones and take back the id it registered the result under.
    pub(crate) fn register_acceleration_structure_tlas(
        &self,
        python: Python<'_>,
        label: &str,
        instances: &Bound<'_, PyList>,
    ) -> PyResult<String> {
        let op = PyDict::new(python);
        op.set_item("op", "register_acceleration_structure_tlas")?;
        op.set_item("label", label)?;
        op.set_item("instances", instances)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        response_field(&response, "handle_id")?.extract()
    }

    /// Hand an acceleration structure this helper built back to the parent,
    /// which drops the registry's strong reference and with it the device
    /// memory the structure holds.
    ///
    /// Best-effort: this runs from the handle's drop, which has no caller to
    /// raise into — so a failure is logged, never raised.
    pub(crate) fn release_acceleration_structure(
        &self,
        python: Python<'_>,
        acceleration_structure_id: &str,
    ) {
        if let Err(release_failure) = hand_a_release_handle_to_the_release_worker(
            python,
            &self.release_to_parent_without_waiting,
            acceleration_structure_id,
        ) {
            tracing::warn!(
                "releasing acceleration structure {acceleration_structure_id} to the parent \
                 failed ({release_failure}); its device memory stays allocated until the runtime \
                 stops"
            );
        }
    }
}
