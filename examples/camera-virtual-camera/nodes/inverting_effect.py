# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The effect between the camera and the second virtual camera.

Importable as `nodes.inverting_effect:InvertingEffect`, which is the name the
engine spawns this node's child interpreter with.

The camera's frames reach the passthrough virtual camera and this effect alike,
and both resolve the same engine-owned surface. So this effect never edits the
frame it was handed: it publishes the inverted picture from a texture ring of
its own.
"""

from typing import Annotated, Callable

import numpy
from tatolab.stream import (
    Description,
    InputPin,
    NodeOutputTextureRing,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    TextureToWrite,
    node,
)

# What a `VirtualCameraSink` samples on its way to the device's buffers.
INVERTED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"
INVERTED_FRAME_TEXTURE_USAGE = ["texture_binding"]


@node(execution="reactive")
def InvertingEffect(
    *,
    video_from_upstream: InputPin[
        Annotated[Texture, StreamDelivery("newest"), Description("Frames from the camera")]
    ],
    video_to_downstream: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(INVERTED_FRAME_TEXTURE_FORMAT),
            Description("The same frames, colour-inverted"),
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads each camera frame on the CPU and publishes its colour-inverted twin."""

    inverted_frame_texture_ring = NodeOutputTextureRing(
        INVERTED_FRAME_TEXTURE_FORMAT, INVERTED_FRAME_TEXTURE_USAGE
    )

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        camera_frame = video_from_upstream.read()
        if camera_frame is None:
            return

        inverted_texture = inverted_frame_texture_ring.next_texture_for_this_frame(
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
        # sets the device's colorimetry from the frame's colour, so the two
        # cameras present the same picture under the same time and colour.
        video_to_downstream.write(
            TextureToWrite.of_texture(inverted_texture, camera_frame.colour),
            timestamp_ns=camera_frame.timestamp_ns,
        )

    return each_frame
