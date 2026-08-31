# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""The right-hand half: the same scene, ray traced.

The kernel is an object — built once in `setup()` where the capability is
Full, traced per frame in `process()` with its bindings passed by name. So is
the scene: `build_triangles_blas` takes the unit cube's triangles and
`build_tlas` places nine instances of it, and neither hands back an id string
— the handle each build returned is the whole way to name it.

Four shader modules and four groups, because the shader binding table is
where a ray tracer's control flow lives: a ray that hits nothing runs the sky
miss shader, and the shadow ray a hit casts runs the *second* miss shader
instead by naming a different miss index. That is what a hit shader can ask
that a fragment shader cannot — is anything between this point and the light
— and it is the whole visible difference across the divider.

The floor's reflection bounces from ray generation rather than recursing in
the hit shader: a shader may declare only one incoming payload, so a hit
shader cannot both receive at a location and trace at it. Bouncing here keeps
`max_recursion_depth` at 2 — one hit shader casting one shadow ray — and lets
the reflected hit cast a shadow ray of its own, so the reflections have
shadows in them.
"""

from __future__ import annotations

import struct

from streamlib import (
    ProcessorOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    output,
    processor,
)
from streamlib._engine import AccelerationStructureHandle, RayTracingKernel

from processors.showcase_box_scene import (
    SHOWCASE_SCENE_GLSL,
    UNIT_CUBE_CORNER_POSITIONS,
    UNIT_CUBE_TRIANGLE_INDICES,
    showcase_tlas_instances,
)

RAY_TRACED_FRAME_OUTPUT_PORT = "ray_traced_frame_to_downstream"

# The shaders' own names for the two resources they bind, read off them by
# reflection at construction. A trace resolves against these and never against
# slot order.
SCENE_STRUCTURE_BINDING = "scene_structure"
TRACED_FRAME_BINDING = "traced_frame"

TEXTURE_FORMAT = "rgba8_unorm"
# Written by the trace as a storage image, then sampled by the compositor in
# its own helper process.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# `float elapsed_seconds` and `float aspect`, little-endian at the wire like
# every push constant. The declared size must equal what the shaders reflect,
# so the two cannot drift.
SCENE_PUSH_CONSTANT_FORMAT = "<ff"
SCENE_PUSH_CONSTANT_SIZE = struct.calcsize(SCENE_PUSH_CONSTANT_FORMAT)

NANOSECONDS_PER_SECOND = 1_000_000_000

# The sky miss shader is index 0 in the miss region of the shader binding
# table and the shadow miss shader is index 1, in the order the groups below
# declare them. A `traceRayEXT` names which one it wants, which is how one
# scene structure serves two entirely different questions.
SKY_MISS_INDEX = 0
SHADOW_MISS_INDEX = 1

_RAY_TRACING_PREAMBLE = (
    "#version 460\n"
    "#extension GL_EXT_ray_tracing : require\n"
    f"#define SKY_MISS_INDEX {SKY_MISS_INDEX}\n"
    f"#define SHADOW_MISS_INDEX {SHADOW_MISS_INDEX}\n"
)

# Declared identically in every shader that touches it: ray generation traces
# with it, the sky miss shader fills it in, the hit shader answers into it.
_PRIMARY_RAY_PAYLOAD_GLSL = """
struct PrimaryRayPayload {
    vec3 colour;
    vec3 world_position;
    vec3 world_normal;
    float mirror_strength;
};
"""

_SCENE_PUSH_CONSTANTS_GLSL = """
layout(push_constant) uniform ScenePushConstants {
    float elapsed_seconds;
    float aspect;
} scene;
"""

RAY_GENERATION_GLSL = (
    _RAY_TRACING_PREAMBLE
    + SHOWCASE_SCENE_GLSL
    + _PRIMARY_RAY_PAYLOAD_GLSL
    + _SCENE_PUSH_CONSTANTS_GLSL
    + """
layout(set = 0, binding = 0) uniform accelerationStructureEXT scene_structure;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D traced_frame;

layout(location = 0) rayPayloadEXT PrimaryRayPayload primary;

