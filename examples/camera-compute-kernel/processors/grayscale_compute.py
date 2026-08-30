# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""Grayscale, as a GLSL compute kernel the engine compiles and dispatches.

The kernel is an object: built once in `setup()`, where the capability is
Full, and dispatched per frame in `process()` with its bindings passed by
name. Nothing here is a shader toolchain — the GLSL below is a Python string
until the engine compiles it, and the compiler ships inside the wheel.

The one step that is not the kernel is the landing copy. `CameraSource`
publishes a buffer-backed frame and a dispatch binds texture-backed surfaces
only, so each frame is copied device-to-device into a texture this processor
owns before the kernel can sample it. cupy does nothing here but that copy;
any DLPack-speaking GPU array package would serve, which is the point — the
pixels never touch the host.
"""

from __future__ import annotations

import struct

import cupy

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    ProcessorOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    output,
    processor,
)

CAMERA_FRAME_INPUT_PORT = "camera_frame_from_upstream"
GRAYSCALE_FRAME_OUTPUT_PORT = "grayscale_frame_to_downstream"

# The shader's own names for its two bindings, taken off it by reflection at
# construction. A dispatch resolves against these and never against slot
# order, so they are the contract between the GLSL below and the `dispatch`
# call — and they are deliberately not the port names above, because a port
# and a shader binding are two different contracts that happen to carry the
# same frame.
CAMERA_FRAME_BINDING = "camera_frame"
GRAYSCALE_FRAME_BINDING = "grayscale_frame"

# One format end to end, so no stage in the chain has to convert. The camera
# publishes RGBA8 and the window samples it.
TEXTURE_FORMAT = "rgba8_unorm"

# The texture the landing copy fills and the kernel samples. `copy_src` and
# `copy_dst` ride every acquire, which is what lets the copy write into it.
SAMPLED_TEXTURE_USAGE = ["texture_binding"]

# The texture the kernel writes as a storage image and the window then samples.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# The dispatch asks for one workgroup per tile of this size, and the shader
# declares the same size as its `local_size`. The two reach the shader as one
# number — see the `#define` below — because raising the Python without the
# GLSL leaves the right and bottom of every frame ungraded and nothing
# anywhere refuses.
WORKGROUP_TILE_SIZE = 8

# One `float strength`, little-endian at the wire like every push constant.
GRAYSCALE_STRENGTH_FORMAT = "<f"
GRAYSCALE_STRENGTH_SIZE = struct.calcsize(GRAYSCALE_STRENGTH_FORMAT)

# The `#define` is the only interpolated line: the body stays a plain string,
# so the shader's own braces need no doubling and it reads as GLSL.
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
    // The dispatch rounds up to whole workgroups, so the tiles along the right
    // and bottom edges run past a frame whose extent is not a multiple of the
    // tile. Those invocations have no texel to write.
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }
    // texelFetch rather than texture(): input and output are the same extent,
    // so there is nothing to filter and the fetch reads the exact texel.
    vec4 source = texelFetch(camera_frame, at, 0);
    // BT.601 luma, the weights the CRT-era standard published for it.
    float luma = dot(source.rgb, vec3(0.299, 0.587, 0.114));
    vec3 graded = mix(source.rgb, vec3(luma), dial.strength);
    imageStore(grayscale_frame, at, vec4(graded, source.a));
}
"""
)


def _workgroups_covering(pixels: int) -> int:
    """How many tiles it takes to cover `pixels`, the last one hanging over."""
    return (pixels + WORKGROUP_TILE_SIZE - 1) // WORKGROUP_TILE_SIZE


@processor(description="Grades each frame toward its luma with a compute kernel")
class GrayscaleCompute:
    """Camera frame in, the same picture in black and white out."""

    def __init__(self, strength: float = 1.0) -> None:
        if not 0.0 <= float(strength) <= 1.0:
            raise ValueError(
                f"GrayscaleCompute was configured with strength={strength} — the "
                f"dial runs from 0.0 (the picture untouched) to 1.0 (full luma)"
            )
        # Packed once, because the dial is fixed at construction. It is still
        # handed to every dispatch below: push constants travel with a
        # dispatch and never persist on the kernel, exactly as bindings do.
        self.strength_push_constants = struct.pack(
            GRAYSCALE_STRENGTH_FORMAT, float(strength)
        )

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Depth 1, unlike the output ring: `dispatch` returns with the GPU
        # work retired, and nothing outside this processor ever names this
        # texture, so the frame it holds is finished with before the next one
        # lands in it.
        self.camera_frame_landing_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT, SAMPLED_TEXTURE_USAGE, depth=1
        )
        self.grayscale_frame_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT, STORAGE_AND_SAMPLED_TEXTURE_USAGE
        )
        self.grayscale_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=GRAYSCALE_COMPUTE_GLSL,
            push_constant_size=GRAYSCALE_STRENGTH_SIZE,
            # Asserted against the shader's own reflection, so renaming a
            # binding on one side of this file is refused here at construction
            # rather than at the first dispatch.
            bindings={
                CAMERA_FRAME_BINDING: "sampled_texture",
                GRAYSCALE_FRAME_BINDING: "storage_image",
            },
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read(CAMERA_FRAME_INPUT_PORT, into=VideoFrame)
        if frame is None:
            return

        camera_frame_texture = self.camera_frame_landing_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        # The frame is a DLPack producer in its own right, so this is the whole
        # read — GPU-resident, and the cast object's claim is what holds the
        # camera's pixels still for the length of the copy. Leaving the scope
        # blits the write into the texture, ordered ahead of the engine's next
        # read of it.
        with camera_frame_texture.as_device_tensor() as writable_texture:
            cupy.from_dlpack(writable_texture)[...] = cupy.from_dlpack(frame)

        grayscale_frame_texture = self.grayscale_frame_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        self.grayscale_kernel.dispatch(
            bindings={
                CAMERA_FRAME_BINDING: camera_frame_texture,
                GRAYSCALE_FRAME_BINDING: grayscale_frame_texture,
            },
            group_count=(
                _workgroups_covering(frame.width),
                _workgroups_covering(frame.height),
                1,
            ),
            push_constants=self.strength_push_constants,
        )

        ctx.outputs.write(
            GRAYSCALE_FRAME_OUTPUT_PORT,
            {
                "surface_id": grayscale_frame_texture.surface_id,
                "width": frame.width,
                "height": frame.height,
                # Carried from the frame this one was derived from rather than
                # re-read off the clock: the timestamp is the ordering
                # primitive downstream, and restamping here would date the
                # picture to when the effect ran instead of when the camera
                # captured it.
                "timestamp_ns": frame.timestamp_ns,
            },
        )

    @input(
        delivery_profile="newest",
        description="Frames from the camera, as VideoFrame bags",
    )
    def camera_frame_from_upstream(self) -> VideoFrame: ...

    @output(description="The same frames graded toward their luma by the kernel")
    def grayscale_frame_to_downstream(self) -> VideoFrame: ...
