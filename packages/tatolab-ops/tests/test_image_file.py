# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`ImageFile` decodes a PNG or a JPEG asset to RGBA, refuses anything else by name, declares one Image output, publishes on start and on each config change, and resends to a reader that connects later."""

from __future__ import annotations

from pathlib import Path
from typing import Any

import numpy
import pytest
from PIL import Image as PillowImage
from PIL import ImageCms

from tatolab.ops import ImageFile, ImageFileConfig
from tatolab.ops.image_file import IMAGE_FILE_TEXTURE_FORMAT, decoded_rgba_pixels_of_the_image_file
from tatolab.stream import ImageFrameToWrite


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


def test_image_file_config_schema_marks_its_asset_key_as_a_project_image_asset():
    asset_key_schema = getattr(ImageFile, "__tatolab_node_config_schema__")["properties"]["asset"]
    assert asset_key_schema == {
        "type": "string",
        "default": "",
        "x-tatolab-project-asset": {"media": ["image"]},
    }


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


def test_image_file_declares_the_one_texture_format_it_publishes():
    (output_port,) = getattr(ImageFile, "__tatolab_node_output_ports__")
    assert output_port["family"]["formats"] == [IMAGE_FILE_TEXTURE_FORMAT]


def test_a_png_tagged_srgb_keeps_its_pixels_and_alpha_through_its_colour_profile(tmp_path: Path):
    path = tmp_path / "tagged.png"
    srgb_profile = ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB")).tobytes()
    PillowImage.new("RGBA", (3, 2), (200, 100, 50, 128)).save(
        path, format="PNG", icc_profile=srgb_profile
    )
    pixels = decoded_rgba_pixels_of_the_image_file(str(path))
    assert pixels.shape == (2, 3, 4)
    assert all(abs(int(channel) - expected) <= 1 for channel, expected in zip(pixels[1, 2][:3], (200, 100, 50)))
    assert int(pixels[1, 2][3]) == 128


class _RingSlotStandIn:
    """A texture ring slot whose pixels are a NumPy array."""

    def __init__(self, width: int, height: int) -> None:
        self.pixels = numpy.zeros((height, width, 4), dtype=numpy.uint8)
        self.locked = False

    def lock(self, read_only: bool) -> None:
        self.locked = True

    def unlock(self) -> None:
        self.locked = False

    def as_numpy(self) -> Any:
        return self.pixels


class _TextureRingStandIn:
    def __init__(self) -> None:
        self.slots: "list[_RingSlotStandIn]" = []

    def next_texture_for_this_frame(self, gpu_limited_access: Any, width: int, height: int) -> _RingSlotStandIn:
        slot = _RingSlotStandIn(width, height)
        self.slots.append(slot)
        return slot


class _OutputsStandIn:
    """Output ports whose readers the last write missed are set by the test."""

    def __init__(self) -> None:
        self.written: "list[tuple[Any, Any]]" = []
        self.a_reader_the_last_write_missed = False

    def write(self, port: Any, frame: Any) -> None:
        self.written.append((port, frame))
        self.a_reader_the_last_write_missed = False

    def has_a_reader_the_last_write_missed(self, port: Any) -> bool:
        return self.a_reader_the_last_write_missed


class _ContextReadingAssetsFrom:
    """A node context whose assets are files in a folder, by UUID."""

    gpu_limited_access = None

    def __init__(self, path_by_asset: "dict[str, Path]") -> None:
        self.path_by_asset = path_by_asset
        self.outputs = _OutputsStandIn()

    def asset_content_path(self, asset: str) -> str:
        return str(self.path_by_asset[asset])


def test_image_file_publishes_its_asset_once_and_again_after_a_config_change(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    frames_of_texture: "list[_RingSlotStandIn]" = []

    def a_frame_of_the_texture(slot: _RingSlotStandIn, colour: Any) -> _RingSlotStandIn:
        frames_of_texture.append(slot)
        return slot

    monkeypatch.setattr(ImageFrameToWrite, "of_texture", staticmethod(a_frame_of_the_texture))
    first = an_image_file_of(tmp_path, "PNG", "RGBA")
    second = tmp_path / "second.png"
    PillowImage.new("RGBA", (2, 2), (10, 20, 30, 255)).save(second, format="PNG")
    context = _ContextReadingAssetsFrom({"first": first, "second": second})
    image_file = ImageFile(ImageFileConfig(asset="first"))
    texture_ring = _TextureRingStandIn()
    setattr(image_file, "_output_texture_ring", texture_ring)

    image_file.process(context)
    image_file.process(context)
    assert len(context.outputs.written) == 1, "an idle process publishes nothing"
    assert tuple(texture_ring.slots[0].pixels[1, 2]) == (200, 100, 50, 128)
    assert not texture_ring.slots[0].locked

    image_file.configure(ImageFileConfig(asset="second"))
    image_file.process(context)
    assert len(context.outputs.written) == 2
    assert tuple(texture_ring.slots[1].pixels[0, 0]) == (10, 20, 30, 255)
    assert frames_of_texture == texture_ring.slots


def test_image_file_resends_its_last_picture_to_a_reader_that_connects_later(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr(
        ImageFrameToWrite, "of_texture", staticmethod(lambda slot, colour: slot)
    )
    context = _ContextReadingAssetsFrom({"first": an_image_file_of(tmp_path, "PNG", "RGBA")})
    image_file = ImageFile(ImageFileConfig(asset="first"))
    texture_ring = _TextureRingStandIn()
    setattr(image_file, "_output_texture_ring", texture_ring)

    image_file.process(context)
    image_file.process(context)
    assert len(context.outputs.written) == 1, "no reader joined, so nothing is resent"

    context.outputs.a_reader_the_last_write_missed = True
    image_file.process(context)
    assert len(context.outputs.written) == 2
    assert context.outputs.written[1] == context.outputs.written[0], "the same picture again"
    assert len(texture_ring.slots) == 1, "a resend decodes and uploads nothing"

    image_file.process(context)
    assert len(context.outputs.written) == 2


def test_image_file_resends_nothing_before_its_first_publish_succeeds():
    image_file = ImageFile(ImageFileConfig(asset="0f0e"))
    context = _AssetContentPathThatFails()
    with pytest.raises(RuntimeError, match="not readable yet"):
        image_file.process(context)
    image_file.process(context)
    assert context.asset_content_path_calls == 2, "the publish stays pending; no resend is tried"
