# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Named-binding ray tracing, from Python.

Ray tracing is an always-present capability of `GpuContext` on a device whose
extension chain carries `VK_KHR_ray_tracing_pipeline`; a device without it
refuses by name. Building a scene and tracing it — a bottom-level structure
over triangle geometry, a top-level one placing it, a trace into an acquired
storage image — needs nothing from the application but the processor itself.

The acceleration structures are objects, not ids: nothing publishes one for
another processor to resolve, so the handle a build returned is the whole way
to name it, and a trace binds the top-level one because that is what holds the
instances.

The stage claim is the case the plan singles out. A ray-tracing kernel's stage
set varies per kernel, so a binding declared for a stage this kernel has no
module for is a claim no trace could ever make true — and it is refused at the
`create_ray_tracing_kernel` line, where the multi-stage declaration is built.

These tests are `requires_gpu` and execute on the rig only; CI green is not
proof for them. `requires_gpu` does not cover the extension chain, so a device
without `VK_KHR_ray_tracing_pipeline` reports that refusal and the test skips
on it rather than failing on a capability the machine does not have.
"""

import json
import re
from pathlib import Path

import pytest

from ray_tracing_kernel_probes import SCENE_BINDING, TRACED_OUTPUT_BINDING

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "ray_tracing_kernel_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_probe(start_app_under_test, probe_class_name: str) -> dict:
    """One probe, one observation dict — or a failure carrying the probe's own
    traceback, which names the cause better than a missing marker.

    A device with no ray-tracing chain skips: the probe reports the engine's
    own refusal, which is a capability statement rather than a defect.
    """
    app = start_app_under_test(APP, probe_class_name)
    app.await_output_containing(
        "MARKER:PROBE_RESULT", f"the {probe_class_name} result"
    )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "ray_tracing_unavailable" in observation:
        pytest.skip(observation["ray_tracing_unavailable"])
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
    return observation


def spelled_the_same_way(message: str) -> str:
    """A message with the two spellings of a binding kind — the wire's
    `storage_image` and the engine type's `StorageImage` — made comparable."""
    return message.lower().replace("_", "")


def test_a_python_processor_builds_a_scene_and_traces_it(start_app_under_test):
    """The demo: a BLAS, a TLAS placing it, and a trace into a storage image,
    all from a helper process with no application-supplied bridge."""
    observed = run_probe(start_app_under_test, "TracedTriangleProbe")

    assert observed["traced"] is True
    assert observed["binding_names"] == [SCENE_BINDING, TRACED_OUTPUT_BINDING], (
        "the kernel reports the shaders' own binding names, which is what a "
        "trace resolves against"
    )
    assert observed["bottom_level_label"] == "python-triangle-blas"
    assert observed["top_level_label"] == "python-triangle-tlas", (
        "the label is all a structure hands back — no id string reaches Python"
    )
    assert observed["traced_output_surface_id"], (
        "the traced output is a surface this processor acquired for itself"
    )


def test_a_binding_declared_for_a_stage_this_kernel_has_no_module_for_is_refused_at_construction(
    start_app_under_test,
):
    """The ticket's named validation case.

    A trace never revisits which stage reads what — the descriptor set layout
    is built once — so the mistake has to refuse where the multi-stage
    declaration is built. The traceback is asserted on, not just the message:
    "at construction" means the `create_ray_tracing_kernel` line raised and no
    kernel object was ever handed back to trace with.
    """
    observed = run_probe(start_app_under_test, "RayTracingStageMismatchProbe")

    stage_mismatch = observed["stage_mismatch"]
    assert SCENE_BINDING in stage_mismatch, stage_mismatch
    assert "any_hit" in stage_mismatch, (
        f"must name the stage that was claimed: {stage_mismatch}"
    )
    assert "no shader module" in stage_mismatch, (
        f"must say why the claim can never come true: {stage_mismatch}"
    )
    assert "ray_gen" in stage_mismatch, (
        f"must name the stages the kernel was built from: {stage_mismatch}"
    )

    raised_at = observed["stage_mismatch_traceback"]
    assert "create_ray_tracing_kernel" in raised_at, (
        f"the refusal must come from the construction line: {raised_at}"
    )
    assert ".trace(" not in raised_at, (
        f"a stage mismatch caught at the trace is caught too late: {raised_at}"
    )
    assert observed["the_corrected_declaration_traced"] is True, (
        "the same modules with the stage claim corrected must still trace, or "
        "the refusal rejected the scene rather than the declaration"
    )


def test_a_binding_the_shaders_do_not_declare_is_refused_at_construction(
    start_app_under_test,
):
    observed = run_probe(start_app_under_test, "RayTracingBindingRefusalProbe")

    unknown = observed["unknown_at_construction"]
    assert "ambient_occlusion_radius" in unknown, (
        f"must name the unknown binding: {unknown}"
    )
    assert SCENE_BINDING in unknown and TRACED_OUTPUT_BINDING in unknown, (
        f"must name what the shaders do declare: {unknown}"
    )


