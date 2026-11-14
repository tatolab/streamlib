# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Grayscale, as a Slang compute kernel the runtime compiles and dispatches.

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
    Image,
    ImageFrameToWrite,
    Input,
    NodeOutputTextureRing,
    Output,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    node,
)

# The shader's own names for its two bindings, read off it by reflection. A
# dispatch resolves against these, never against slot order, and they are not
# the port names: a port and a shader binding are two different contracts.
CAMERA_FRAME_BINDING = "camera_frame"
GRAYSCALE_FRAME_BINDING = "grayscale_frame"

# One format end to end: the camera's `rgba` and a texture's `rgba8_unorm` are
# one format to the landing copy.
TEXTURE_FORMAT = "rgba8_unorm"

# The texture the landing copy fills and the kernel samples. `copy_src` and
# `copy_dst` ride every acquire, which is what lets the copy write into it.
SAMPLED_TEXTURE_USAGE = ["texture_binding"]

# The texture the kernel writes as a storage image and downstream then reads.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# The dispatch's tile and the shader's `numthreads` must agree, and nothing
# refuses a mismatch — the right and bottom of every frame would go ungraded —
# so the Slang takes this number from here.
WORKGROUP_TILE_SIZE = 8

# One `float strength`, little-endian at the wire like every push constant.
GRAYSCALE_STRENGTH_FORMAT = "<f"
GRAYSCALE_STRENGTH_SIZE = struct.calcsize(GRAYSCALE_STRENGTH_FORMAT)

GRAYSCALE_COMPUTE_SLANG = f"""
static const uint WORKGROUP_TILE_SIZE = {WORKGROUP_TILE_SIZE};
"""  """
[[vk::binding(0, 0)]] Sampler2D camera_frame;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> grayscale_frame;

struct GrayscaleDial {
    float strength;
};
[[vk::push_constant]] ConstantBuffer<GrayscaleDial> dial;

[numthreads(WORKGROUP_TILE_SIZE, WORKGROUP_TILE_SIZE, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    uint2 extent;
    grayscale_frame.GetDimensions(extent.x, extent.y);
    // The dispatch rounds up to whole workgroups, so the edge tiles of a frame
    // whose extent is not a multiple of the tile run past it.
    if (any(invocation.xy >= extent)) {
        return;
    }
    // Input and output share an extent, so there is nothing to filter.
    float4 source = camera_frame.Load(int3(invocation.xy, 0));
    // BT.601 luma weights.
    float luma = dot(source.rgb, float3(0.299, 0.587, 0.114));
    float3 graded = lerp(source.rgb, float3(luma), dial.strength);
    grayscale_frame[invocation.xy] = float4(graded, source.a);
}
"""


def _workgroups_covering(pixels: int) -> int:
    """How many tiles it takes to cover `pixels`, the last one hanging over."""
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


@dataclass
class GrayscaleComputeConfig:
    """GrayscaleCompute's settings, as its node's config in the graph document spells them."""

    strength: float = 1.0


@node.meta(description="Grades each frame toward its luma with a compute kernel")
@node.process
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

    camera_frame_from_upstream = Input(
        Image, delivery="newest", description="Frames from the camera"
    )
    grayscale_frame_to_downstream = Output(
        Image, description="The same frames graded toward their luma by the kernel"
    )

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
            source=GRAYSCALE_COMPUTE_SLANG,
            push_constant_size=GRAYSCALE_STRENGTH_SIZE,
            # Asserted against the shader's reflection, so a binding renamed on
            # one side of this file is refused here rather than at dispatch.
            bindings={
                CAMERA_FRAME_BINDING: "sampled_texture",
                GRAYSCALE_FRAME_BINDING: "storage_image",
            },
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(self.camera_frame_from_upstream)
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
        ctx.outputs.write(
            self.grayscale_frame_to_downstream,
            ImageFrameToWrite.of_texture(grayscale_frame_texture, frame.colour),
            timestamp_ns=frame.timestamp_ns,
        )
