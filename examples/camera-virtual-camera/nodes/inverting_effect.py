# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The effect between the camera and the second virtual camera.

Importable as `nodes.inverting_effect:InvertingEffect`, which is the name the
engine spawns this node's child interpreter with.

The camera's frames reach the passthrough virtual camera and this effect alike,
and both resolve the same engine-owned surface. So this effect never edits the
frame it was handed: it publishes the inverted picture from a texture ring of
its own.
"""

import numpy
from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    node,
)

# What a `VirtualCameraSink` samples on its way to the device's buffers.
INVERTED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"
INVERTED_FRAME_TEXTURE_USAGE = ["texture_binding"]


@node
class InvertingEffect:
    """Reads each camera frame on the CPU and publishes its colour-inverted twin."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> VideoFrame: ...

    @node.output()
    def video_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.inverted_frame_texture_ring = NodeOutputTextureRing(
            INVERTED_FRAME_TEXTURE_FORMAT, INVERTED_FRAME_TEXTURE_USAGE
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        camera_frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if camera_frame is None:
            return

        inverted_texture = self.inverted_frame_texture_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, camera_frame.width, camera_frame.height
        )
        # A read-only host view: the passthrough camera is showing these pixels.
        camera_pixels = numpy.from_dlpack(camera_frame, device="cpu")
        # `unlock` in a `finally` rather than a `with`: the handle's context
        # manager closes the surface on the way out, and the ring owns this
        # slot for the node's life.
        inverted_texture.lock(read_only=False)
        try:
            inverted_pixels = inverted_texture.as_numpy()
            # Colour channels only — inverting alpha would erase the picture.
            numpy.subtract(255, camera_pixels[..., :3], out=inverted_pixels[..., :3])
            inverted_pixels[..., 3] = camera_pixels[..., 3]
        finally:
            inverted_texture.unlock()

        # The capture stamp is what consumers order by, and a `VirtualCameraSink`
        # sets the device's colorimetry from `color_info`, so the two cameras
        # present the same picture under the same time and colour.
        inverted_frame = VideoFrame(
            surface_id=inverted_texture.surface_id,
            width=camera_frame.width,
            height=camera_frame.height,
            timestamp_ns=camera_frame.timestamp_ns,
            color_info=camera_frame.color_info,
        )
        ctx.outputs.write(
            "video_to_downstream",
            inverted_frame.to_bytes(),
            timestamp_ns=camera_frame.timestamp_ns,
        )
