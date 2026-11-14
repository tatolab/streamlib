# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The right-hand half: the showcase scene traced by a ray-tracing kernel.

Importable as `nodes.ray_traced_scene_renderer:RayTracedSceneRenderer`, which
is the name the runtime spawns this node's child interpreter with. The kernel and
both acceleration structures are objects built once in `setup()`; a trace binds
them by name.
"""

from __future__ import annotations

import struct
from collections.abc import Sequence
from dataclasses import dataclass

from tatolab.stream import (
    Image,
    ImageFrameToWrite,
    NodeOutputTextureRing,
    Output,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    node,
)

from nodes.showcase_box_scene import (
    SHOWCASE_RENDERED_FRAME_COLOUR,
    SHOWCASE_SCENE_SLANG,
    UNIT_CUBE_CORNER_POSITIONS,
    UNIT_CUBE_TRIANGLE_INDICES,
    orbit_azimuths_at,
    showcase_tlas_instances,
)

# The shaders' own names for the two resources they bind, read off them by
# reflection. A trace resolves against these, never against slot order.
SCENE_STRUCTURE_BINDING = "scene_structure"
TRACED_FRAME_BINDING = "traced_frame"

TEXTURE_FORMAT = "rgba8_unorm"
# Written by the trace as a storage image, then sampled by the compositor in
# its own process.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# `float camera_azimuth; float light_azimuth; float aspect;`, little-endian
# like every push constant.
SCENE_PUSH_CONSTANT_FORMAT = "<fff"
SCENE_PUSH_CONSTANT_SIZE = struct.calcsize(SCENE_PUSH_CONSTANT_FORMAT)

# Miss-region indices in the shader binding table, in the order the groups
# below declare the miss shaders. A `TraceRay` names which one it wants.
SKY_MISS_INDEX = 0
SHADOW_MISS_INDEX = 1

_RAY_TRACING_PREAMBLE_SLANG = (
    f"static const uint SKY_MISS_INDEX = {SKY_MISS_INDEX};\n"
    f"static const uint SHADOW_MISS_INDEX = {SHADOW_MISS_INDEX};\n"
)

# Declared identically in ray generation, the sky miss shader and the hit
# shader, which all read or write it.
_PRIMARY_RAY_PAYLOAD_SLANG = """
struct PrimaryRayPayload {
    float3 colour;
    float3 world_position;
    float3 world_normal;
    float mirror_strength;
};
"""

# Declared identically in the hit shader, which casts the shadow ray, and the
# shadow miss shader, which answers it.
_SHADOW_RAY_PAYLOAD_SLANG = """
struct ShadowRayPayload {
    float lit_fraction;
};
"""

_SCENE_PUSH_CONSTANTS_SLANG = """
struct ScenePushConstants {
    float camera_azimuth;
    float light_azimuth;
    float aspect;
};
[[vk::push_constant]] ConstantBuffer<ScenePushConstants> scene;
"""

_SCENE_STRUCTURE_BINDING_SLANG = """
[[vk::binding(0, 0)]] RaytracingAccelerationStructure scene_structure;
"""

RAY_GENERATION_SLANG = (
    _RAY_TRACING_PREAMBLE_SLANG
    + SHOWCASE_SCENE_SLANG
    + _PRIMARY_RAY_PAYLOAD_SLANG
    + _SCENE_PUSH_CONSTANTS_SLANG
    + _SCENE_STRUCTURE_BINDING_SLANG
    + """
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> traced_frame;

PrimaryRayPayload traced_primary_ray(float3 origin, float3 direction) {
    RayDesc ray;
    ray.Origin = origin;
    ray.TMin = 0.001;
    ray.Direction = direction;
    ray.TMax = 400.0;
    PrimaryRayPayload primary;
    TraceRay(scene_structure, RAY_FLAG_FORCE_OPAQUE, 0xff, 0, 0, SKY_MISS_INDEX, ray, primary);
    return primary;
}

void main() {
    uint2 launch = DispatchRaysIndex().xy;
    float2 pixel_centre = float2(launch) + float2(0.5);
    float2 on_screen = pixel_centre / float2(DispatchRaysDimensions().xy) * 2.0 - 1.0;
    on_screen.x *= scene.aspect;
    on_screen.y = -on_screen.y;

    float3 eye, forward, right, up;
    showcase_camera_basis(scene.camera_azimuth, eye, forward, right, up);
    float half_height = tan(CAMERA_FIELD_OF_VIEW_RADIANS * 0.5);
    float3 direction = normalize(forward
                                 + right * (on_screen.x * half_height)
                                 + up * (on_screen.y * half_height));

    PrimaryRayPayload primary = traced_primary_ray(eye, direction);
    float3 colour = primary.colour;

    // One bounce off the mirror floor, iterated here rather than recursed in
    // the hit shader, which would spend a level of recursion depth on it. The
    // reflected ray runs the same hit shader, so it casts its own shadow ray.
    if (primary.mirror_strength > 0.0) {
        float strength = primary.mirror_strength;
        float3 normal = primary.world_normal;
        PrimaryRayPayload reflected = traced_primary_ray(
            primary.world_position + normal * SHADOW_RAY_BIAS, reflect(direction, normal));
        colour = lerp(colour, reflected.colour, strength);
    }

    traced_frame[launch] = float4(colour, 1.0);
}
"""
)

SKY_MISS_SLANG = (
    SHOWCASE_SCENE_SLANG
    + _PRIMARY_RAY_PAYLOAD_SLANG
    + """
