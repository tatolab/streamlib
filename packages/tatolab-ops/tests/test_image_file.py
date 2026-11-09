# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`ImageFile` decodes a PNG or a JPEG asset to RGBA, refuses anything else by name, and declares one Image output."""

from __future__ import annotations

from pathlib import Path

import numpy
import pytest
from PIL import Image as PillowImage

from tatolab.ops import ImageFile, ImageFileConfig
from tatolab.ops.image_file import decoded_rgba_pixels_of_the_image_file


def an_image_file_of(tmp_path: Path, file_format: str, mode: str) -> Path:
    path = tmp_path / f"asset.{file_format.lower()}"
    PillowImage.new(mode, (3, 2), (200, 100, 50) if mode == "RGB" else (200, 100, 50, 128)).save(
        path, format=file_format
    )
    return path


def test_a_png_decodes_to_rgba_keeping_its_alpha(tmp_path: Path):
    pixels = decoded_rgba_pixels_of_the_image_file(str(an_image_file_of(tmp_path, "PNG", "RGBA")))
    assert pixels.shape == (2, 3, 4)
    assert pixels.dtype == numpy.uint8
    assert tuple(pixels[1, 2]) == (200, 100, 50, 128)


def test_a_jpeg_decodes_to_rgba_with_opaque_alpha(tmp_path: Path):
    pixels = decoded_rgba_pixels_of_the_image_file(str(an_image_file_of(tmp_path, "JPEG", "RGB")))
    assert pixels.shape == (2, 3, 4)
    assert (pixels[..., 3] == 255).all()


def test_an_image_of_another_format_is_refused_by_name(tmp_path: Path):
    with pytest.raises(ValueError, match="is GIF, and an ImageFile decodes PNG and JPEG"):
        decoded_rgba_pixels_of_the_image_file(str(an_image_file_of(tmp_path, "GIF", "RGB")))


def test_image_file_is_a_class_node_with_one_image_output_configured_by_asset():
    (output_port,) = getattr(ImageFile, "__tatolab_node_output_ports__")
    assert output_port["name"] == "image"
    assert output_port["family"]["family"] == "image"
    assert getattr(ImageFile, "__tatolab_node_input_ports__") == []
    assert ImageFileConfig().asset == ""


def test_a_jpeg_is_decoded_upright_by_its_exif_orientation(tmp_path: Path):
    path = tmp_path / "rotated.jpeg"
    exif = PillowImage.Exif()
    exif[0x0112] = 6
    PillowImage.new("RGB", (3, 2), (200, 100, 50)).save(path, format="JPEG", exif=exif)
    pixels = decoded_rgba_pixels_of_the_image_file(str(path))
    assert pixels.shape == (3, 2, 4), "orientation 6 turns a 3x2 image a quarter turn"


class _AssetContentPathThatFails:
    """A node context whose `asset_content_path` raises, standing in for an asset not yet readable."""

    gpu_limited_access = None

    def __init__(self) -> None:
        self.asset_content_path_calls = 0

    def asset_content_path(self, asset: str) -> str:
        self.asset_content_path_calls += 1
        raise RuntimeError(f"asset {asset} is not readable yet")


def test_a_failed_publish_is_tried_again_and_raised_once_until_its_failure_changes():
    image_file = ImageFile(ImageFileConfig(asset="0f0e"))
    context = _AssetContentPathThatFails()
    with pytest.raises(RuntimeError, match="not readable yet"):
        image_file.process(context)
    image_file.process(context)
    assert context.asset_content_path_calls == 2, "the publish stays pending after a failure"

    image_file.configure(ImageFileConfig(asset="0f0e"))
    with pytest.raises(RuntimeError, match="not readable yet"):
        image_file.process(context)
