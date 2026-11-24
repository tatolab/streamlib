# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Texture Info hands on each picture's size, pixel format and stated frame rate as values,
writing an output again only when its value changes."""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

from tatolab.stream import Vector2
from tatolab.stream._class_node_construction import class_node_members, construct_class_node
from tatolab.stream.stream_facts import ImageStreamFacts, ImageStreamFrameRate, PacketsStreamFacts

from core_nodes.texture_info import TextureInfo

OUTPUTS = ("width", "height", "size", "pixel_format", "frame_rate")


class RecordingOutputPin:
    """An output pin that records each value written."""

    def __init__(self) -> None:
        self.writes: "list[Any]" = []

    def write(self, value: Any, timestamp_ns: "int | None" = None, **keywords: Any) -> None:
        del timestamp_ns, keywords
        self.writes.append(value)


class PictureInputPin:
    """An input pin holding the next frame a read gives and the facts its upstream states."""

    def __init__(self) -> None:
        self.next_frame: Any = None
        self.stream_facts: Any = None

    def read(self) -> Any:
        frame, self.next_frame = self.next_frame, None
        return frame

    def upstream_stream_facts(self, *, inbound_link: "str | None" = None) -> Any:
        del inbound_link
        return self.stream_facts


def a_frame(width: int, height: int, texture_format: str = "rgba8_unorm") -> Any:
    return SimpleNamespace(width=width, height=height, format=texture_format)


def a_texture_info() -> "tuple[Any, PictureInputPin, dict[str, RecordingOutputPin]]":
    picture = PictureInputPin()
    outputs = {name: RecordingOutputPin() for name in OUTPUTS}
    node: Any = construct_class_node(TextureInfo, field_values={}, pins={"picture": picture, **outputs})
    return node, picture, outputs


def the_writes(outputs: "dict[str, RecordingOutputPin]") -> "dict[str, list[Any]]":
    return {name: pin.writes for name, pin in outputs.items()}


def test_texture_info_takes_a_texture_and_gives_typed_values():
    declared = {member.port: member.pin_declaration for member in class_node_members(TextureInfo) or ()}
    assert declared["picture"]["pin_type"] == {"kind": "is", "pin_type": {"type": "texture"}}
    assert {name: declared[name]["pin_type"]["pin_type"] for name in OUTPUTS} == {
        "width": {"type": "integer"},
        "height": {"type": "integer"},
        "size": {"type": "vector", "component_count": "two"},
        "pixel_format": {"type": "string"},
        "frame_rate": {"type": "float"},
    }
    assert all(declared[name]["flow"] == "value" for name in OUTPUTS)


def test_the_first_picture_writes_its_size_format_and_stated_frame_rate():
    node, picture, outputs = a_texture_info()
    picture.next_frame = a_frame(1280, 720)
    picture.stream_facts = ImageStreamFacts(frame_rate=ImageStreamFrameRate(30_000, 1_001))
    node.process(None)
    assert the_writes(outputs) == {
        "width": [1280],
        "height": [720],
        "size": [Vector2(1280.0, 720.0)],
        "pixel_format": ["rgba8_unorm"],
        "frame_rate": [30_000 / 1_001],
    }


def test_a_picture_with_the_same_facts_writes_nothing_again():
    node, picture, outputs = a_texture_info()
    for _ in range(3):
        picture.next_frame = a_frame(640, 480)
        node.process(None)
    assert all(len(writes) <= 1 for writes in the_writes(outputs).values())


def test_a_change_of_size_writes_the_size_again_and_nothing_else():
    node, picture, outputs = a_texture_info()
    picture.stream_facts = ImageStreamFacts(frame_rate=ImageStreamFrameRate(60))
    picture.next_frame = a_frame(1920, 1080)
    node.process(None)
    picture.next_frame = a_frame(1280, 720)
    node.process(None)
    assert the_writes(outputs) == {
        "width": [1920, 1280],
        "height": [1080, 720],
        "size": [Vector2(1920.0, 1080.0), Vector2(1280.0, 720.0)],
        "pixel_format": ["rgba8_unorm"],
        "frame_rate": [60.0],
    }


def test_a_change_of_format_writes_only_the_format_again():
    node, picture, outputs = a_texture_info()
    picture.next_frame = a_frame(320, 240)
    node.process(None)
    picture.next_frame = a_frame(320, 240, "rgba16_float")
    node.process(None)
    assert outputs["pixel_format"].writes == ["rgba8_unorm", "rgba16_float"]
    assert outputs["width"].writes == [320]


def test_no_frame_rate_is_written_while_the_stream_states_none():
    node, picture, outputs = a_texture_info()
    for stream_facts in (None, ImageStreamFacts(), PacketsStreamFacts(rfc6381_codec="avc1.64001f")):
        picture.stream_facts = stream_facts
        picture.next_frame = a_frame(320, 240)
        node.process(None)
    assert outputs["frame_rate"].writes == []


def test_a_frame_rate_stated_later_is_written_when_it_arrives():
    node, picture, outputs = a_texture_info()
    picture.next_frame = a_frame(320, 240)
    node.process(None)
    picture.stream_facts = ImageStreamFacts(frame_rate=ImageStreamFrameRate(25))
    picture.next_frame = a_frame(320, 240)
    node.process(None)
    assert outputs["frame_rate"].writes == [25.0]


def test_a_look_with_no_picture_waiting_writes_nothing():
    node, _, outputs = a_texture_info()
    node.process(None)
    assert all(writes == [] for writes in the_writes(outputs).values())
