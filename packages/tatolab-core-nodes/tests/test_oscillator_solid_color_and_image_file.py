# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Oscillator, Solid Color and Image File are class nodes of fields and pins, built as their helper builds them.

Oscillator's pitch follows its field from the next block on. Solid Color fills its picture
again only when its colour or size changes. Image File publishes each asset chosen once, on an
output that keeps its picture for a reader linked late, and reports itself not ready, trying
again at each look, while it has no asset or cannot read the one chosen.
"""

from __future__ import annotations

from typing import Any

import numpy
import pytest
from tatolab.stream import Asset, Color
from tatolab.stream._class_node_construction import class_node_members, construct_class_node

from core_nodes.image_file import ImageFile
from core_nodes.oscillator import Oscillator
from core_nodes.solid_color import SolidColor

AN_ASSET = "0f0e0d0c-0b0a-4909-8807-060504030201"
ANOTHER_ASSET = "1f0e0d0c-0b0a-4909-8807-060504030201"


class RecordingOutputPin:
    """An output pin that records each write."""

    def __init__(self, port: str) -> None:
        self.port = port
        self.writes: "list[tuple[Any, ...]]" = []

    def write(self, value: Any, timestamp_ns: "int | None" = None, **keywords: Any) -> None:
        self.writes.append((value, timestamp_ns, keywords))


def the_pin_declaration_of(node_class: type, port: str) -> "dict[str, Any]":
    members = class_node_members(node_class)
    assert members is not None
    return next(member.pin_declaration for member in members if member.port == port)


class OscillatorClock:
    def __init__(self) -> None:
        self.time = 0


def test_an_oscillator_declares_its_pitch_as_a_wired_value_and_its_shape_as_settings():
    frequency = the_pin_declaration_of(Oscillator, "frequency")
    assert frequency["flow"] == "value"
    assert frequency["former_names"] == ["frequency_hz"]
    assert frequency["annotations"]["unit"] == "Hz"
    for setting in ("sample_rate", "channels", "block_size"):
        member = next(
            member for member in class_node_members(Oscillator) or () if member.port == setting
        )
        assert member.a_setting, setting
    assert the_pin_declaration_of(Oscillator, "audio")["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "audio"},
    }


def test_an_oscillators_pitch_changes_from_the_next_block_without_a_click():
    audio = RecordingOutputPin("audio")
    oscillator: Any = construct_class_node(
        Oscillator,
        field_values={"frequency": 480.0, "sample_rate": 48_000, "channels": 1, "block_size": 100},
        pins={"audio": audio},
    )
    clock = OscillatorClock()
    oscillator.process(clock)
    clock.time = 100 * 1_000_000_000 // 48_000 + 1
    oscillator.process(clock)
    oscillator.frequency = 960.0
    clock.time = 200 * 1_000_000_000 // 48_000 + 1
    oscillator.process(clock)

    assert len(audio.writes) == 2
    first, second = (numpy.asarray(write[0])[0] for write in audio.writes)
    assert [write[2]["start_sample"] for write in audio.writes] == [0, 100]
    assert all(write[2]["sample_rate"] == 48_000 for write in audio.writes)
    # 480 Hz at 48 kHz is a hundredth of a cycle per sample, so the first block is one whole
    # cycle; 960 Hz steps twice as far, from the phase the first block ended on.
    assert first[25] == pytest.approx(0.2, abs=1e-6)
    assert second[0] == pytest.approx(0.0, abs=1e-6)
    assert second[12] == pytest.approx(0.2 * numpy.sin(2 * numpy.pi * 0.24), abs=1e-6)


def test_an_oscillators_rate_set_while_it_plays_starts_a_run_at_that_rate():
    """Mental-revert: count the samples owed since the anchor at the new rate, and halving the
    rate after a block owes none until the old count is reached, so the tone falls silent."""
    audio = RecordingOutputPin("audio")
    oscillator: Any = construct_class_node(
        Oscillator,
        field_values={"frequency": 480.0, "sample_rate": 48_000, "channels": 1, "block_size": 100},
        pins={"audio": audio},
    )
    clock = OscillatorClock()
    oscillator.process(clock)
    clock.time = 1_000 * 1_000_000_000 // 48_000 + 1
    oscillator.process(clock)
    assert len(audio.writes) == 10

    oscillator.sample_rate = 24_000
    oscillator.process(clock)
    clock.time += 100 * 1_000_000_000 // 24_000 + 1
    oscillator.process(clock)

    assert len(audio.writes) == 11
    assert audio.writes[-1][2]["sample_rate"] == 24_000
    assert audio.writes[-1][2]["start_sample"] == 1_000


class SolidColorContext:
    """A context the picture is filled through, its GPU left to the stand-in fill."""

    gpu_limited_access = None


def test_a_solid_color_fills_its_picture_again_only_when_its_colour_or_size_changes(monkeypatch):
    filled: "list[tuple[int, int, Color]]" = []

    def filled_frame(self: Any, ctx: Any) -> "tuple[int, int, Color]":
        frame = (self.width, self.height, self.color)
        filled.append(frame)
        return frame

    monkeypatch.setattr(SolidColor, "_filled_frame", filled_frame)
    monkeypatch.setattr("core_nodes.solid_color.NodeOutputTextureRing", lambda *_a, **_k: None)
    image = RecordingOutputPin("image")
    solid_color: Any = construct_class_node(
        SolidColor, field_values={"width": 4, "height": 2}, pins={"image": image}
    )
    context = SolidColorContext()

    solid_color.process(context)
    solid_color.process(context)
    solid_color.color = Color(1.0, 0.0, 0.0)
    solid_color.process(context)

    assert filled == [(4, 2, Color(0.0, 0.0, 0.0, 1.0)), (4, 2, Color(1.0, 0.0, 0.0))]
    assert [write[0] for write in image.writes] == [filled[0], filled[0], filled[1]]
    assert the_pin_declaration_of(SolidColor, "image")["pin_type"]["pin_type"] == {
        "type": "texture",
        "pixel_formats": ["rgba8_unorm"],
    }
    assert the_pin_declaration_of(SolidColor, "color")["pin_type"]["pin_type"] == {"type": "color"}


class ImageFileContext:
    """A node context whose `asset_content_path` raises until `readable`, recording the reason
    the node last reported itself not ready, or `None` while it is ready."""

    gpu_limited_access = None

    def __init__(self) -> None:
        self.not_ready_because: "str | None" = "not reported yet"
        self.paths_asked_for: "list[tuple[str, str | None]]" = []
        self.readable = False

    def report_not_ready(self, reason: str, **_placeholder_image_size: int) -> None:
        self.not_ready_because = reason

    def report_ready(self) -> None:
        self.not_ready_because = None

    def asset_content_path(self, asset: str, from_project: "str | None" = None) -> str:
        self.paths_asked_for.append((asset, from_project))
        if not self.readable:
            raise LookupError(f"asset {asset} is not readable yet")
        return f"/assets/{asset}.png"


def an_image_file_publishing_through(monkeypatch, image: RecordingOutputPin) -> Any:
    monkeypatch.setattr(
        ImageFile,
        "_publish",
        lambda self, asset, ctx: self.image.write(
            ctx.asset_content_path(asset.asset, asset.from_project)
        ),
    )
    monkeypatch.setattr("core_nodes.image_file.NodeOutputTextureRing", lambda *_a, **_k: None)
    return construct_class_node(ImageFile, field_values={}, pins={"image": image})


def test_an_image_file_states_one_format_and_keeps_its_picture_for_a_reader_linked_late():
    assert the_pin_declaration_of(ImageFile, "image") == {
        "pin_type": {"kind": "is", "pin_type": {"type": "texture", "pixel_formats": ["rgba8_unorm"]}},
        "flow": "value",
    }
    assert the_pin_declaration_of(ImageFile, "asset") == {
        "pin_type": {"kind": "is", "pin_type": {"type": "asset", "media": ["image"]}},
        "flow": "value",
        "may_be_unset": True,
    }


def test_an_image_file_naming_no_asset_reports_itself_not_ready_naming_its_input(monkeypatch):
    image_file = an_image_file_publishing_through(monkeypatch, RecordingOutputPin("image"))
    context = ImageFileContext()

    image_file.process(context)

    assert context.not_ready_because is not None
    assert "set its `asset` input" in context.not_ready_because
    assert context.paths_asked_for == []


def test_an_image_file_tries_an_unreadable_asset_at_each_look_and_publishes_each_asset_once(
    monkeypatch,
):
    image = RecordingOutputPin("image")
    image_file = an_image_file_publishing_through(monkeypatch, image)
    context = ImageFileContext()
    image_file.asset = Asset(AN_ASSET)

    image_file.process(context)
    image_file.process(context)
    assert context.not_ready_because == (
        f"ImageFile cannot read its image asset: asset {AN_ASSET} is not readable yet"
    )
    assert len(context.paths_asked_for) == 2, "the publish is tried again at the next look"

    context.readable = True
    image_file.process(context)
    image_file.process(context)
    assert context.not_ready_because is None
    assert [write[0] for write in image.writes] == [f"/assets/{AN_ASSET}.png"]

    image_file.asset = Asset(ANOTHER_ASSET, from_project=AN_ASSET)
    image_file.process(context)
    assert context.paths_asked_for[-1] == (ANOTHER_ASSET, AN_ASSET)
    assert len(image.writes) == 2
