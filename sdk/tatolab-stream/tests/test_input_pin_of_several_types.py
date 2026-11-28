# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node function's `InputPin` of several types takes each of them, each narrowed where it is written, and reads them as one stream."""

from __future__ import annotations

from typing import TYPE_CHECKING, Annotated, Any, assert_type

import pytest

from tatolab.stream import (
    Audio,
    Description,
    EncodedAudio,
    EncodedCodecs,
    EncodedVideo,
    Input,
    InputPin,
    NodeDeclarationRefusal,
    OutputPin,
    StreamDelivery,
    Texture,
    Trigger,
    node,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_INPUT_PORTS_STAMP


@node(execution="reactive")
def Publisher(
    *,
    tracks: InputPin[
        Annotated[
            Annotated[EncodedVideo, EncodedCodecs("h264")] | Annotated[EncodedAudio, EncodedCodecs("opus")],
            StreamDelivery("ordered"),
            Description("Video or audio, one track per link"),
        ]
    ],
    pictures_or_sound: InputPin[Texture | Audio | None],
) -> None:
    return None


def the_port(port_name: str) -> "dict[str, Any]":
    (port,) = [port for port in getattr(Publisher, NODE_DECLARATION_INPUT_PORTS_STAMP) if port["name"] == port_name]
    return port


def test_each_type_is_taken_narrowed_as_it_is_written_and_streams():
    assert the_port("tracks") == {
        "name": "tracks",
        "description": "Video or audio, one track per link",
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


def test_types_travelling_as_several_kinds_of_message_are_each_taken_and_may_be_unset():
    port = the_port("pictures_or_sound")
    assert port["pin_declaration"] == {
        "pin_type": {"kind": "one_of", "pin_types": [{"type": "texture"}, {"type": "audio"}]},
        "flow": "stream",
        "may_be_unset": True,
    }


def _a_trigger_beside_a_stream() -> None:
    @node(execution="reactive")
    def Mixed(
        *,
        either: InputPin[Trigger | Texture],
    ) -> None:
        return None


def _numbers() -> None:
    @node(execution="reactive")
    def Numbers(
        *,
        number: InputPin[float | int],
    ) -> None:
        return None


def _a_stream_beside_a_number() -> None:
    @node(execution="reactive")
    def Mixed(
        *,
        either: InputPin[Texture | float],
    ) -> None:
        return None


def _a_narrowing_of_another_type() -> None:
    @node(execution="reactive")
    def Misnarrowed(
        *,
        tracks: InputPin[Annotated[Texture, EncodedCodecs("h264")] | Audio],
    ) -> None:
        return None


def _an_output_of_several_types() -> None:
    @node(execution="reactive")
    def Gives(
        *,
        out: OutputPin[Texture | Audio],
    ) -> None:
        return None


def _a_field_of_several_types() -> None:
    @node(execution="reactive")
    def Holds(
        *,
        level: Input[float | int] = Input(0.0),
    ) -> None:
        return None


@pytest.mark.parametrize(
    ("declare", "finding_code"),
    [
        (_a_trigger_beside_a_stream, "pin_type_not_read_yet"),
        (_numbers, "pin_type_not_read_yet"),
        (_a_stream_beside_a_number, "pin_type_not_read_yet"),
        (_a_narrowing_of_another_type, "pin_annotation_not_taken"),
        (_an_output_of_several_types, "pin_type_not_read_yet"),
        (_a_field_of_several_types, "pin_type_not_read_yet"),
    ],
)
def test_a_pin_of_several_types_is_refused_unless_it_is_an_input_pin_of_streams(
    declare: Any, finding_code: str
):
    with pytest.raises(NodeDeclarationRefusal) as refused:
        declare()
    assert refused.value.finding_code == finding_code, refused.value


if TYPE_CHECKING:
    from tatolab.stream import Message, Tensor

    def the_read_of_each_pin_of_several_types(
        tracks: InputPin[EncodedVideo | EncodedAudio],
        pictures_or_sound: InputPin[Texture | Audio | None],
        pictures: InputPin[Texture],
    ) -> None:
        """A read of a pin of several types is typed as what any of them reads, narrowed by `pin_type`."""
        assert_type(tracks.read(), "EncodedVideo | EncodedAudio | None")
        assert_type(tracks.read_message(), "Message[EncodedVideo | EncodedAudio] | None")
        assert_type(
            pictures_or_sound.read(),
            "Texture | Audio | Tensor | EncodedVideo | EncodedAudio | None",
        )
        assert_type(pictures.read(), "Texture | None")
