# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A wide-FOV lens this desk does not have, as a Slang compute kernel.

Importable as `nodes.synthetic_fisheye_lens:SyntheticFisheyeLens`, which is the
name the runtime spawns this node's child interpreter with.
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
    log,
    node,
)

from nodes.radial_distortion_model import (
    DEFAULT_RADIAL_DISTORTION_K1,
    DEFAULT_RADIAL_DISTORTION_K2,
    FRAME_TEXTURE_FORMAT,
    RADIAL_DISTORTION_MODEL_SLANG,
    STORAGE_AND_SAMPLED_TEXTURE_USAGE,
    workgroups_covering,
)

# The shader's own names for its bindings, which `dispatch` resolves against.
CAMERA_FRAME_BINDING = "camera_frame"
FISHEYE_FRAME_BINDING = "fisheye_frame"

SAMPLED_TEXTURE_USAGE = ["texture_binding"]

# Two consumers can hold the lens's frames at once: the viewer and the rectifier.
FISHEYE_FRAME_RING_DEPTH = 3

# Push constants are little-endian at the wire: `float k1; float k2;`.
LENS_COEFFICIENT_PUSH_CONSTANT_FORMAT = "<2f"
LENS_COEFFICIENT_PUSH_CONSTANT_SIZE = struct.calcsize(
    LENS_COEFFICIENT_PUSH_CONSTANT_FORMAT
)

FISHEYE_WARP_SLANG = (
    RADIAL_DISTORTION_MODEL_SLANG
    + """
[[vk::binding(0, 0)]] Sampler2D camera_frame;
[[vk::binding(1, 0)]] [format("rgba8")] RWTexture2D<float4> fisheye_frame;

struct LensCoefficients {
    float k1;
    float k2;
};
[[vk::push_constant]] ConstantBuffer<LensCoefficients> lens;

[numthreads(WORKGROUP_TILE_SIZE, WORKGROUP_TILE_SIZE, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    int2 at = int2(invocation.xy);
    int2 extent = frame_extent_of(fisheye_frame);
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }

    // A pull: this invocation owns one output texel and samples the input
    // texel that lands on it. Under a barrel the scale is below one, so it
    // reaches inward, which pushes the picture outward.
    float2 centre = frame_centre(extent);
    float scale = radial_scale(normalised_radius(at, extent), lens.k1, lens.k2);
    float2 source_texel = centre + (float2(at) - centre) * scale;

    // Outside the sensor a lens projects nothing. The sampler clamps to the
    // edge, which would smear the border outward, so this writes black.
    float2 last_texel = float2(extent) - 1.0;
    if (any(source_texel < float2(0.0)) || any(source_texel > last_texel)) {
        fisheye_frame[invocation.xy] = float4(0.0, 0.0, 0.0, 1.0);
        return;
    }

    // Level 0 named: a compute stage has no derivatives to choose one by.
    float3 sampled = camera_frame.SampleLevel(
        texel_to_sampler_coordinates(source_texel, extent), 0.0
    ).rgb;
    fisheye_frame[invocation.xy] = float4(sampled, 1.0);
}
"""
)


@dataclass
class SyntheticFisheyeLensConfig:
    """SyntheticFisheyeLens's settings, as its node's config in the graph document spells them."""

    radial_distortion_k1: float = DEFAULT_RADIAL_DISTORTION_K1
    radial_distortion_k2: float = DEFAULT_RADIAL_DISTORTION_K2


@node.meta(description="Barrels each camera frame the way a wide-FOV lens would, on the GPU")
@node.process
class SyntheticFisheyeLens:
    """Camera frame in, the same picture through a synthetic fisheye lens out."""

    camera_frame_from_upstream = Input(
        Image, delivery="newest", description="Frames from the camera"
    )
    fisheye_frame_to_downstream = Output(
        Image, description="The same frames with a barrel distortion applied"
    )

    def __init__(self, config: SyntheticFisheyeLensConfig) -> None:
        self.radial_distortion_k1 = float(config.radial_distortion_k1)
        self.radial_distortion_k2 = float(config.radial_distortion_k2)
        # Push constants travel with each dispatch and never persist on the
        # kernel, so this is handed to every one.
        self.lens_coefficient_push_constants = struct.pack(
            LENS_COEFFICIENT_PUSH_CONSTANT_FORMAT,
            self.radial_distortion_k1,
            self.radial_distortion_k2,
        )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Depth 1: `dispatch` returns with the GPU work retired, and nothing
        # outside this node names the landing texture.
        self.camera_frame_landing_ring = NodeOutputTextureRing(
            FRAME_TEXTURE_FORMAT, SAMPLED_TEXTURE_USAGE, depth=1
        )
        self.fisheye_frame_ring = NodeOutputTextureRing(
            FRAME_TEXTURE_FORMAT,
            STORAGE_AND_SAMPLED_TEXTURE_USAGE,
            depth=FISHEYE_FRAME_RING_DEPTH,
        )
        self.fisheye_warp_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=FISHEYE_WARP_SLANG,
            push_constant_size=LENS_COEFFICIENT_PUSH_CONSTANT_SIZE,
            bindings={
                CAMERA_FRAME_BINDING: "sampled_texture",
                FISHEYE_FRAME_BINDING: "storage_image",
            },
        )
        log.info(
            "synthetic fisheye lens ready",
            radial_distortion_k1=self.radial_distortion_k1,
            radial_distortion_k2=self.radial_distortion_k2,
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(self.camera_frame_from_upstream)
        if frame is None:
            return

        # The camera publishes a buffer-backed frame and a dispatch samples
        # textures only, so the engine copies it device-to-device into one.
        camera_frame_landing_texture = (
            self.camera_frame_landing_ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, frame.width, frame.height
            )
        )
        ctx.gpu_limited_access.copy_surface_to_surface(
            frame.surface_id, camera_frame_landing_texture
        )

        fisheye_frame_texture = self.fisheye_frame_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        self.fisheye_warp_kernel.dispatch(
            bindings={
                CAMERA_FRAME_BINDING: camera_frame_landing_texture,
                FISHEYE_FRAME_BINDING: fisheye_frame_texture,
            },
            group_count=(
                workgroups_covering(frame.width),
                workgroups_covering(frame.height),
                1,
            ),
            push_constants=self.lens_coefficient_push_constants,
        )

        # The capture's timestamp, not the lens's: downstream orders by it.
        ctx.outputs.write(
            self.fisheye_frame_to_downstream,
            ImageFrameToWrite.of_texture(fisheye_frame_texture, frame.colour),
            timestamp_ns=frame.timestamp_ns,
        )
