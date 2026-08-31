# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""The left-hand half: the same scene, rasterized.

Ray tracing genuinely off — this is a graphics kernel drawing triangles, not a
ray-tracing kernel with its effects turned down. It reads the same generated
scene table, builds the same camera, and shades with the same function, so
the only thing that can differ across the divider is what a ray buys.

Two constraints shape it, and both are the Python graphics surface being
honest about what it is:

- **No vertex or index buffer reaches a Python processor**, so the vertex
  stage fabricates its positions from `gl_VertexIndex` against the unit cube
  the scene module generated into it, and `gl_InstanceIndex` picks the box.
- **The pass has no depth attachment** — depth attachments are an unbuilt
  engine capability, reachable from neither language. So this shader collapses
  faces the camera cannot see, and the app packs a back-to-front instance
  order into a push constant. Convex boxes on one ring make that exactly
  right rather than approximately.
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
from processors.showcase_box_scene import (
    SHOWCASE_BOXES,
    SHOWCASE_CUBE_MESH_GLSL,
    SHOWCASE_SCENE_GLSL,
    UNIT_CUBE_VERTEX_COUNT,
    orbit_azimuths_at,
    packed_ring_draw_order_furthest_first,
)

RASTERIZED_FRAME_OUTPUT_PORT = "rasterized_frame_to_downstream"

TEXTURE_FORMAT = "rgba8_unorm"
# Drawn into as a colour attachment, then sampled by the compositor in its own
# helper process.
RENDER_ATTACHMENT_AND_SAMPLED_TEXTURE_USAGE = ["render_attachment", "texture_binding"]

# The two orbit angles, the aspect, and `uint ring_draw_order` — the whole
# back-to-front order of the ring in one word, four bits per cube.
RASTER_PUSH_CONSTANT_FORMAT = "<fffI"
RASTER_PUSH_CONSTANT_SIZE = struct.calcsize(RASTER_PUSH_CONSTANT_FORMAT)

SHOWCASE_BOX_COUNT = len(SHOWCASE_BOXES)

# Declared identically in both stages: reflection unions them, and a block one
# stage spells shorter than the other is a size the declaration cannot match.
_RASTER_PUSH_CONSTANTS_GLSL = """
layout(push_constant) uniform RasterPushConstants {
    float camera_azimuth;
    float light_azimuth;
    float aspect;
    uint ring_draw_order;
} raster;
"""

RASTER_VERTEX_GLSL = (
    "#version 450\n"
    + SHOWCASE_SCENE_GLSL
    + SHOWCASE_CUBE_MESH_GLSL
    + _RASTER_PUSH_CONSTANTS_GLSL
    + """
layout(location = 0) out vec3 fragment_in_unit_cube;
layout(location = 1) out vec3 fragment_world_position;
layout(location = 2) out vec3 fragment_world_normal;
layout(location = 3) flat out int fragment_box;

void main() {
    // Instance 0 is the sky box and instance 1 the floor, which is why those
    // two indices lead the table. The rest are the ring, in the order the app
    // packed: furthest from the camera first.
    int box = gl_InstanceIndex < FIRST_RING_CUBE_BOX_INDEX
        ? gl_InstanceIndex
        : FIRST_RING_CUBE_BOX_INDEX
          + int((raster.ring_draw_order
                 >> uint(DRAW_ORDER_BITS_PER_CUBE
                         * (gl_InstanceIndex - FIRST_RING_CUBE_BOX_INDEX)))
                & 0xFu);

    vec3 in_unit_cube = UNIT_CUBE_CORNERS[UNIT_CUBE_TRIANGLE_INDICES[gl_VertexIndex]];
    // Six vertices of the index list share a face, which is how a stage with
    // no vertex buffer to read one from still has a normal.
    vec3 face_normal = UNIT_CUBE_FACE_NORMALS[gl_VertexIndex / 6];

    vec3 centre = SHOWCASE_BOX_CENTRES[box];
    vec3 size = SHOWCASE_BOX_SIZES[box];
    vec3 world_position = centre + in_unit_cube * size;

    fragment_in_unit_cube = in_unit_cube;
    fragment_world_position = world_position;
    fragment_world_normal = face_normal;
    fragment_box = box;

    vec3 eye, forward, right, up;
    showcase_camera_basis(raster.camera_azimuth, eye, forward, right, up);

    // Back-face culling done here rather than by the pipeline: the test is
    // against the face's own centre, so all six of its vertices agree and a
    // hidden face collapses whole. Doing it this way needs no winding
    // convention to be right. The sky box is the one the camera stands
    // inside, so its faces are the ones pointing away.
    float facing = dot(face_normal, eye - (centre + face_normal * 0.5 * size));
    if ((box == SKY_BOX_INDEX) ? (facing >= 0.0) : (facing <= 0.0)) {
        // Behind the near plane, identically for every vertex of the face:
        // clipped away, and zero-area even if it were not.
        gl_Position = vec4(0.0, 0.0, -1.0, 1.0);
        return;
    }

    // The inverse of what the ray generator does with this same basis: that
    // shader turns a pixel into a ray, this one turns a point into a pixel.
    // The y negation is Vulkan's clip space having its origin at the top.
    vec3 from_eye = world_position - eye;
    vec3 in_view = vec3(dot(from_eye, right), dot(from_eye, up), dot(from_eye, forward));
    float half_height = tan(CAMERA_FIELD_OF_VIEW_RADIANS * 0.5);
    gl_Position = vec4(in_view.x / (half_height * raster.aspect),
                       -in_view.y / half_height,
                       FAR_PLANE * (in_view.z - NEAR_PLANE) / (FAR_PLANE - NEAR_PLANE),
                       in_view.z);
}
"""
)

