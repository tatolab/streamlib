# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""The two halves, cut down the middle by a compute kernel.

Fan-in: two producers, each in its own child interpreter, publishing into one
consumer. What arrives on either port is a bag naming a surface — an id, an
extent and a timestamp, no pixels — and the dispatch binds both ids straight
into the shader. There is no landing copy here, unlike a camera: a kernel
binding resolves texture-backed surfaces, and a kernel output is exactly
that, whichever process acquired it.

The traced side paces the composite. Both renderers run their own clocks, so
the two ports rarely deliver on the same wake; compositing when the traced
frame lands, against the newest rasterized one held, costs one dispatch per
displayed frame instead of two and leaves the left half at most one interval
behind the right — far below what an eye reads as a difference.
"""

from __future__ import annotations

import struct

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    ProcessorOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    output,
    processor,
)
from streamlib._engine import ComputeKernel

RASTERIZED_FRAME_INPUT_PORT = "rasterized_frame_from_upstream"
RAY_TRACED_FRAME_INPUT_PORT = "ray_traced_frame_from_upstream"
SPLIT_SCREEN_FRAME_OUTPUT_PORT = "split_screen_frame_to_downstream"

# The shader's own names for its three bindings, read off it by reflection.
RASTERIZED_FRAME_BINDING = "rasterized_frame"
RAY_TRACED_FRAME_BINDING = "ray_traced_frame"
SPLIT_SCREEN_FRAME_BINDING = "split_screen_frame"

TEXTURE_FORMAT = "rgba8_unorm"
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

WORKGROUP_TILE_SIZE = 8

# One `float split_fraction`, little-endian at the wire like every push
# constant.
SPLIT_PUSH_CONSTANT_FORMAT = "<f"
SPLIT_PUSH_CONSTANT_SIZE = struct.calcsize(SPLIT_PUSH_CONSTANT_FORMAT)

# The `#define`s are the only interpolated lines: the body stays a plain
# string, so the shader's own braces need no doubling and it reads as GLSL.
SPLIT_SCREEN_COMPUTE_GLSL = (
    f"#version 450\n#define WORKGROUP_TILE_SIZE {WORKGROUP_TILE_SIZE}\n"
    "#define DIVIDER_HALF_WIDTH_IN_PIXELS 2\n"
    "#define DIVIDER_COLOUR vec4(0.92, 0.94, 0.98, 1.0)\n"
    """
layout(local_size_x = WORKGROUP_TILE_SIZE, local_size_y = WORKGROUP_TILE_SIZE) in;

layout(set = 0, binding = 0) uniform sampler2D rasterized_frame;
layout(set = 0, binding = 1) uniform sampler2D ray_traced_frame;
layout(set = 0, binding = 2, rgba8) uniform writeonly image2D split_screen_frame;

layout(push_constant) uniform SplitDial {
    float split_fraction;
} dial;

