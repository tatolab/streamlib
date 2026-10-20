# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The right-hand half: the showcase scene traced by a ray-tracing kernel.

Importable as `nodes.ray_traced_scene_renderer:RayTracedSceneRenderer`, which
is the name the engine spawns this node's child interpreter with. The kernel and
both acceleration structures are objects built once in `setup()`; a trace binds
them by name.
"""

from __future__ import annotations

import struct
from collections.abc import Sequence
from dataclasses import dataclass

from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    node,
)

from nodes.showcase_box_scene import (
    SHOWCASE_SCENE_GLSL,
    UNIT_CUBE_CORNER_POSITIONS,
    UNIT_CUBE_TRIANGLE_INDICES,
    orbit_azimuths_at,
    showcase_tlas_instances,
)

RAY_TRACED_FRAME_OUTPUT_PORT = "ray_traced_frame_to_downstream"

# The shaders' own names for the two resources they bind, read off them by
# reflection. A trace resolves against these, never against slot order.
SCENE_STRUCTURE_BINDING = "scene_structure"
TRACED_FRAME_BINDING = "traced_frame"

TEXTURE_FORMAT = "rgba8_unorm"
# Written by the trace as a storage image, then sampled by the compositor and
# read by an encoder, each in its own process.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# `float camera_azimuth; float light_azimuth; float aspect;`, little-endian
# like every push constant.
SCENE_PUSH_CONSTANT_FORMAT = "<fff"
SCENE_PUSH_CONSTANT_SIZE = struct.calcsize(SCENE_PUSH_CONSTANT_FORMAT)

# Miss-region indices in the shader binding table, in the order the groups
# below declare the miss shaders. A `traceRayEXT` names which one it wants.
SKY_MISS_INDEX = 0
SHADOW_MISS_INDEX = 1

_RAY_TRACING_PREAMBLE = (
    "#version 460\n"
    "#extension GL_EXT_ray_tracing : require\n"
    f"#define SKY_MISS_INDEX {SKY_MISS_INDEX}\n"
    f"#define SHADOW_MISS_INDEX {SHADOW_MISS_INDEX}\n"
)

# Declared identically in ray generation, the sky miss shader and the hit
# shader, which all read or write it at payload location 0.
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
    float camera_azimuth;
    float light_azimuth;
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
    showcase_camera_basis(scene.camera_azimuth, eye, forward, right, up);
    float half_height = tan(CAMERA_FIELD_OF_VIEW_RADIANS * 0.5);
    vec3 direction = normalize(forward
                               + right * (on_screen.x * half_height)
                               + up * (on_screen.y * half_height));

    traceRayEXT(scene_structure, gl_RayFlagsOpaqueEXT, 0xff, 0, 0, SKY_MISS_INDEX,
                eye, 0.001, direction, 400.0, 0);
    vec3 colour = primary.colour;

    // One bounce off the mirror floor, iterated here rather than recursed in
    // the hit shader, which cannot both receive and trace at one payload
    // location. The reflected ray runs the same hit shader, so it casts its
    // own shadow ray.
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

# A shadow ray skips closest-hit shaders and stops at its first hit, so
# arriving here is the only thing that can happen to it — and it means nothing
# was in the way.
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
    // Every instance's transform maps the one unit cube onto its box, so
    // object space is unit-cube space.
    vec3 in_unit_cube = gl_ObjectRayOriginEXT + gl_HitTEXT * gl_ObjectRayDirectionEXT;
    vec3 world_position = gl_WorldRayOriginEXT + gl_HitTEXT * gl_WorldRayDirectionEXT;
    // The transforms are axis-aligned scale and translation only, so the
    // forward transform carries a normal without an inverse transpose.
    vec3 world_normal = normalize(mat3(gl_ObjectToWorldEXT)
                                  * unit_cube_face_normal(in_unit_cube));

    int box = gl_InstanceCustomIndexEXT;
    vec3 base_colour = SHOWCASE_BOX_COLOURS[box] * box_edge_darkening(in_unit_cube);

    // The question a fragment shader cannot ask: is anything between this
    // point and the light? Only reaching the shadow miss shader lights it.
    vec3 light = light_position_at(scene.light_azimuth);
    lit_fraction = 0.0;
    traceRayEXT(scene_structure,
                gl_RayFlagsOpaqueEXT
                    | gl_RayFlagsTerminateOnFirstHitEXT
                    | gl_RayFlagsSkipClosestHitShaderEXT,
                0xff, 0, 0, SHADOW_MISS_INDEX,
                world_position + world_normal * SHADOW_RAY_BIAS, 0.001,
                normalize(light - world_position), distance(light, world_position), 1);

    primary.colour = directly_lit_colour(base_colour, world_position, world_normal,
                                         scene.light_azimuth, lit_fraction);
    primary.world_position = world_position;
    primary.world_normal = world_normal;
    primary.mirror_strength = (box == FLOOR_BOX_INDEX) ? FLOOR_MIRROR_STRENGTH : 0.0;
}
"""
)

