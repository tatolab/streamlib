# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`ImageFile`: a project image asset, published as an Image."""

from __future__ import annotations

from typing import Annotated, Any

from tatolab.stream import (
    Asset,
    AssetMedia,
    ColorInfo,
    FlowsAsValue,
    TextureToWrite,
    NodeOutputTextureRing,
    OutputPin,
    RuntimeContextLimitedAccess,
    Texture,
    TexturePixelFormats,
    node,
)

#: The texture format an `ImageFile` publishes: 8-bit RGBA, the decode of a PNG or a JPEG.
IMAGE_FILE_TEXTURE_FORMAT = "rgba8_unorm"

#: Two slots, so a frame published for a newly chosen asset never waits on a reader
#: still holding the one before it.
IMAGE_FILE_OUTPUT_RING_DEPTH = 2

#: How often an `ImageFile` looks at its `asset`: to publish one newly chosen, and to try
#: again one it could not read. It sends nothing between, since its output keeps its picture.
IMAGE_FILE_INTERVAL_BETWEEN_LOOKS_MS = 100

#: What an 8-bit PNG or JPEG decodes to: sRGB primaries and transfer, RGB, full range.
DECODED_IMAGE_FILE_COLOUR = ColorInfo(
    primaries="bt709", transfer="srgb", matrix="identity", range="full"
)


#: The Pillow modes whose channels an embedded colour profile of that mode reads directly.
MODES_AN_EMBEDDED_COLOUR_PROFILE_READS_AS_THEY_ARE = ("RGB", "CMYK", "L")


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
        upright = ImageOps.exif_transpose(image_file)
        embedded_colour_profile = image_file.info.get("icc_profile")
        if embedded_colour_profile:
            # A profile reads the channels of its own colour mode, so a CMYK
            # image is handed over as CMYK; a mode carrying alpha or a palette
            # goes through RGB with its alpha set aside.
            if upright.mode in MODES_AN_EMBEDDED_COLOUR_PROFILE_READS_AS_THEY_ARE:
                alpha, in_the_mode_of_the_profile = None, upright
            else:
                with_alpha = upright.convert("RGBA")
                alpha, in_the_mode_of_the_profile = (
                    with_alpha.getchannel("A"),
                    with_alpha.convert("RGB"),
                )
            in_srgb = ImageCms.profileToProfile(
                in_the_mode_of_the_profile,
                ImageCms.ImageCmsProfile(io.BytesIO(embedded_colour_profile)),
                ImageCms.createProfile("sRGB"),
                outputMode="RGB",
            )
            assert in_srgb is not None
            if alpha is not None:
                in_srgb.putalpha(alpha)
            upright = in_srgb
        upright = upright.convert("RGBA")
        return numpy.asarray(upright)


@node.meta(
    category="Sources",
    description="Show a picture from the project's assets. Pick a PNG or JPEG you imported in its `asset` input.",
)
@node(execution="continuous", interval_ms=IMAGE_FILE_INTERVAL_BETWEEN_LOOKS_MS)
class ImageFile:
    """Publishes the project image asset its `asset` names, once for each asset chosen.

    Its output flows as a value, so the runtime sends the picture again to a reader linked
    after it was published, with no decode and no upload. While no asset is chosen, or the one
    chosen cannot be read, the node reports itself not ready, saying why, and tries the publish
    again at each look until one succeeds.
    """

    asset: Annotated[Asset, AssetMedia("image")] | None = None
    image: OutputPin[
        Annotated[Texture, TexturePixelFormats(IMAGE_FILE_TEXTURE_FORMAT), FlowsAsValue()]
    ]

    def __init__(self) -> None:
        self._output_texture_ring = NodeOutputTextureRing(
            IMAGE_FILE_TEXTURE_FORMAT, ["texture_binding"], depth=IMAGE_FILE_OUTPUT_RING_DEPTH
        )
        self._asset_published: "Asset | None" = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        asset = self.asset
        if asset is None:
            self._asset_published = None
            ctx.report_not_ready(
                "ImageFile names no image asset. To fix: set its `asset` input to a PNG or JPEG "
                "imported into the project"
            )
            return
        if asset == self._asset_published:
            return
        try:
            self._publish(asset, ctx)
        except Exception as failure:
            ctx.report_not_ready(f"ImageFile cannot read its image asset: {failure}")
            return
        ctx.report_ready()
        self._asset_published = asset

    def _publish(self, asset: Asset, ctx: RuntimeContextLimitedAccess) -> None:
        pixels = decoded_rgba_pixels_of_the_image_file(
            ctx.asset_content_path(asset.asset, asset.from_project)
        )
        height, width = pixels.shape[0], pixels.shape[1]
        slot = self._output_texture_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, width, height
        )
        slot.lock(read_only=False)
        try:
            slot.as_numpy()[:height, :width] = pixels
        finally:
            slot.unlock()
        self.image.write(TextureToWrite.of_texture(slot, DECODED_IMAGE_FILE_COLOUR))