void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(split_screen_frame);
    // The dispatch rounds up to whole workgroups, so the tiles along the right
    // and bottom edges run past a frame whose extent is not a multiple of the
    // tile. Those invocations have no texel to write.
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }

    int divide_at = int(float(extent.x) * dial.split_fraction);
    // texelFetch rather than texture(): all three surfaces are the same
    // extent, so there is nothing to filter and the fetch reads the exact
    // texel of whichever half this column belongs to.
    vec4 colour = at.x < divide_at
        ? texelFetch(rasterized_frame, at, 0)
        : texelFetch(ray_traced_frame, at, 0);
    if (abs(at.x - divide_at) <= DIVIDER_HALF_WIDTH_IN_PIXELS) {
        colour = DIVIDER_COLOUR;
    }
    imageStore(split_screen_frame, at, colour);
}
"""
)


def _workgroups_covering(pixels: int) -> int:
    """How many tiles it takes to cover `pixels`, the last one hanging over."""
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


@processor(description="Cuts the rasterized and ray-traced frames together")
class SplitScreenCompositor:
    """Rasterized on the left, ray traced on the right, one frame out."""

    def __init__(self, split_fraction: float = 0.5) -> None:
        if not 0.0 <= float(split_fraction) <= 1.0:
            raise ValueError(
                f"SplitScreenCompositor was configured with "
                f"split_fraction={split_fraction} — the dial runs from 0.0 (all "
                f"ray traced) to 1.0 (all rasterized), and 0.5 cuts down the middle"
            )
        # Packed once, because the dial is fixed at construction. It is still
        # handed to every dispatch below: push constants travel with a
        # dispatch and never persist on the kernel, exactly as bindings do.
        self.split_push_constants = struct.pack(
            SPLIT_PUSH_CONSTANT_FORMAT, float(split_fraction)
        )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.output_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        self.split_screen_kernel: ComputeKernel = (
            ctx.gpu_full_access.create_compute_kernel(
                source=SPLIT_SCREEN_COMPUTE_GLSL,
                push_constant_size=SPLIT_PUSH_CONSTANT_SIZE,
                # Asserted against the shader's own reflection, so renaming a
                # binding on one side of this file is refused here at
                # construction rather than at the first dispatch.
                bindings={
                    RASTERIZED_FRAME_BINDING: "sampled_texture",
                    RAY_TRACED_FRAME_BINDING: "sampled_texture",
                    SPLIT_SCREEN_FRAME_BINDING: "storage_image",
                },
            )
        )
        self.newest_rasterized_frame: dict | None = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        rasterized_frame = ctx.inputs.read(RASTERIZED_FRAME_INPUT_PORT)
        if rasterized_frame is not None:
            self.newest_rasterized_frame = rasterized_frame

        # The traced side is the pacer: a wake that brought only a rasterized
        # frame leaves it held for the next traced one rather than spending a
        # dispatch on a half that has not changed.
        ray_traced_frame = ctx.inputs.read(RAY_TRACED_FRAME_INPUT_PORT)
        if ray_traced_frame is None or self.newest_rasterized_frame is None:
            return

        width = ray_traced_frame["width"]
        height = ray_traced_frame["height"]
        split_screen_frame_texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, width, height
        )
        # Both upstream ids name textures acquired in other processes, and a
        # dispatch binds them as they are: the engine resolves a kernel
        # binding through the surface-share service exactly as it resolves one
        # of this processor's own.
        self.split_screen_kernel.dispatch(
            bindings={
                RASTERIZED_FRAME_BINDING: self.newest_rasterized_frame["surface_id"],
                RAY_TRACED_FRAME_BINDING: ray_traced_frame["surface_id"],
                SPLIT_SCREEN_FRAME_BINDING: split_screen_frame_texture,
            },
            group_count=(
                _workgroups_covering(width),
                _workgroups_covering(height),
                1,
            ),
            push_constants=self.split_push_constants,
        )

        ctx.outputs.write(
            SPLIT_SCREEN_FRAME_OUTPUT_PORT,
            {
                "surface_id": split_screen_frame_texture.surface_id,
                "width": width,
                "height": height,
                # Carried from the traced frame this composite paced on rather
                # than re-read off the clock: the timestamp is the ordering
                # primitive downstream, and restamping here would date the
                # picture to when the cut ran instead of when it was rendered.
                "timestamp_ns": ray_traced_frame["timestamp_ns"],
            },
        )

    @input(
        delivery_profile="newest",
        description="The rasterized half — direct lighting only",
    )
    def rasterized_frame_from_upstream(self) -> VideoFrame: ...

    @input(
        delivery_profile="newest",
        description="The ray-traced half — shadows and a mirror floor",
    )
    def ray_traced_frame_from_upstream(self) -> VideoFrame: ...

    @output(description="Rasterized left, ray traced right, one frame")
    def split_screen_frame_to_downstream(self) -> VideoFrame: ...