# One module per shader, and one shader-binding-table group over them. A group
# names its modules by index into `stages`, which is how two modules fill the
# miss stage.
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

# Asserted against the shaders' reflection at construction. The hit shader
# traces too, so the scene structure is read from two stages.
DECLARED_BINDINGS: dict[str, str | tuple[str, Sequence[str]]] = {
    SCENE_STRUCTURE_BINDING: ("acceleration_structure", ["ray_gen", "closest_hit"]),
    TRACED_FRAME_BINDING: ("storage_image", ["ray_gen"]),
}

# The primary ray, then the one shadow ray its hit shader casts. The reflection
# bounce costs no depth because ray generation fires it.
MAX_RECURSION_DEPTH = 2


@dataclass
class RayTracedSceneRendererConfig:
    """RayTracedSceneRenderer's settings: the extent of the frame it traces."""

    width: int = 1280
    height: int = 720


@node(
    execution="continuous",
    interval_ms=16,
    description="The showcase scene with ray-traced shadows and a mirror floor",
)
class RayTracedSceneRenderer:
    """Ray tracing on: hard shadows that track the light, and reflections."""

    def __init__(self, config: RayTracedSceneRendererConfig) -> None:
        self.frame_width = config.width
        self.frame_height = config.height

    @node.output(description="The scene with ray-traced shadows and a mirror floor")
    def ray_traced_frame_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        gpu_full_access = ctx.gpu_full_access
        self.output_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        # Built here because `build_tlas` is on the full capability, which only
        # `setup()` holds — so the geometry is fixed and the camera and the
        # light are what move.
        self.cube_structure = gpu_full_access.build_triangles_blas(
            vertices=list(UNIT_CUBE_CORNER_POSITIONS),
            indices=list(UNIT_CUBE_TRIANGLE_INDICES),
            label="showcase-unit-cube",
        )
        self.scene_structure = gpu_full_access.build_tlas(
            instances=showcase_tlas_instances(self.cube_structure),
            label="showcase-scene",
        )
        self.trace_kernel = gpu_full_access.create_ray_tracing_kernel(
            stages=RAY_TRACING_STAGES,
            groups=RAY_TRACING_GROUPS,
            max_recursion_depth=MAX_RECURSION_DEPTH,
            push_constant_size=SCENE_PUSH_CONSTANT_SIZE,
            bindings=DECLARED_BINDINGS,
            label="showcase-ray-tracer",
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame_time_ns = ctx.time
        camera_azimuth, light_azimuth = orbit_azimuths_at(frame_time_ns)

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
                camera_azimuth,
                light_azimuth,
                self.frame_width / self.frame_height,
            ),
        )

        ray_traced_frame = VideoFrame(
            surface_id=traced_frame_texture.surface_id,
            width=self.frame_width,
            height=self.frame_height,
            timestamp_ns=frame_time_ns,
        )
        ctx.outputs.write(
            RAY_TRACED_FRAME_OUTPUT_PORT,
            ray_traced_frame.to_bytes(),
            timestamp_ns=frame_time_ns,
        )
