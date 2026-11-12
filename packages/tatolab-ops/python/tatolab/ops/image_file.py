# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`ImageFile`, the one class node of `tatolab-ops`: a project image asset, published as an Image."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, Any

from tatolab.stream import (
    ColorInfo,
    Image,
    ImageFrameToWrite,
    NodeOutputTextureRing,
    Output,
    ProjectAssetUuid,
    node,
)

#: The texture format an `ImageFile` publishes: 8-bit RGBA, the decode of a PNG or a JPEG.
IMAGE_FILE_TEXTURE_FORMAT = "rgba8_unorm"

#: Two slots, so a frame published on a config change never waits on a reader
#: still holding the one before it.
IMAGE_FILE_OUTPUT_RING_DEPTH = 2

#: What an 8-bit PNG or JPEG decodes to: sRGB primaries and transfer, RGB, full range.
DECODED_IMAGE_FILE_COLOUR = ColorInfo(
    primaries="bt709", transfer="srgb", matrix="identity", range="full"
)


@dataclass(frozen=True)
class ImageFileConfig:
    """Which project image asset an `ImageFile` publishes, by its UUID."""

    asset: Annotated[str, ProjectAssetUuid(media=("image",))] = ""


def decoded_rgba_pixels_of_the_image_file(path: str) -> Any:
    """The PNG or JPEG at `path`, upright by its EXIF orientation and in sRGB by its embedded colour profile, decoded to an `(height, width, 4)` array of 8-bit RGBA."""
    import io

    import numpy
    from PIL import Image as PillowImage
    from PIL import ImageCms, ImageOps

    with PillowImage.open(path) as image_file:
        if image_file.format not in ("PNG", "JPEG"):
            raise ValueError(
                f"the image asset at {path} is {image_file.format or 'no format Pillow knows'}, "
                f"and an ImageFile decodes PNG and JPEG"
            )
        upright = ImageOps.exif_transpose(image_file).convert("RGBA")
        embedded_colour_profile = image_file.info.get("icc_profile")
        if embedded_colour_profile:
            alpha = upright.getchannel("A")
            in_srgb = ImageCms.profileToProfile(
                upright.convert("RGB"),
                ImageCms.ImageCmsProfile(io.BytesIO(embedded_colour_profile)),
                ImageCms.createProfile("sRGB"),
                outputMode="RGB",
            )
            assert in_srgb is not None
            in_srgb.putalpha(alpha)
            upright = in_srgb
        return numpy.asarray(upright)


@node.process(execution="continuous", interval_ms=100)
class ImageFile:
    """Publishes the project image asset its config names, once at start and again on each config change.

    A reader that connects after a publish is sent the same picture again,
    with no decode and no upload. A publish that fails is tried again at each
    process until one succeeds, each failure raised once until it changes.
    """

    image = Output(Image, formats=(IMAGE_FILE_TEXTURE_FORMAT,))

    def __init__(self, config: ImageFileConfig) -> None:
        self._asset = config.asset
        self._output_texture_ring = NodeOutputTextureRing(
            IMAGE_FILE_TEXTURE_FORMAT, ["texture_binding"], depth=IMAGE_FILE_OUTPUT_RING_DEPTH
        )
        self._publish_on_the_next_process = True
        self._failure_of_the_last_publish: str | None = None
        self._frame_of_the_last_publish: Any = None

    def configure(self, config: ImageFileConfig) -> None:
        """Publish the asset `config` names on the next process."""
        self._asset = config.asset
        self._publish_on_the_next_process = True
        self._failure_of_the_last_publish = None

    def process(self, ctx) -> None:
        if not self._publish_on_the_next_process:
            self._resend_the_last_publish_to_a_reader_it_missed(ctx)
            return
        try:
            self._publish_the_asset(ctx)
        except Exception as failure:
            failure_named = f"{type(failure).__name__}: {failure}"
            if failure_named == self._failure_of_the_last_publish:
                return
            self._failure_of_the_last_publish = failure_named
            raise
        self._publish_on_the_next_process = False
        self._failure_of_the_last_publish = None

    def _publish_the_asset(self, ctx) -> None:
        if not self._asset:
            raise ValueError(
                "ImageFile names no image asset. To fix: set its `asset` config to the UUID of "
                "a PNG or JPEG imported into the project"
            )
        pixels = decoded_rgba_pixels_of_the_image_file(ctx.asset_content_path(self._asset))
        height, width = pixels.shape[0], pixels.shape[1]
        slot = self._output_texture_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, width, height
        )
        slot.lock(read_only=False)
        try:
            slot.as_numpy()[:height, :width] = pixels
        finally:
            slot.unlock()
        frame = ImageFrameToWrite.of_texture(slot, DECODED_IMAGE_FILE_COLOUR)
        ctx.outputs.write(self.image, frame)
        self._frame_of_the_last_publish = frame

    def _resend_the_last_publish_to_a_reader_it_missed(self, ctx) -> None:
        if self._frame_of_the_last_publish is None:
            return
        if ctx.outputs.has_a_reader_the_last_write_missed(self.image):
            ctx.outputs.write(self.image, self._frame_of_the_last_publish)