void main() {
    vec2 pixel_centre = vec2(gl_LaunchIDEXT.xy) + vec2(0.5);
    vec2 on_screen = pixel_centre / vec2(gl_LaunchSizeEXT.xy) * 2.0 - 1.0;
    on_screen.x *= scene.aspect;
    on_screen.y = -on_screen.y;

    vec3 eye, forward, right, up;
    showcase_camera_basis(scene.elapsed_seconds, eye, forward, right, up);
    float half_height = tan(CAMERA_FIELD_OF_VIEW_RADIANS * 0.5);
    vec3 direction = normalize(forward
                               + right * (on_screen.x * half_height)
                               + up * (on_screen.y * half_height));

    traceRayEXT(scene_structure, gl_RayFlagsOpaqueEXT, 0xff, 0, 0, SKY_MISS_INDEX,
                eye, 0.001, direction, 400.0, 0);
    vec3 colour = primary.colour;

    // One bounce off the mirror floor, iterated rather than recursed. The
    // reflected ray runs the same hit shader, so it casts its own shadow ray
    // and the reflections come back with shadows already in them.
    if (primary.mirror_strength > 0.0) {
        float strength = primary.mirror_strength;
        vec3 surface = primary.world_position;
        vec3 normal = primary.world_normal;
        traceRayEXT(scene_structure, gl_RayFlagsOpaqueEXT, 0xff, 0, 0, SKY_MISS_INDEX,
                    surface + normal * SHADOW_RAY_BIAS, 0.001,
                    reflect(direction, normal), 400.0, 0);
        colour = mix(colour, primary.colour, strength);
    }

    imageStore(traced_frame, ivec2(gl_LaunchIDEXT.xy), vec4(colour, 1.0));
}
"""
)

SKY_MISS_GLSL = (
    _RAY_TRACING_PREAMBLE
    + SHOWCASE_SCENE_GLSL
    + _PRIMARY_RAY_PAYLOAD_GLSL
    + """
layout(location = 0) rayPayloadInEXT PrimaryRayPayload primary;

void main() {
    primary.colour = sky_colour_towards(gl_WorldRayDirectionEXT);
    primary.world_position = vec3(0.0);
    primary.world_normal = vec3(0.0);
    primary.mirror_strength = 0.0;
}
"""
)

# The second miss shader, and the reason the shadow ray is cheap: a shadow ray
# runs no closest-hit shader and stops at the first thing it touches, so all
# it can do is arrive here — which means nothing was in the way.
SHADOW_MISS_GLSL = (
    _RAY_TRACING_PREAMBLE
    + """
layout(location = 1) rayPayloadInEXT float lit_fraction;

void main() {
    lit_fraction = 1.0;
}
"""
)

CLOSEST_HIT_GLSL = (
    _RAY_TRACING_PREAMBLE
    + SHOWCASE_SCENE_GLSL
    + _PRIMARY_RAY_PAYLOAD_GLSL
    + _SCENE_PUSH_CONSTANTS_GLSL
    + """
layout(set = 0, binding = 0) uniform accelerationStructureEXT scene_structure;

layout(location = 0) rayPayloadInEXT PrimaryRayPayload primary;
layout(location = 1) rayPayloadEXT float lit_fraction;