void main(inout PrimaryRayPayload primary) {
    primary.colour = sky_colour_towards(WorldRayDirection());
    primary.world_position = float3(0.0);
    primary.world_normal = float3(0.0);
    primary.mirror_strength = 0.0;
}
"""
)

# A shadow ray skips closest-hit shaders and stops at its first hit, so
# arriving here is the only thing that can happen to it — and it means nothing
# was in the way.
SHADOW_MISS_SLANG = (
    _SHADOW_RAY_PAYLOAD_SLANG
    + """
void main(inout ShadowRayPayload shadow) {
    shadow.lit_fraction = 1.0;
}
"""
)

CLOSEST_HIT_SLANG = (
    _RAY_TRACING_PREAMBLE_SLANG
    + SHOWCASE_SCENE_SLANG
    + _PRIMARY_RAY_PAYLOAD_SLANG
    + _SHADOW_RAY_PAYLOAD_SLANG
    + _SCENE_PUSH_CONSTANTS_SLANG
    + _SCENE_STRUCTURE_BINDING_SLANG
    + """
void main(inout PrimaryRayPayload primary, BuiltInTriangleIntersectionAttributes hit) {
    // Every instance's transform maps the one unit cube onto its box, so
    // object space is unit-cube space.
    float3 in_unit_cube = ObjectRayOrigin() + RayTCurrent() * ObjectRayDirection();
    float3 world_position = WorldRayOrigin() + RayTCurrent() * WorldRayDirection();
    // The transforms are axis-aligned scale and translation only, so the
    // forward transform carries a normal without an inverse transpose.
    float3 world_normal = normalize(
        mul(ObjectToWorld3x4(), float4(unit_cube_face_normal(in_unit_cube), 0.0)));

    // The instance's custom index, which the node set to its box.
    int box = int(InstanceID());
    float3 base_colour = SHOWCASE_BOX_COLOURS[box] * box_edge_darkening(in_unit_cube);

    // The question a fragment shader cannot ask: is anything between this
    // point and the light? Only reaching the shadow miss shader lights it.
    float3 light = light_position_at(scene.light_azimuth);
    RayDesc towards_light;
    towards_light.Origin = world_position + world_normal * SHADOW_RAY_BIAS;
    towards_light.TMin = 0.001;
    towards_light.Direction = normalize(light - world_position);
    towards_light.TMax = distance(light, world_position);
    ShadowRayPayload shadow;
    shadow.lit_fraction = 0.0;
    TraceRay(scene_structure,
             RAY_FLAG_FORCE_OPAQUE
                 | RAY_FLAG_ACCEPT_FIRST_HIT_AND_END_SEARCH
                 | RAY_FLAG_SKIP_CLOSEST_HIT_SHADER,
             0xff, 0, 0, SHADOW_MISS_INDEX, towards_light, shadow);

    primary.colour = directly_lit_colour(base_colour, world_position, world_normal,
                                         scene.light_azimuth, shadow.lit_fraction);
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
    {"stage": "ray_gen", "source": RAY_GENERATION_SLANG},
    {"stage": "miss", "source": SKY_MISS_SLANG},
    {"stage": "miss", "source": SHADOW_MISS_SLANG},
    {"stage": "closest_hit", "source": CLOSEST_HIT_SLANG},
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


@node.meta(description="The showcase scene with ray-traced shadows and a mirror floor")
@node.process(execution="continuous", interval_ms=16)
class RayTracedSceneRenderer:
    """Ray tracing on: hard shadows that track the light, and reflections."""

    def __init__(self, config: RayTracedSceneRendererConfig) -> None:
        self.frame_width = config.width
        self.frame_height = config.height

    ray_traced_frame_to_downstream = Output(
        Image, description="The scene with ray-traced shadows and a mirror floor"
    )

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

        ctx.outputs.write(
            self.ray_traced_frame_to_downstream,
            ImageFrameToWrite.of_texture(traced_frame_texture, SHOWCASE_RENDERED_FRAME_COLOUR),
            timestamp_ns=frame_time_ns,
        )
