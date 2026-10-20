# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Grayscale, as a GLSL compute kernel the runtime compiles and dispatches.

Importable as `nodes.grayscale_compute:GrayscaleCompute`. The kernel is an
object: built once in `setup()`, where the capability is Full, and dispatched
per frame in `process()` with its bindings passed by name.

`CameraSource` publishes a buffer-backed frame and a dispatch binds
texture-backed surfaces only, so each frame is first copied device-to-device
into a texture this node owns. The engine does that copy, so the pixels never
touch the host and no GPU array package is involved.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    node,
)

CAMERA_FRAME_INPUT_PORT = "camera_frame_from_upstream"
GRAYSCALE_FRAME_OUTPUT_PORT = "grayscale_frame_to_downstream"

# The shader's own names for its two bindings, read off it by reflection. A
# dispatch resolves against these, never against slot order, and they are not
# the port names: a port and a shader binding are two different contracts.
CAMERA_FRAME_BINDING = "camera_frame"
GRAYSCALE_FRAME_BINDING = "grayscale_frame"

# One format end to end: the camera's `rgba` and a texture's `rgba8_unorm` are
# one format to the landing copy, and the encoder takes the kernel's output.
TEXTURE_FORMAT = "rgba8_unorm"

# The texture the landing copy fills and the kernel samples. `copy_src` and
# `copy_dst` ride every acquire, which is what lets the copy write into it.
SAMPLED_TEXTURE_USAGE = ["texture_binding"]

# The texture the kernel writes as a storage image and the encoder then reads.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# The dispatch's tile and the shader's `local_size` must agree, and nothing
# refuses a mismatch — the right and bottom of every frame would go ungraded —
# so the GLSL takes this number through the `#define` below.
WORKGROUP_TILE_SIZE = 8

# One `float strength`, little-endian at the wire like every push constant.
GRAYSCALE_STRENGTH_FORMAT = "<f"
GRAYSCALE_STRENGTH_SIZE = struct.calcsize(GRAYSCALE_STRENGTH_FORMAT)

GRAYSCALE_COMPUTE_GLSL = (
    f"#version 450\n#define WORKGROUP_TILE_SIZE {WORKGROUP_TILE_SIZE}\n"
    """
layout(local_size_x = WORKGROUP_TILE_SIZE, local_size_y = WORKGROUP_TILE_SIZE) in;

layout(set = 0, binding = 0) uniform sampler2D camera_frame;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D grayscale_frame;

layout(push_constant) uniform GrayscaleDial {
    float strength;
} dial;

void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(grayscale_frame);
    // The dispatch rounds up to whole workgroups, so the edge tiles of a frame
    // whose extent is not a multiple of the tile run past it.
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }
    // Input and output share an extent, so there is nothing to filter.
    vec4 source = texelFetch(camera_frame, at, 0);
    // BT.601 luma weights.
    float luma = dot(source.rgb, vec3(0.299, 0.587, 0.114));
    vec3 graded = mix(source.rgb, vec3(luma), dial.strength);
    imageStore(grayscale_frame, at, vec4(graded, source.a));
}
"""
)


def _workgroups_covering(pixels: int) -> int:
    """How many tiles it takes to cover `pixels`, the last one hanging over."""
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


@dataclass
class GrayscaleComputeConfig:
    """GrayscaleCompute's settings, as `stream_builder.add(..., config={...})` spells them."""

    strength: float = 1.0


@node(description="Grades each frame toward its luma with a compute kernel")
class GrayscaleCompute:
    """Camera frame in, the same picture in black and white out."""

    def __init__(self, config: GrayscaleComputeConfig) -> None:
        strength = float(config.strength)
        if not 0.0 <= strength <= 1.0:
            raise ValueError(
                f"GrayscaleCompute was configured with strength={strength} — the "
                f"dial runs from 0.0 (the picture untouched) to 1.0 (full luma)"
            )
        # Push constants travel with a dispatch and never persist on the
        # kernel, so these bytes are handed to every dispatch.
        self.strength_push_constants = struct.pack(GRAYSCALE_STRENGTH_FORMAT, strength)

    @node.input(
        delivery_profile="newest",
        description="Frames from the camera, as VideoFrame records",
    )
    def camera_frame_from_upstream(self) -> VideoFrame: ...

    @node.output(description="The same frames graded toward their luma by the kernel")
    def grayscale_frame_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Depth 1: `dispatch` returns with the GPU work retired, and nothing
        # outside this node names the landing texture.
        self.camera_frame_landing_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, SAMPLED_TEXTURE_USAGE, depth=1
        )
        self.grayscale_frame_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        self.grayscale_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=GRAYSCALE_COMPUTE_GLSL,
            push_constant_size=GRAYSCALE_STRENGTH_SIZE,
            # Asserted against the shader's reflection, so a binding renamed on
            # one side of this file is refused here rather than at dispatch.
            bindings={
                CAMERA_FRAME_BINDING: "sampled_texture",
                GRAYSCALE_FRAME_BINDING: "storage_image",
            },
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(CAMERA_FRAME_INPUT_PORT, into=VideoFrame)
        if frame is None:
            return

        camera_frame_landing_texture = (
            self.camera_frame_landing_ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, frame.width, frame.height
            )
        )
        # `frame` holds its surface against producer reuse, so the camera's
        # pixels stay put for the length of the copy.
        ctx.gpu_limited_access.copy_surface_to_surface(
            frame.surface_id, camera_frame_landing_texture
        )

        grayscale_frame_texture = self.grayscale_frame_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        self.grayscale_kernel.dispatch(
            bindings={
                CAMERA_FRAME_BINDING: camera_frame_landing_texture,
                GRAYSCALE_FRAME_BINDING: grayscale_frame_texture,
            },
            group_count=(
                _workgroups_covering(frame.width),
                _workgroups_covering(frame.height),
                1,
            ),
            push_constants=self.strength_push_constants,
        )

        # The capture's timestamp, not the effect's: downstream orders frames by it.
        grayscale_frame = VideoFrame(
            surface_id=grayscale_frame_texture.surface_id,
            width=frame.width,
            height=frame.height,
            timestamp_ns=frame.timestamp_ns,
            color_info=frame.color_info,
        )
        ctx.outputs.write(
            GRAYSCALE_FRAME_OUTPUT_PORT,
            grayscale_frame.to_bytes(),
            timestamp_ns=frame.timestamp_ns,
        )