void main() {
    // Object space here *is* unit-cube space: every instance's transform maps
    // the one cube onto its box, so the same helpers the rasterizer uses on
    // its interpolated corner work on the hit position.
    vec3 in_unit_cube = gl_ObjectRayOriginEXT + gl_HitTEXT * gl_ObjectRayDirectionEXT;
    vec3 world_position = gl_WorldRayOriginEXT + gl_HitTEXT * gl_WorldRayDirectionEXT;
    // The instance transforms are axis-aligned scale and translation only, so
    // the forward transform carries a face normal without an inverse
    // transpose.
    vec3 world_normal = normalize(mat3(gl_ObjectToWorldEXT)
                                  * unit_cube_face_normal(in_unit_cube));

    int box = gl_InstanceCustomIndexEXT;
    vec3 base_colour = SHOWCASE_BOX_COLOURS[box] * box_edge_darkening(in_unit_cube);

    // The question a fragment shader cannot ask. A hit leaves the fraction at
    // zero because the shadow ray skips closest-hit shaders entirely — only
    // reaching the shadow miss shader lights this point.
    vec3 light = light_position_at(scene.elapsed_seconds);
    lit_fraction = 0.0;
    traceRayEXT(scene_structure,
                gl_RayFlagsOpaqueEXT
                    | gl_RayFlagsTerminateOnFirstHitEXT
                    | gl_RayFlagsSkipClosestHitShaderEXT,
                0xff, 0, 0, SHADOW_MISS_INDEX,
                world_position + world_normal * SHADOW_RAY_BIAS, 0.001,
                normalize(light - world_position), distance(light, world_position), 1);

    primary.colour = directly_lit_colour(base_colour, world_position, world_normal,
                                         scene.elapsed_seconds, lit_fraction);
    primary.world_position = world_position;
    primary.world_normal = world_normal;
    primary.mirror_strength = (box == FLOOR_BOX_INDEX) ? FLOOR_MIRROR_STRENGTH : 0.0;
}
"""
)

# One module per shader stage, and one group per shader binding table record
# over them. A group names its modules by index into `stages`, which is what
# lets two modules fill the same stage — here, the two miss shaders.
RAY_TRACING_STAGES = [
    {"stage": "ray_gen", "source": RAY_GENERATION_GLSL},
    {"stage": "miss", "source": SKY_MISS_GLSL},
    {"stage": "miss", "source": SHADOW_MISS_GLSL},
    {"stage": "closest_hit", "source": CLOSEST_HIT_GLSL},
]
RAY_TRACING_GROUPS = [
    {"kind": "general", "general_stage": 0},
    {"kind": "general", "general_stage": 1},
    {"kind": "general", "general_stage": 2},
    {"kind": "triangles_hit", "closest_hit_stage": 3},
]

# The hit shader traces too, so the scene structure is read from two stages —
# a declaration may widen what reflection found but never narrow it.
DECLARED_BINDINGS: dict[str, str | tuple[str, list[str]]] = {
    SCENE_STRUCTURE_BINDING: ("acceleration_structure", ["ray_gen", "closest_hit"]),
    TRACED_FRAME_BINDING: ("storage_image", ["ray_gen"]),
}

# Primary ray, then the one shadow ray its hit shader casts. The reflection
# bounce costs no depth because ray generation fires it, not a hit shader.
MAX_RECURSION_DEPTH = 2


@processor(
    execution="continuous",
    interval_ms=16,
    description="The showcase scene with ray-traced shadows and a mirror floor",
)
class RayTracedSceneRenderer:
    """Ray tracing on: hard shadows that track the light, and reflections."""

    def __init__(self, width: int = 1280, height: int = 720) -> None:
        self.frame_width = width
        self.frame_height = height

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        gpu = ctx.gpu_full_access
        self.output_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        # One bottom-level structure over the unit cube's triangles, placed
        # nine times by the top-level one. The scene is built here and not per
        # frame because `build_tlas` is a method on the Full capability, which
        # only `setup()` holds — so the camera and the light are what move.
        self.cube_structure: AccelerationStructureHandle = gpu.build_triangles_blas(
            vertices=list(UNIT_CUBE_CORNER_POSITIONS),
            indices=list(UNIT_CUBE_TRIANGLE_INDICES),
            label="showcase-unit-cube",
        )
        self.scene_structure: AccelerationStructureHandle = gpu.build_tlas(
            instances=showcase_tlas_instances(self.cube_structure),
            label="showcase-scene",
        )
        self.trace_kernel: RayTracingKernel = gpu.create_ray_tracing_kernel(
            stages=RAY_TRACING_STAGES,
            groups=RAY_TRACING_GROUPS,
            max_recursion_depth=MAX_RECURSION_DEPTH,
            push_constant_size=SCENE_PUSH_CONSTANT_SIZE,
            # Asserted against the shaders' own reflection, so renaming a
            # binding on one side of this file is refused here at construction
            # rather than at the first trace.
            bindings=DECLARED_BINDINGS,
            label="showcase-ray-tracer",
        )
        self.first_process_at_ns: int | None = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self.first_process_at_ns is None:
            self.first_process_at_ns = ctx.time
        elapsed_seconds = (ctx.time - self.first_process_at_ns) / NANOSECONDS_PER_SECOND

        traced_frame_texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, self.frame_width, self.frame_height
        )
        self.trace_kernel.trace(
            bindings={
                SCENE_STRUCTURE_BINDING: self.scene_structure,
                TRACED_FRAME_BINDING: traced_frame_texture,
            },
            grid=(self.frame_width, self.frame_height, 1),
            push_constants=struct.pack(
                SCENE_PUSH_CONSTANT_FORMAT,
                elapsed_seconds,
                self.frame_width / self.frame_height,
            ),
        )

        ctx.outputs.write(
            RAY_TRACED_FRAME_OUTPUT_PORT,
            {
                "surface_id": traced_frame_texture.surface_id,
                "width": self.frame_width,
                "height": self.frame_height,
                "timestamp_ns": ctx.time,
            },
        )

    @output(description="The scene with ray-traced shadows and a mirror floor")
    def ray_traced_frame_to_downstream(self) -> VideoFrame: ...
