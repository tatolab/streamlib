# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""The effect between the camera and the second virtual camera.

Importable as `processors.inverting_effect:InvertingEffect`, which is the name
the engine spawns this processor's child interpreter with.

Unlike the effect `streamlib new` scaffolds, this one publishes into textures
of its own rather than editing the frame it was handed. The camera's output
feeds two consumers here — the passthrough sink and this effect — and both
resolve the same engine-owned surface, so an edit in place would land in the
picture the passthrough camera is showing.
"""

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    ProcessorOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    output,
    processor,
)

# What a `VirtualCameraSink` samples on its way to the device's buffers.
INVERTED_FRAME_TEXTURE_FORMAT = "rgba8_unorm"
INVERTED_FRAME_TEXTURE_USAGE = ["texture_binding"]


@processor
class InvertingEffect:
    """Reads each camera frame and publishes its color-inverted twin."""

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    @output()
    def video_to_downstream(self) -> None: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.output_ring = ProcessorOutputTextureRing(
            INVERTED_FRAME_TEXTURE_FORMAT, INVERTED_FRAME_TEXTURE_USAGE
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None:
            return
        frame = VideoFrame.from_bag(bag)

        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as camera_surface:
            camera_surface.lock(read_only=True)
            try:
                # One bulk read out of the engine's mapping: it is
                # write-combined, and reading it through a strided view
                # re-reads that memory once per channel.
                inverted_pixels = camera_surface.as_numpy().copy()
            finally:
                camera_surface.unlock()

        # Color channels only — inverting alpha would erase the picture.
        inverted_pixels[:, :, :3] = 255 - inverted_pixels[:, :, :3]

        inverted_texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        # `unlock` in a `finally` rather than a `with`: the handle's context
        # manager closes the surface on the way out, and this slot has to
        # outlive the frame — the ring owns it for the processor's life.
        inverted_texture.lock(read_only=False)
        try:
            inverted_texture.as_numpy()[...] = inverted_pixels
        finally:
            inverted_texture.unlock()

        # The camera's bag forwarded whole, with only the surface swapped for
        # this effect's own. Everything else on it still describes this
        # picture: the capture stamp, which is what consumers order by, and the
        # colour metadata, from which a `VirtualCameraSink` sets the device's
        # colorimetry and picks its RGBA→YUYV matrix. Spelling four keys by
        # hand instead would leave the two cameras announcing different colour.
        inverted_bag = dict(bag)
        inverted_bag["surface_id"] = inverted_texture.surface_id
        # A per-frame layout override describes the surface it was published
        # with, and this is a different surface.
        inverted_bag.pop("texture_layout", None)
        ctx.outputs.write("video_to_downstream", inverted_bag)