def test_a_binding_declared_as_the_wrong_kind_is_refused_at_construction(
    start_app_under_test,
):
    observed = run_probe(start_app_under_test, "RayTracingBindingRefusalProbe")

    mismatch = observed["kind_mismatch_at_construction"]
    assert SCENE_BINDING in mismatch, mismatch
    assert "storageimage" in spelled_the_same_way(mismatch), (
        f"must name the kind claimed: {mismatch}"
    )
    assert "accelerationstructure" in spelled_the_same_way(mismatch), (
        f"must name the kind the shaders declare: {mismatch}"
    )


def test_a_binding_the_shaders_do_not_declare_is_refused_at_the_trace(
    start_app_under_test,
):
    observed = run_probe(start_app_under_test, "RayTracingBindingRefusalProbe")

    unknown = observed["unknown_at_trace"]
    assert "ambient_occlusion_radius" in unknown, (
        f"must name the unknown binding: {unknown}"
    )
    assert SCENE_BINDING in unknown and TRACED_OUTPUT_BINDING in unknown, (
        f"must name what the shaders do declare: {unknown}"
    )


def test_an_unsupplied_binding_is_refused_naming_the_shaders_bindings(
    start_app_under_test,
):
    """No implicit default and no carried-over value — for the surface-bound
    binding and for the acceleration structure alike, which resolve through
    different registries and so are two separate refusals."""
    observed = run_probe(start_app_under_test, "RayTracingBindingRefusalProbe")

    missing_output = observed["missing_output_at_trace"]
    assert TRACED_OUTPUT_BINDING in missing_output, missing_output
    assert "not supplied" in missing_output, missing_output
    assert "do not persist between traces" in missing_output, (
        f"must say why there is no fallback: {missing_output}"
    )

    missing_scene = observed["missing_scene_at_trace"]
    assert SCENE_BINDING in missing_scene, missing_scene
    assert "not supplied" in missing_scene, missing_scene


def test_a_refused_trace_leaves_the_kernel_traceable(start_app_under_test):
    """Every refusal above raises before anything is submitted, so none of them
    strands the kernel holding half a trace's bindings."""
    observed = run_probe(start_app_under_test, "RayTracingBindingRefusalProbe")
    assert observed["traced_after_the_refusals"] is True


def test_a_buffer_kind_binding_is_refused_naming_the_kinds_a_trace_can_bind(
    start_app_under_test,
):
    """The only by-surface-id resolution the engine has is texture-shaped, so a
    uniform-buffer binding is refused rather than pointed at a texture."""
    observed = run_probe(start_app_under_test, "RayTracingBufferBindingRefusalProbe")

    refusal = observed["buffer_kind_binding"]
    assert observed["buffer_binding"] in refusal, (
        f"must name the binding it cannot resolve: {refusal}"
    )
    assert "uniform_buffer" in refusal, refusal
    assert "storage_image" in refusal and "sampled_texture" in refusal, (
        f"must name the kinds a trace can bind a surface for: {refusal}"
    )


def test_an_acceleration_structure_binding_takes_a_handle_not_a_surface(
    start_app_under_test,
):
    """It is the one binding kind that cannot be spelled as an id string:
    nothing publishes an acceleration structure for another processor to
    resolve."""
    observed = run_probe(
        start_app_under_test, "AccelerationStructureHandleRefusalProbe"
    )

    refusal = observed["a_surface_where_a_structure_belongs"]
    assert SCENE_BINDING in refusal, refusal
    assert "build_tlas" in refusal, (
        f"must name the builder whose handle it wants: {refusal}"
    )


def test_a_trace_binds_the_top_level_structure_not_a_bottom_level_one(
    start_app_under_test,
):
    """The top-level structure is what holds the instances, so binding the
    bottom-level one traces an empty scene — refused instead."""
    observed = run_probe(
        start_app_under_test, "AccelerationStructureHandleRefusalProbe"
    )

    refusal = observed["a_bottom_level_structure_at_the_trace"]
    assert SCENE_BINDING in refusal, refusal
    assert "bottom-level" in refusal and "top-level" in refusal, refusal


def test_an_instance_places_a_bottom_level_structure(start_app_under_test):
    """The other direction of the same discipline: a scene is built out of
    bottom-level structures, so a top-level one is not an instance."""
    observed = run_probe(
        start_app_under_test, "AccelerationStructureHandleRefusalProbe"
    )

    refusal = observed["a_top_level_structure_as_an_instance"]
    assert "top-level" in refusal, refusal
    assert "instance 0" in refusal, (
        f"must name which instance was wrong: {refusal}"
    )


def test_geometry_that_is_not_whole_triangles_is_refused(start_app_under_test):
    """A vertex is three floats and a triangle is three indices; a blob that is
    neither would build a structure over misread memory."""
    observed = run_probe(
        start_app_under_test, "AccelerationStructureHandleRefusalProbe"
    )

    vertices = observed["vertices_that_are_not_triangles"]
    assert "vertex" in vertices and "three" in vertices, vertices

    indices = observed["indices_that_are_not_triangles"]
    assert "triangle" in indices and "three" in indices, indices
