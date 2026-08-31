# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A wide-FOV lens this desk does not have, as a GLSL compute kernel.

The pipeline downstream is the one a monocular drone runs: rectify the lens,
then detect. Testing it needs a barrelled frame, and an ordinary webcam does
not produce one — so this processor applies the distortion a fisheye lens
would have baked in, and hands the result on as if the camera had.

The kernel is an object: built once in `setup()`, where the capability is
Full, and dispatched per frame in `process()` with its bindings passed by
name. The GLSL below is a Python string until the engine compiles it, and the
compiler ships inside the wheel — there is no shader toolchain here.

The one step that is not the kernel is the landing copy. `CameraSource`
publishes a buffer-backed frame and a dispatch binds texture-backed surfaces
only, so each frame is copied device-to-device into a texture this processor
owns before the kernel can sample it. torch does nothing in this module but
that copy; the pixels never touch the host.
"""

from __future__ import annotations

import struct

import torch

from processors.radial_distortion_model import (
    RADIAL_DISTORTION_MODEL_GLSL,
    workgroups_covering,
)
from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    ProcessorOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    output,
    processor,
)

CAMERA_FRAME_INPUT_PORT = "camera_frame_from_upstream"
FISHEYE_FRAME_OUTPUT_PORT = "fisheye_frame_to_downstream"

# The shader's own names for its two bindings, taken off it by reflection at
# construction. A dispatch resolves against these and never against slot
# order, so they are the contract between the GLSL below and the `dispatch`
# call — and they are deliberately not the port names above, because a port
# and a shader binding are two different contracts that happen to carry the
# same frame.
CAMERA_FRAME_BINDING = "camera_frame"
FISHEYE_FRAME_BINDING = "fisheye_frame"

# One format end to end, so no stage in the chain has to convert. The camera
# publishes RGBA8, the rectifier samples it and the window shows it.
TEXTURE_FORMAT = "rgba8_unorm"

# The texture the landing copy fills and the kernel samples. `copy_src` and
# `copy_dst` ride every acquire, which is what lets the copy write into it.
SAMPLED_TEXTURE_USAGE = ["texture_binding"]

# The texture the kernel writes as a storage image, which is then both shown
# in a window and sampled by the rectifier's own kernel.
STORAGE_AND_SAMPLED_TEXTURE_USAGE = ["storage_binding", "texture_binding"]

# Two consumers hold frames from this ring at once — the window and the
# rectifier — where the app's other rings serve one. Depth bounds how far
# behind a consumer may fall before the producer overwrites what it is still
# reading, so it is the consumer count that sets it, not the frame rate.
FISHEYE_FRAME_RING_DEPTH = 3

# Two `float`s, little-endian at the wire like every push constant. The
# rectifier packs a third alongside these — see its own module — because it
# needs an answer this shader does not.
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
    // The dispatch rounds up to whole workgroups, so the tiles along the right
    // and bottom edges run past a frame whose extent is not a multiple of the
    // tile. Those invocations have no texel to write.
    if (at.x >= extent.x || at.y >= extent.y) {
        return;
    }

    // A pull, not a push: this invocation owns one output texel and goes
    // looking for the input texel that lands on it. Under a barrel the factor
    // is below one, so it reaches inward — which is what pushes the picture
    // outward and curves the frame in at its edges.
    vec2 centre = frame_centre(extent);
    float scale = radial_scale(normalised_radius(at, extent), lens.k1, lens.k2);
    vec2 source_texel = centre + (vec2(at) - centre) * scale;

    // Outside the sensor a real lens projects nothing. The engine's sampler
    // clamps to the edge, which would smear the border outward into a plausible
    // picture of something that was never in frame, so this says black instead.
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


@processor(description="Barrels each camera frame the way a wide-FOV lens would")
class SyntheticFisheyeLens:
    """Camera frame in, the same picture through a fisheye lens out."""

    def __init__(
        self,
        radial_distortion_k1: float = -0.25,
        radial_distortion_k2: float = 0.0,
    ) -> None:
        # Packed once, because a lens does not change its coefficients while
        # it is bolted on. It is still handed to every dispatch below: push
        # constants travel with a dispatch and never persist on the kernel,
        # exactly as bindings do.
        self.lens_coefficient_push_constants = struct.pack(
            LENS_COEFFICIENT_PUSH_CONSTANT_FORMAT,
            float(radial_distortion_k1),
            float(radial_distortion_k2),
        )
        self.radial_distortion_k1 = float(radial_distortion_k1)
        self.radial_distortion_k2 = float(radial_distortion_k2)

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Depth 1, unlike the output ring: `dispatch` returns with the GPU
        # work retired, and nothing outside this processor ever names this
        # texture, so the frame it holds is finished with before the next one
        # lands in it.
        self.camera_frame_landing_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT, SAMPLED_TEXTURE_USAGE, depth=1
        )
        self.fisheye_frame_ring = ProcessorOutputTextureRing(
            TEXTURE_FORMAT,
            STORAGE_AND_SAMPLED_TEXTURE_USAGE,
            depth=FISHEYE_FRAME_RING_DEPTH,
        )
        self.fisheye_warp_kernel = ctx.gpu_full_access.create_compute_kernel(
            source=FISHEYE_WARP_GLSL,
            push_constant_size=LENS_COEFFICIENT_PUSH_CONSTANT_SIZE,
            # Asserted against the shader's own reflection, so renaming a
            # binding on one side of this file is refused here at construction
            # rather than at the first dispatch.
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

        camera_frame_landing_texture = (
            self.camera_frame_landing_ring.next_texture_for_this_frame(
                ctx.gpu_limited_access, frame.width, frame.height
            )
        )
        # The frame is a DLPack producer in its own right, so this is the whole
        # read — GPU-resident, and the cast object's claim is what holds the
        # camera's pixels still for the length of the copy. Leaving the scope
        # blits the write into the texture, ordered ahead of the engine's next
        # read of it.
        with camera_frame_landing_texture.as_device_tensor() as writable_texture:
            torch.from_dlpack(writable_texture)[...] = torch.from_dlpack(frame)

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

        ctx.outputs.write(
            FISHEYE_FRAME_OUTPUT_PORT,
            {
                "surface_id": fisheye_frame_texture.surface_id,
                "width": frame.width,
                "height": frame.height,
                # Carried from the frame this one was derived from rather than
                # re-read off the clock: the timestamp is the ordering
                # primitive downstream, and restamping here would date the
                # picture to when the lens ran instead of when the camera
                # captured it.
                "timestamp_ns": frame.timestamp_ns,
            },
        )

    @input(
        delivery_profile="newest",
        description="Frames from the camera, as VideoFrame bags",
    )
    def camera_frame_from_upstream(self) -> VideoFrame: ...

    @output(description="The same frames with a barrel distortion applied")
    def fisheye_frame_to_downstream(self) -> VideoFrame: ...
