# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class node's `InputPin` of several types takes each of them, each narrowed where it is written, and reads them as one stream."""

from __future__ import annotations

from typing import Annotated, Any

import pytest

from tatolab.stream import (
    Audio,
    Description,
    EncodedAudio,
    EncodedCodecs,
    EncodedVideo,
    InputPin,
    NodeDeclarationRefusal,
    OutputPin,
    StreamDelivery,
    Texture,
    Trigger,
    node,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_INPUT_PORTS_STAMP


@node
class Publisher:
    tracks: InputPin[
        Annotated[
            Annotated[EncodedVideo, EncodedCodecs("h264")] | Annotated[EncodedAudio, EncodedCodecs("opus")],
            StreamDelivery("ordered"),
            Description("Video or audio, one track per link"),
        ]
    ]
    pictures_or_sound: InputPin[Texture | Audio | None]

    def process(self, ctx: Any) -> None:
        del ctx


def the_port(port_name: str) -> "dict[str, Any]":
    (port,) = [port for port in getattr(Publisher, NODE_DECLARATION_INPUT_PORTS_STAMP) if port["name"] == port_name]
    return port


def test_each_type_is_taken_narrowed_as_it_is_written_and_streams():
    assert the_port("tracks") == {
        "name": "tracks",
        "description": "Video or audio, one track per link",
        "family": {"family": "packets"},
        "pin_declaration": {
            "pin_type": {
                "kind": "one_of",
                "pin_types": [
                    {"type": "encoded_video", "codecs": ["h264"]},
                    {"type": "encoded_audio", "codecs": ["opus"]},
                ],
            },
            "flow": "stream",
        },
        "delivery_profile": "ordered",
    }


def test_types_travelling_on_several_families_read_each_and_may_be_unset():
    port = the_port("pictures_or_sound")
    assert port["family"] == {"family": "image", "also_takes_families": ["channels"]}
    assert port["pin_declaration"] == {
        "pin_type": {"kind": "one_of", "pin_types": [{"type": "texture"}, {"type": "audio"}]},
        "flow": "stream",
        "may_be_unset": True,
    }


def _a_trigger_beside_a_stream() -> None:
    @node
    class Mixed:
        either: InputPin[Trigger | Texture]

        def process(self, ctx: Any) -> None:
            del ctx


def _a_narrowing_of_another_type() -> None:
    @node
    class Misnarrowed:
        tracks: InputPin[Annotated[Texture, EncodedCodecs("h264")] | Audio]

        def process(self, ctx: Any) -> None:
            del ctx


def _an_output_of_several_types() -> None:
    @node
    class Gives:
        out: OutputPin[Texture | Audio]

        def process(self, ctx: Any) -> None:
            del ctx


def _a_field_of_several_types() -> None:
    @node
    class Holds:
        level: float | int = 0.0

        def process(self, ctx: Any) -> None:
            del ctx


@pytest.mark.parametrize(
    ("declare", "finding_code"),
    [
        (_a_trigger_beside_a_stream, "pin_flow_not_taken"),
        (_a_narrowing_of_another_type, "pin_annotation_not_taken"),
        (_an_output_of_several_types, "pin_type_not_read_yet"),
        (_a_field_of_several_types, "pin_type_not_read_yet"),
    ],
)
def test_a_pin_of_several_types_is_refused_unless_it_is_an_input_pin_of_one_flow(
    declare: Any, finding_code: str
):
    with pytest.raises(NodeDeclarationRefusal) as refused:
        declare()
    assert refused.value.finding_code == finding_code, refused.value
