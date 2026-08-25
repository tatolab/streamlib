# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Lands the camera's frame in a texture the effect kernels can bind.

`CameraSource` publishes buffer-backed frames, and a kernel binding resolves
texture-backed surfaces only — a draw handed a buffer-backed surface id is
refused by name. So the chain starts here: read the frame as a GPU tensor,
copy it device-to-device into a texture this processor acquired, and publish
that. cupy is doing nothing but the copy; any DLPack-speaking GPU package
would serve, which is the point — the frame reaches a third-party GPU stack
with no CPU copy in the path.
"""

from __future__ import annotations

import cupy

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    output,
    processor,
)

from ..gpu_surface_conventions import (
    SAMPLED_ONLY_TEXTURE_USAGE,
    video_frame_bag_naming,
)
from ..published_texture_ring import PublishedTextureRing


@processor(description="Copies the camera's frame into a bindable device texture")
class CameraFrameToTexture:
    """Buffer-backed camera frame in, texture-backed frame out."""

    @input(delivery_profile="latest")
    def video_from_camera(self) -> VideoFrame: ...

    @output()
    def video_to_downstream(self) -> VideoFrame: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.output_ring = PublishedTextureRing(SAMPLED_ONLY_TEXTURE_USAGE)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_camera", into=VideoFrame)
        if frame is None:
            return

        # The frame is a DLPack producer in its own right: this is the whole
        # read, GPU-resident, and the cast object's claim is what holds the
        # pixels still for the length of the copy.
        camera_pixels = cupy.from_dlpack(frame)

        texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        with texture.as_device_tensor() as writable_texture:
            cupy.from_dlpack(writable_texture)[...] = camera_pixels

        ctx.outputs.write(
            "video_to_downstream",
            video_frame_bag_naming(
                texture.surface_id, frame.width, frame.height, frame.timestamp_ns
            ),
        )
