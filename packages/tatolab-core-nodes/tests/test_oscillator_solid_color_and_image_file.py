# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Oscillator, Solid Color and Image File are each written as one function, its body run with its inputs and pins.

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

from core_nodes import image_file as image_file_module
from core_nodes import solid_color as solid_color_module
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


class HeldInput:
    """A value input holding the value it was last set to, read with `get()` as a node's is."""

    def __init__(self, value: Any) -> None:
        self.value = value

    def get(self) -> Any:
        return self.value


def the_pin_declaration_of(node: Any, port: str) -> "dict[str, Any]":
    return next(member.pin_declaration for member in node.members if member.port == port)


def the_body_of(node: Any, values: "dict[str, Any]", pins: "dict[str, Any]") -> "tuple[Any, dict[str, HeldInput]]":
    """The per-frame function `node`'s body returns, each value input holding its value in
    `values` or its default, and each of its held inputs by its port."""
    inputs = {
        member.port: HeldInput(values.get(member.port, member.default))
        for member in node.members
        if member.form == "field"
    }
    arguments = {
        member.attribute: inputs[member.port] if member.form == "field" else pins[member.port]
        for member in node.members
    }
    return node.__wrapped__(**arguments), inputs


class OscillatorClock:
    def __init__(self) -> None:
        self.time = 0


def test_an_oscillator_declares_its_pitch_and_its_shape_as_live_value_inputs():
    frequency = the_pin_declaration_of(Oscillator, "frequency")
    assert frequency["flow"] == "value"
    assert frequency["former_names"] == ["frequency_hz"]
    assert frequency["annotations"]["unit"] == "Hz"
    for value_input in ("sample_rate", "channels", "block_size"):
        member = next(member for member in Oscillator.members if member.port == value_input)
        assert member.form == "field" and not member.a_setting, value_input
    assert the_pin_declaration_of(Oscillator, "audio")["pin_type"] == {
        "kind": "is",
        "pin_type": {"type": "audio"},
    }


def test_an_oscillators_pitch_changes_from_the_next_block_without_a_click():
    audio = RecordingOutputPin("audio")
    each_frame, inputs = the_body_of(
        Oscillator,
        {"frequency": 480.0, "sample_rate": 48_000, "channels": 1, "block_size": 100},
        {"audio": audio},
    )
    clock = OscillatorClock()
    each_frame(clock)
    clock.time = 100 * 1_000_000_000 // 48_000 + 1
    each_frame(clock)
    inputs["frequency"].value = 960.0
    clock.time = 200 * 1_000_000_000 // 48_000 + 1
    each_frame(clock)

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
    each_frame, inputs = the_body_of(
        Oscillator,
        {"frequency": 480.0, "sample_rate": 48_000, "channels": 1, "block_size": 100},
        {"audio": audio},
    )
    clock = OscillatorClock()
    each_frame(clock)
    clock.time = 1_000 * 1_000_000_000 // 48_000 + 1
    each_frame(clock)
    assert len(audio.writes) == 10

    inputs["sample_rate"].value = 24_000
    each_frame(clock)
    clock.time += 100 * 1_000_000_000 // 24_000 + 1
    each_frame(clock)

    assert len(audio.writes) == 11
    assert audio.writes[-1][2]["sample_rate"] == 24_000
    assert audio.writes[-1][2]["start_sample"] == 1_000


class SolidColorContext:
    """A context the picture is filled through, its GPU left to the stand-in fill."""

    gpu_limited_access = None


def test_a_solid_color_fills_its_picture_again_only_when_its_colour_or_size_changes(monkeypatch):
    filled: "list[tuple[int, int, Color]]" = []

    def filled_frame(
        _ring: Any, _ctx: Any, width: int, height: int, color: Color
    ) -> "tuple[int, int, Color]":
        frame = (width, height, color)
        filled.append(frame)
        return frame

    monkeypatch.setattr(solid_color_module, "filled_frame_of", filled_frame)
    monkeypatch.setattr(solid_color_module, "NodeOutputTextureRing", lambda *_a, **_k: None)
    image = RecordingOutputPin("image")
    each_frame, inputs = the_body_of(SolidColor, {"width": 4, "height": 2}, {"image": image})
    context = SolidColorContext()

    each_frame(context)
    each_frame(context)
    inputs["color"].value = Color(1.0, 0.0, 0.0)
    each_frame(context)

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


def an_image_file_publishing_through(
    monkeypatch, image: RecordingOutputPin
) -> "tuple[Any, dict[str, HeldInput]]":
    monkeypatch.setattr(
        image_file_module,
        "picture_of_the_image_asset",
        lambda _ring, asset, ctx: ctx.asset_content_path(asset.asset, asset.from_project),
    )
    monkeypatch.setattr(image_file_module, "NodeOutputTextureRing", lambda *_a, **_k: None)
    return the_body_of(ImageFile, {}, {"image": image})


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
    each_frame, _ = an_image_file_publishing_through(monkeypatch, RecordingOutputPin("image"))
    context = ImageFileContext()

    each_frame(context)

    assert context.not_ready_because is not None
    assert "set its `asset` input" in context.not_ready_because
    assert context.paths_asked_for == []


def test_an_image_file_tries_an_unreadable_asset_at_each_look_and_publishes_each_asset_once(
    monkeypatch,
):
    image = RecordingOutputPin("image")
    each_frame, inputs = an_image_file_publishing_through(monkeypatch, image)
    context = ImageFileContext()
    inputs["asset"].value = Asset(AN_ASSET)

    each_frame(context)
    each_frame(context)
    assert context.not_ready_because == (
        f"ImageFile cannot read its image asset: asset {AN_ASSET} is not readable yet"
    )
    assert len(context.paths_asked_for) == 2, "the publish is tried again at the next look"

    context.readable = True
    each_frame(context)
    each_frame(context)
    assert context.not_ready_because is None
    assert [write[0] for write in image.writes] == [f"/assets/{AN_ASSET}.png"]

    inputs["asset"].value = Asset(ANOTHER_ASSET, from_project=AN_ASSET)
    each_frame(context)
    assert context.paths_asked_for[-1] == (ANOTHER_ASSET, AN_ASSET)
    assert len(image.writes) == 2