RASTER_FRAGMENT_GLSL = (
    "#version 450\n"
    + SHOWCASE_SCENE_GLSL
    + _RASTER_PUSH_CONSTANTS_GLSL
    + """
layout(location = 0) in vec3 fragment_in_unit_cube;
layout(location = 1) in vec3 fragment_world_position;
layout(location = 2) in vec3 fragment_world_normal;
layout(location = 3) flat in int fragment_box;

layout(location = 0) out vec4 out_colour;

void main() {
    if (fragment_box == SKY_BOX_INDEX) {
        // The same gradient the ray tracer's miss shader answers with, out of
        // the same shared function — so the two halves' skies match exactly
        // and the divider does not draw a seam of its own.
        vec3 eye = camera_position_at(raster.camera_azimuth);
        out_colour = vec4(sky_colour_towards(normalize(fragment_world_position - eye)), 1.0);
        return;
    }

    vec3 base_colour = SHOWCASE_BOX_COLOURS[fragment_box]
                       * box_edge_darkening(fragment_in_unit_cube);
    // Fully lit, always. Nothing in a rasterized pass can ask whether the
    // light actually reaches this point, and nothing can ask what the floor
    // reflects: both questions need a ray, and casting one is what the other
    // half of the picture does.
    out_colour = vec4(directly_lit_colour(base_colour,
                                          fragment_world_position,
                                          fragment_world_normal,
                                          raster.light_azimuth,
                                          1.0),
                      1.0);
}
"""
)


@processor(
    execution="continuous",
    interval_ms=16,
    description="The showcase scene rasterized — direct lighting and nothing else",
)
class RasterizedSceneRenderer:
    """Ray tracing off: flat direct lighting, no shadows, no reflections."""

    def __init__(self, width: int = 1280, height: int = 720) -> None:
        self.frame_width = width
        self.frame_height = height

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.output_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT, RENDER_ATTACHMENT_AND_SAMPLED_TEXTURE_USAGE
        )
        self.raster_kernel = ctx.gpu_full_access.create_graphics_kernel(
            color_attachment_formats=[TEXTURE_FORMAT],
            vertex_source=RASTER_VERTEX_GLSL,
            fragment_source=RASTER_FRAGMENT_GLSL,
            push_constant_size=RASTER_PUSH_CONSTANT_SIZE,
            # The vertex stage collapses hidden faces itself, so the pipeline
            # is asked to cull nothing.
            cull_mode="none",
            label="showcase-rasterizer",
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # Off the shared clock, never off a private epoch: the ray tracer is
        # another process reaching its own first frame at its own moment, and
        # the two halves have to be the same picture.
        camera_azimuth, light_azimuth = orbit_azimuths_at(ctx.time)

        rasterized_frame_texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, self.frame_width, self.frame_height
        )
        self.raster_kernel.draw(
            bindings={},
            color_targets=[rasterized_frame_texture],
            extent=(self.frame_width, self.frame_height),
            vertex_count=UNIT_CUBE_VERTEX_COUNT,
            instance_count=SHOWCASE_BOX_COUNT,
            push_constants=struct.pack(
                RASTER_PUSH_CONSTANT_FORMAT,
                camera_azimuth,
                light_azimuth,
                self.frame_width / self.frame_height,
                packed_ring_draw_order_furthest_first(camera_azimuth),
            ),
        )

        ctx.outputs.write(
            RASTERIZED_FRAME_OUTPUT_PORT,
            {
                "surface_id": rasterized_frame_texture.surface_id,
                "width": self.frame_width,
                "height": self.frame_height,
                "timestamp_ns": ctx.time,
            },
        )

    @output(description="The scene with direct lighting only — no rays cast")
    def rasterized_frame_to_downstream(self) -> VideoFrame: ...
