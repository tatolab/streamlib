# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Lands the camera's frame in a texture the effect kernels can bind.

`CameraSource` publishes buffer-backed frames, and a kernel binding resolves
texture-backed surfaces only — a draw handed a buffer-backed surface id is
refused by name. So the chain starts here: the engine copies the frame
device-to-device into a texture this node's ring holds, and that is what is
published. The pixels never touch the host and no GPU array package is
involved.
"""

from __future__ import annotations

from typing import Annotated, Callable

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

from ..gpu_surface_conventions import (
    SAMPLED_ONLY_TEXTURE_USAGE,
    TEXTURE_FORMAT,
)


@node.meta(description="Copies the camera's frame into a bindable device texture")
@node(execution="reactive")
def CameraFrameToTexture(
    *,
    video_from_camera: InputPin[
        Annotated[Texture, StreamDelivery("newest"), Description("Frames from the camera")]
    ],
    video_to_downstream: OutputPin[
        Annotated[
            Texture,
            TexturePixelFormats(TEXTURE_FORMAT),
            Description("The same frames, in textures a kernel can bind"),
        ]
    ],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Buffer-backed camera frame in, texture-backed frame out."""

    output_ring = NodeOutputTextureRing(
        TEXTURE_FORMAT, SAMPLED_ONLY_TEXTURE_USAGE
    )

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        frame = video_from_camera.read()
        if frame is None:
            return

        texture = output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        # The read frame's claim is what holds the camera's pixels still for
        # the length of the copy, which returns once the next reader of the
        # texture would see them.
        ctx.gpu_limited_access.copy_surface_to_surface(frame.surface_id, texture)

        video_to_downstream.write(
            TextureToWrite.of_texture(texture, frame.colour),
            timestamp_ns=frame.timestamp_ns,
        )

    return each_frame
