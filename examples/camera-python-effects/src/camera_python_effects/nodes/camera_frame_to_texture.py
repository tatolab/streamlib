# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Lands the camera's frame in a texture the effect kernels can bind.

`CameraSource` publishes buffer-backed frames, and a kernel binding resolves
texture-backed surfaces only — a draw handed a buffer-backed surface id is
refused by name. So the chain starts here: the engine copies the frame
device-to-device into a texture this node's ring holds, and that is what is
published. The pixels never touch the host and no GPU array package is
involved.
"""

from __future__ import annotations

from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    node,
)

from ..gpu_surface_conventions import (
    SAMPLED_ONLY_TEXTURE_USAGE,
    TEXTURE_FORMAT,
)


@node(description="Copies the camera's frame into a bindable device texture")
class CameraFrameToTexture:
    """Buffer-backed camera frame in, texture-backed frame out."""

    @node.input(delivery_profile="newest")
    def video_from_camera(self) -> VideoFrame: ...

    @node.output()
    def video_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.output_ring = NodeOutputTextureRing(
            TEXTURE_FORMAT, SAMPLED_ONLY_TEXTURE_USAGE
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_camera", into=VideoFrame)
        if frame is None:
            return

        texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        # The typed read's claim is what holds the camera's pixels still for
        # the length of the copy, which returns once the next reader of the
        # texture would see them.
        ctx.gpu_limited_access.copy_surface_to_surface(frame.surface_id, texture)

        ctx.outputs.write(
            "video_to_downstream",
            VideoFrame(
                surface_id=texture.surface_id,
                width=frame.width,
                height=frame.height,
                timestamp_ns=frame.timestamp_ns,
                color_info=frame.color_info,
            ).to_bytes(),
            timestamp_ns=frame.timestamp_ns,
        )
