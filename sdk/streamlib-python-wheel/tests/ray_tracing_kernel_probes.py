# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes for named-binding ray tracing, from where a kernel really runs.

A probe builds its own scene — a bottom-level structure over triangle
geometry, a top-level one placing it — and its own kernel in `setup()` where
the capability is Full, then traces into a storage image it acquired. Every
probe runs in its own helper process and reports one `MARKER:PROBE_RESULT`
JSON line.

What is worth breaking a build over is that a Python processor can trace at
all — no application-supplied bridge, no acceleration-structure id string, the
handle a build returned being the whole way to name it — and that every way of
getting the bindings wrong is refused by name, the stage ones before a kernel
exists.

A device without `VK_KHR_ray_tracing_pipeline` can build neither structures nor
pipelines, so a probe that meets that refusal reports it rather than failing:
it is a capability the `requires_gpu` marker does not cover.
"""

import json
import os
import traceback
from collections.abc import Sequence

from streamlib import (
    GpuContextFullAccess,
    GpuSurfaceHandle,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    processor,
)
from streamlib._engine import AccelerationStructureHandle, RayTracingKernel

SURFACE_WIDTH = 64
SURFACE_HEIGHT = 64

TRACED_OUTPUT_FORMAT = "rgba8_unorm"
TRACED_OUTPUT_TEXTURE_USAGE = [
    "storage_binding",
    "texture_binding",
    "copy_src",
    "copy_dst",
]

RESULT_MARKER = "MARKER:PROBE_RESULT "

# What the escalate handler says when the device has no ray-tracing chain. A
# probe that sees it reports it, and the test skips on it.
RAY_TRACING_UNAVAILABLE = "VK_KHR_ray_tracing_pipeline"

# The ray-gen stage's own names for the two resources it binds. One takes the
# handle `build_tlas` returned; the other takes a surface.
SCENE_BINDING = "scene_structure"
TRACED_OUTPUT_BINDING = "traced_output_image"

RAY_GENERATION_GLSL = """\
#version 460
#extension GL_EXT_ray_tracing : require
layout(set = 0, binding = 0) uniform accelerationStructureEXT scene_structure;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D traced_output_image;
layout(location = 0) rayPayloadEXT vec3 ray_payload_colour;
void main() {
    vec2 in_view =
        (vec2(gl_LaunchIDEXT.xy) + vec2(0.5)) / vec2(gl_LaunchSizeEXT.xy) * 2.0 - 1.0;
    ray_payload_colour = vec3(0.0);
    traceRayEXT(scene_structure, gl_RayFlagsOpaqueEXT, 0xff, 0, 0, 0,
                vec3(in_view, 0.0), 0.001, vec3(0.0, 0.0, 1.0), 10.0, 0);
    imageStore(traced_output_image, ivec2(gl_LaunchIDEXT.xy),
               vec4(ray_payload_colour, 1.0));
}
"""

MISS_GLSL = """\
#version 460
#extension GL_EXT_ray_tracing : require
layout(location = 0) rayPayloadInEXT vec3 ray_payload_colour;
void main() { ray_payload_colour = vec3(0.0, 0.0, 1.0); }
"""

CLOSEST_HIT_GLSL = """\
#version 460
#extension GL_EXT_ray_tracing : require
layout(location = 0) rayPayloadInEXT vec3 ray_payload_colour;
void main() { ray_payload_colour = vec3(0.0, 1.0, 0.0); }
"""

# A third binding no trace can ever name a surface for: the only by-surface-id
# resolution the engine has is texture-shaped.
RAY_GENERATION_WITH_A_UNIFORM_BUFFER_GLSL = """\
#version 460
#extension GL_EXT_ray_tracing : require
layout(set = 0, binding = 0) uniform accelerationStructureEXT scene_structure;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D traced_output_image;
layout(set = 0, binding = 2) uniform TintBlock { vec4 tint; } tint_block;
layout(location = 0) rayPayloadEXT vec3 ray_payload_colour;
void main() {
    vec2 in_view =
        (vec2(gl_LaunchIDEXT.xy) + vec2(0.5)) / vec2(gl_LaunchSizeEXT.xy) * 2.0 - 1.0;
    ray_payload_colour = vec3(0.0);
    traceRayEXT(scene_structure, gl_RayFlagsOpaqueEXT, 0xff, 0, 0, 0,
                vec3(in_view, 0.0), 0.001, vec3(0.0, 0.0, 1.0), 10.0, 0);
    imageStore(traced_output_image, ivec2(gl_LaunchIDEXT.xy),
               vec4(ray_payload_colour * tint_block.tint.rgb, 1.0));
}
"""

# One triangle in front of every ray the grid casts.
TRIANGLE_VERTICES = [-1.0, -1.0, 0.5, 3.0, -1.0, 0.5, -1.0, 3.0, 0.5]
TRIANGLE_INDICES = [0, 1, 2]

# Two general groups — ray-gen and miss — and one triangles hit group, in the
# order the shader binding table is laid out. A group names its modules by
# index into `stages`, because two modules can fill the same stage.
RAY_TRACING_STAGES = [
    {"stage": "ray_gen", "source": RAY_GENERATION_GLSL},
    {"stage": "miss", "source": MISS_GLSL},
    {"stage": "closest_hit", "source": CLOSEST_HIT_GLSL},
]
RAY_TRACING_GROUPS = [
    {"kind": "general", "general_stage": 0},
    {"kind": "general", "general_stage": 1},
    {"kind": "triangles_hit", "closest_hit_stage": 2},
]

# Only the ray-gen module reads either binding, and this kernel has no any-hit
# module at all — which is the stage claim no trace could ever make true.
# Spelled out because a dict's value type is invariant: the shape has to be the
# parameter's own, not the narrower one this literal would otherwise infer.
DECLARED_BINDINGS: dict[str, str | tuple[str, Sequence[str]]] = {
    SCENE_BINDING: ("acceleration_structure", ["ray_gen"]),
    TRACED_OUTPUT_BINDING: ("storage_image", ["ray_gen"]),
}


def _report(probe_body) -> None:
    """One result line per probe, success or failure — the failure carries the
    traceback so the test fails on the cause rather than a missing marker."""
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


def _refusal_of(refused_call) -> str:
    """The message a wrong call raises, or a failure if it did not raise."""
    try:
        refused_call()
    except Exception as refusal:  # noqa: BLE001 — the refusal is the subject
        return str(refusal)
    raise AssertionError("the call was accepted; it should have been refused")


def _refusal_traceback_of(refused_call) -> str:
    """The traceback a wrong call raises, so a test can assert *which line*
    refused — construction or dispatch — rather than only what it said."""
    try:
        refused_call()
    except Exception:  # noqa: BLE001 — the refusal is the subject
        return traceback.format_exc()
    raise AssertionError("the call was accepted; it should have been refused")


def _traced_triangle_kernel(
    gpu: GpuContextFullAccess,
    bindings: dict[str, str | tuple[str, Sequence[str]]],
):
    """The conformance kernel: ray-gen, miss and closest-hit over one scene."""
    return gpu.create_ray_tracing_kernel(
        stages=RAY_TRACING_STAGES,
        groups=RAY_TRACING_GROUPS,
        bindings=bindings,
        label="python-traced-triangle",
    )


class _RayTracingKernelProbeBase:
    """Builds the scene, the kernel and the traced output in `setup`, and
    reports from `setup`.

    Nothing upstream is needed: the probe acquires its own storage image, which
    is the point — a trace's output is an engine-owned texture the processor
    names, not something handed to it.
    """

    # Declared, not merely assigned: `setup` assigns them inside a nested
    # closure, which a type checker does not walk for attribute inference.
    gpu_full_access: GpuContextFullAccess
    kernel: RayTracingKernel
    bottom_level_structure: AccelerationStructureHandle
    top_level_structure: AccelerationStructureHandle
    traced_output: GpuSurfaceHandle

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        def observe() -> dict:
            gpu = ctx.gpu_full_access
            self.gpu_full_access = gpu
            try:
                self.bottom_level_structure = gpu.build_triangles_blas(
                    vertices=TRIANGLE_VERTICES,
                    indices=TRIANGLE_INDICES,
                    label="python-triangle-blas",
                )
                self.top_level_structure = gpu.build_tlas(
                    instances=[{"blas": self.bottom_level_structure}],
                    label="python-triangle-tlas",
                )
                self.kernel = _traced_triangle_kernel(gpu, DECLARED_BINDINGS)
            except Exception as refusal:  # noqa: BLE001 — reported, then skipped on
                if RAY_TRACING_UNAVAILABLE in str(refusal):
                    return {"ray_tracing_unavailable": str(refusal)}
                raise
            self.traced_output = gpu.acquire_texture(
                SURFACE_WIDTH,
                SURFACE_HEIGHT,
                TRACED_OUTPUT_FORMAT,
                TRACED_OUTPUT_TEXTURE_USAGE,
            )
            return self.observe()

        _report(observe)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        pass

    def trace_the_conformance_grid(self) -> None:
        """The one trace every probe here spells the same way."""
        self.kernel.trace(
            bindings={
                SCENE_BINDING: self.top_level_structure,
                TRACED_OUTPUT_BINDING: self.traced_output,
            },
            grid=(SURFACE_WIDTH, SURFACE_HEIGHT, 1),
        )

    def observe(self) -> dict:
        raise NotImplementedError


@processor(
    execution="manual",
    description="Builds a BLAS and a TLAS and traces them into a storage image",
)
class TracedTriangleProbe(_RayTracingKernelProbeBase):
    """The demo: a Python processor builds a scene and traces it.

    No bridge is installed anywhere, and no acceleration-structure id string
    reaches Python — the handle each build returned is the whole way to name
    it.
    """

    def observe(self) -> dict:
        self.trace_the_conformance_grid()
        # Twice, over the same scene: the kernel keeps no binding state, so a
        # second trace that needed the first one's descriptors would fail here
        # rather than in whatever runs next.
        self.trace_the_conformance_grid()
        return {
            "traced": True,
            "binding_names": list(self.kernel.binding_names),
            "bottom_level_label": self.bottom_level_structure.label,
            "top_level_label": self.top_level_structure.label,
            "traced_output_surface_id": self.traced_output.surface_id,
        }


@processor(
    execution="manual",
    description="Every way of getting a trace's bindings wrong, refused by name",
)
class RayTracingBindingRefusalProbe(_RayTracingKernelProbeBase):
    """Unknown, missing and kind-mismatched, each raising with a message naming
    what the shaders actually declare."""

    def observe(self) -> dict:
        gpu = self.gpu_full_access
        grid = (SURFACE_WIDTH, SURFACE_HEIGHT, 1)

        unknown_at_construction = _refusal_of(
            lambda: _traced_triangle_kernel(
                gpu,
                {
                    **DECLARED_BINDINGS,
                    "ambient_occlusion_radius": ("storage_image", ["ray_gen"]),
                },
            )
        )
        kind_mismatch_at_construction = _refusal_of(
            lambda: _traced_triangle_kernel(
                gpu,
                {
                    SCENE_BINDING: ("storage_image", ["ray_gen"]),
                    TRACED_OUTPUT_BINDING: ("storage_image", ["ray_gen"]),
                },
            )
        )

        unknown_at_trace = _refusal_of(
            lambda: self.kernel.trace(
                bindings={
                    SCENE_BINDING: self.top_level_structure,
                    TRACED_OUTPUT_BINDING: self.traced_output,
                    "ambient_occlusion_radius": self.traced_output,
                },
                grid=grid,
            )
        )
        missing_output_at_trace = _refusal_of(
            lambda: self.kernel.trace(
                bindings={SCENE_BINDING: self.top_level_structure},
                grid=grid,
            )
        )
        missing_scene_at_trace = _refusal_of(
            lambda: self.kernel.trace(
                bindings={TRACED_OUTPUT_BINDING: self.traced_output},
                grid=grid,
            )
        )

        # The kernel still traces: every refusal above raised before anything
        # was submitted, so none of them left it holding half a trace's state.
        self.trace_the_conformance_grid()
        return {
            "unknown_at_construction": unknown_at_construction,
            "kind_mismatch_at_construction": kind_mismatch_at_construction,
            "unknown_at_trace": unknown_at_trace,
            "missing_output_at_trace": missing_output_at_trace,
            "missing_scene_at_trace": missing_scene_at_trace,
            "traced_after_the_refusals": True,
            "binding_names": list(self.kernel.binding_names),
        }


@processor(
    execution="manual",
    description="A binding declared for a stage this kernel has no module for",
)
class RayTracingStageMismatchProbe(_RayTracingKernelProbeBase):
    """The ticket's named validation case, at the line it belongs to.

    This kernel is built from ray-gen, miss and closest-hit modules and no
    other, so declaring a binding for `any_hit` is a claim no trace could ever
    make true — and a trace never revisits which stage reads what. The
    traceback proves the `create_ray_tracing_kernel` line raised: there is no
    kernel object to trace with afterwards.
    """

    def observe(self) -> dict:
        gpu = self.gpu_full_access

        def declare_the_scene_for_a_stage_this_kernel_has_no_module_for() -> None:
            _traced_triangle_kernel(
                gpu,
                {
                    SCENE_BINDING: ("acceleration_structure", ["any_hit"]),
                    TRACED_OUTPUT_BINDING: ("storage_image", ["ray_gen"]),
                },
            )

        stage_mismatch = _refusal_of(
            declare_the_scene_for_a_stage_this_kernel_has_no_module_for
        )
        stage_mismatch_traceback = _refusal_traceback_of(
            declare_the_scene_for_a_stage_this_kernel_has_no_module_for
        )

        # The same modules with the stage claim corrected build and trace, so
        # what the refusal rejected is the declaration and not the scene.
        self.trace_the_conformance_grid()
        return {
            "stage_mismatch": stage_mismatch,
            "stage_mismatch_traceback": stage_mismatch_traceback,
            "the_corrected_declaration_traced": True,
        }


@processor(
    execution="manual",
    description="A buffer-kind binding a trace cannot name a surface for",
)
class RayTracingBufferBindingRefusalProbe(_RayTracingKernelProbeBase):
    """A uniform-buffer binding is reflected, and refused at the trace.

    The name is read back off the kernel rather than spelled here — how
    reflection names a uniform block is the shader's business, and the refusal
    has to name whatever it named.
    """

    def observe(self) -> dict:
        tinted = self.gpu_full_access.create_ray_tracing_kernel(
            stages=[
                {"stage": "ray_gen", "source": RAY_GENERATION_WITH_A_UNIFORM_BUFFER_GLSL},
                {"stage": "miss", "source": MISS_GLSL},
                {"stage": "closest_hit", "source": CLOSEST_HIT_GLSL},
            ],
            groups=RAY_TRACING_GROUPS,
            label="python-tinted-traced-triangle",
        )
        binding_names = list(tinted.binding_names)
        buffer_binding = binding_names[2]
        buffer_kind_binding = _refusal_of(
            lambda: tinted.trace(
                bindings={
                    SCENE_BINDING: self.top_level_structure,
                    TRACED_OUTPUT_BINDING: self.traced_output,
                    buffer_binding: self.traced_output,
                },
                grid=(SURFACE_WIDTH, SURFACE_HEIGHT, 1),
            )
        )
        return {
            "binding_names": binding_names,
            "buffer_binding": buffer_binding,
            "buffer_kind_binding": buffer_kind_binding,
        }


@processor(
    execution="manual",
    description="An acceleration structure is named by its handle, or not at all",
)
class AccelerationStructureHandleRefusalProbe(_RayTracingKernelProbeBase):
    """Nothing publishes an acceleration structure for another processor to
    resolve, so a surface id is not a spelling for one — and the structure a
    trace binds is the top-level one, which is what holds the instances."""

    def observe(self) -> dict:
        gpu = self.gpu_full_access
        grid = (SURFACE_WIDTH, SURFACE_HEIGHT, 1)

        a_surface_where_a_structure_belongs = _refusal_of(
            lambda: self.kernel.trace(
                bindings={
                    SCENE_BINDING: self.traced_output,
                    TRACED_OUTPUT_BINDING: self.traced_output,
                },
                grid=grid,
            )
        )
        a_bottom_level_structure_at_the_trace = _refusal_of(
            lambda: self.kernel.trace(
                bindings={
                    SCENE_BINDING: self.bottom_level_structure,
                    TRACED_OUTPUT_BINDING: self.traced_output,
                },
                grid=grid,
            )
        )
        a_top_level_structure_as_an_instance = _refusal_of(
            lambda: gpu.build_tlas(instances=[{"blas": self.top_level_structure}])
        )
        vertices_that_are_not_triangles = _refusal_of(
            lambda: gpu.build_triangles_blas(
                vertices=[0.0, 1.0, 2.0, 3.0], indices=TRIANGLE_INDICES
            )
        )
        indices_that_are_not_triangles = _refusal_of(
            lambda: gpu.build_triangles_blas(
                vertices=TRIANGLE_VERTICES, indices=[0, 1]
            )
        )
        an_index_past_the_last_vertex = _refusal_of(
            lambda: gpu.build_triangles_blas(
                vertices=TRIANGLE_VERTICES, indices=[0, 1, 3]
            )
        )
        return {
            "a_surface_where_a_structure_belongs": a_surface_where_a_structure_belongs,
            "a_bottom_level_structure_at_the_trace": a_bottom_level_structure_at_the_trace,
            "a_top_level_structure_as_an_instance": a_top_level_structure_as_an_instance,
            "vertices_that_are_not_triangles": vertices_that_are_not_triangles,
            "indices_that_are_not_triangles": indices_that_are_not_triangles,
            "an_index_past_the_last_vertex": an_index_past_the_last_vertex,
        }
