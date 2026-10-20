# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A wide-FOV lens this desk does not have, as a GLSL compute kernel.

Importable as `nodes.synthetic_fisheye_lens:SyntheticFisheyeLens`, which is the
name the engine spawns this node's child interpreter with.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    log,
    node,
)

from nodes.radial_distortion_model import (
    DEFAULT_RADIAL_DISTORTION_K1,
    DEFAULT_RADIAL_DISTORTION_K2,
    FRAME_TEXTURE_FORMAT,
    RADIAL_DISTORTION_MODEL_GLSL,
    STORAGE_AND_SAMPLED_TEXTURE_USAGE,
    workgroups_covering,
)

CAMERA_FRAME_INPUT_PORT = "camera_frame_from_upstream"
FISHEYE_FRAME_OUTPUT_PORT = "fisheye_frame_to_downstream"

# The shader's own names for its bindings, which `dispatch` resolves against.
CAMERA_FRAME_BINDING = "camera_frame"
FISHEYE_FRAME_BINDING = "fisheye_frame"

SAMPLED_TEXTURE_USAGE = ["texture_binding"]

# Two consumers hold the lens's frames at once: the encoder and the rectifier.
FISHEYE_FRAME_RING_DEPTH = 3

# Push constants are little-endian at the wire: `float k1; float k2;`.
LENS_COEFFICIENT_PUSH_CONSTANT_FORMAT = "<2f"
LENS_COEFFICIENT_PUSH_CONSTANT_SIZE = struct.calcsize(
    LENS_COEFFICIENT_PUSH_CONSTANT_FORMAT
)

FISHEYE_WARP_GLSL = (
    RADIAL_DISTORTION_MODEL_GLSL
    + """
layout(set = 0, binding = 0) uniform sampler2D camera_frame;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D fisheye_frame;

layout(push_constant) uniform LensCoefficients {
    float k1;
    float k2;
} lens;

void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(fisheye_frame);
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }

    // A pull: this invocation owns one output texel and samples the input
    // texel that lands on it. Under a barrel the scale is below one, so it
    // reaches inward, which pushes the picture outward.
    vec2 centre = frame_centre(extent);
    float scale = radial_scale(normalised_radius(at, extent), lens.k1, lens.k2);
    vec2 source_texel = centre + (vec2(at) - centre) * scale;

    // Outside the sensor a lens projects nothing. The sampler clamps to the
    // edge, which would smear the border outward, so this writes black.
    vec2 last_texel = vec2(extent) - 1.0;
    if (any(lessThan(source_texel, vec2(0.0)))
        || any(greaterThan(source_texel, last_texel))) {
        imageStore(fisheye_frame, at, vec4(0.0, 0.0, 0.0, 1.0));
        return;
    }

    vec3 sampled = texture(
        camera_frame, texel_to_sampler_coordinates(source_texel, extent)
    ).rgb;
    imageStore(fisheye_frame, at, vec4(sampled, 1.0));
}
"""
)


@dataclass
class SyntheticFisheyeLensConfig:
    """SyntheticFisheyeLens's settings, as `stream_builder.add(..., config={...})` spells them."""

    radial_distortion_k1: float = DEFAULT_RADIAL_DISTORTION_K1
    radial_distortion_k2: float = DEFAULT_RADIAL_DISTORTION_K2


@node
class SyntheticFisheyeLens:
    """Barrels each camera frame the way a wide-FOV lens would, on the GPU."""

    @node.input(delivery_profile="newest", description="Frames from the camera")
    def camera_frame_from_upstream(self) -> VideoFrame: ...

    @node.output(description="The same frames with a barrel distortion applied")
    def fisheye_frame_to_downstream(self) -> VideoFrame: ...

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
            source=FISHEYE_WARP_GLSL,
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
        frame = ctx.inputs.read(CAMERA_FRAME_INPUT_PORT, into=VideoFrame)
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
        fisheye_frame = VideoFrame(
            surface_id=fisheye_frame_texture.surface_id,
            width=frame.width,
            height=frame.height,
            timestamp_ns=frame.timestamp_ns,
            color_info=frame.color_info,
        )
        ctx.outputs.write(
            FISHEYE_FRAME_OUTPUT_PORT,
            fisheye_frame.to_bytes(),
            timestamp_ns=frame.timestamp_ns,
        )
