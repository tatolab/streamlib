# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A module's class node types an output by a `PinTypeChoice` field: the output's union is what
the field may choose, and the output carries the type it chooses."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, Any, Self

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
    PinTypeChoice,
    PinTypeFromSetting,
    Texture,
    named_pin_type,
    node,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

NOTE = f"{__name__}:Note"


@named_pin_type(description="One line of text a test sends.")
@dataclass(frozen=True)
class Note:
    text: str

    @classmethod
    def from_bytes(cls, payload: Any) -> Self:
        return cls(bytes(payload).decode())

    def to_bytes(self) -> bytes:
        return self.text.encode()


H264 = Annotated[EncodedVideo, EncodedCodecs("h264")]
OPUS = Annotated[EncodedAudio, EncodedCodecs("opus")]


@node(execution="manual")
class Receive:
    content_type: PinTypeChoice = PinTypeChoice(Note)
    content: OutputPin[
        Annotated[H264 | OPUS | Note, PinTypeFromSetting("content_type"), Description("What arrives")]
    ]
    pictures_or_sound: InputPin[Texture | Note]

    def process(self, ctx: Any) -> None:
        del ctx


def the_port(ports_stamp: str, port_name: str) -> "dict[str, Any]":
    (port,) = [port for port in getattr(Receive, ports_stamp) if port["name"] == port_name]
    return port


def test_the_output_carries_the_type_its_setting_chooses():
    assert the_port(NODE_DECLARATION_OUTPUT_PORTS_STAMP, "content") == {
        "name": "content",
        "description": "What arrives",
        "pin_declaration": {
            "pin_type": {"kind": "set_by_input", "input_name": "content_type"},
            "flow": "stream",
        },
    }


def test_the_setting_may_choose_each_type_of_the_union_and_defaults_to_one():
    choice = the_port(NODE_DECLARATION_INPUT_PORTS_STAMP, "content_type")
    assert choice["pin_declaration"]["pin_type"] == {
        "kind": "is",
        "pin_type": {
            "type": "pin_type_choice",
            "among": [
                {"type": "encoded_video", "codecs": ["h264"]},
                {"type": "encoded_audio", "codecs": ["opus"]},
                {"type": "named", "named_pin_type": NOTE},
            ],
        },
    }
    assert choice["pin_default"] == {"type": "named", "named_pin_type": NOTE}
    assert choice["input_mode"] == {"mode": "setting_only", "read": "at_setup"}


def test_an_input_of_several_types_takes_a_named_type():
    port = the_port(NODE_DECLARATION_INPUT_PORTS_STAMP, "pictures_or_sound")
    assert port["pin_declaration"]["pin_type"] == {
        "kind": "one_of",
        "pin_types": [{"type": "texture"}, {"type": "named", "named_pin_type": NOTE}],
    }


def _a_default_none_of_the_union_chooses() -> None:
    @node(execution="manual")
    class ChoosesSound:
        content_type: PinTypeChoice = PinTypeChoice(Audio)
        content: OutputPin[Annotated[H264 | OPUS, PinTypeFromSetting("content_type")]]

        def process(self, ctx: Any) -> None:
            del ctx


def _a_choice_holding_no_default() -> None:
    @node(execution="manual")
    class ChoosesNothingByDefault:
        content_type: PinTypeChoice | None = None
        content: OutputPin[Annotated[H264 | OPUS, PinTypeFromSetting("content_type")]]

        def process(self, ctx: Any) -> None:
            del ctx


def _a_choice_no_output_names() -> None:
    @node(execution="manual")
    class ChoosesNothing:
        content_type: PinTypeChoice = PinTypeChoice(Note)

        def process(self, ctx: Any) -> None:
            del ctx


def _an_output_naming_no_choice() -> None:
    @node(execution="manual")
    class NamesNoChoice:
        content: OutputPin[Annotated[H264 | OPUS, PinTypeFromSetting("content_type")]]

        def process(self, ctx: Any) -> None:
            del ctx


def _two_outputs_naming_one_choice() -> None:
    @node(execution="manual")
    class TwoOutputs:
        content_type: PinTypeChoice = PinTypeChoice(Note)
        content: OutputPin[Annotated[H264 | Note, PinTypeFromSetting("content_type")]]
        copy: OutputPin[Annotated[H264 | Note, PinTypeFromSetting("content_type")]]

        def process(self, ctx: Any) -> None:
            del ctx


def _an_input_pin_a_setting_types() -> None:
    @node(execution="manual")
    class TypesAnInput:
        content_type: PinTypeChoice = PinTypeChoice(Note)
        content: InputPin[Annotated[H264 | Note, PinTypeFromSetting("content_type")]]

        def process(self, ctx: Any) -> None:
            del ctx


def _one_type_a_setting_types() -> None:
    @node(execution="manual")
    class OneType:
        content_type: PinTypeChoice = PinTypeChoice(Note)
        content: OutputPin[Annotated[Note, PinTypeFromSetting("content_type")]]

        def process(self, ctx: Any) -> None:
            del ctx


def _a_value_among_the_types() -> None:
    @node(execution="manual")
    class ChoosesANumber:
        content_type: PinTypeChoice = PinTypeChoice(Note)
        content: OutputPin[Annotated[float | Note, PinTypeFromSetting("content_type")]]

        def process(self, ctx: Any) -> None:
            del ctx


def _a_narrowing_on_the_whole_union() -> None:
    @node(execution="manual")
    class NarrowsTheUnion:
        content_type: PinTypeChoice = PinTypeChoice(Note)
        content: OutputPin[
            Annotated[EncodedVideo | Note, EncodedCodecs("h264"), PinTypeFromSetting("content_type")]
        ]

        def process(self, ctx: Any) -> None:
            del ctx


@pytest.mark.parametrize(
    ("declare", "finding_code", "words"),
    [
        (_a_default_none_of_the_union_chooses, "pin_default_not_of_its_type", "none of the types"),
        (_a_choice_holding_no_default, "pin_default_not_of_its_type", "no type by default"),
        (_a_choice_no_output_names, "pin_annotation_not_taken", "types no output"),
        (_an_output_naming_no_choice, "output_type_found_from_no_input", "no `PinTypeChoice` field"),
        (_two_outputs_naming_one_choice, "pin_annotation_repeated", "both typed by the setting"),
        (_an_input_pin_a_setting_types, "pin_type_not_read_yet", "the type of an `OutputPin` alone"),
        (_one_type_a_setting_types, "pin_annotation_not_taken", "nothing to choose"),
        (_a_value_among_the_types, "pin_type_not_read_yet", "which is no stream"),
        (_a_narrowing_on_the_whole_union, "pin_annotation_not_taken", "narrow each type"),
    ],
)
def test_a_type_choice_is_refused_unless_it_types_one_output_of_several_streams(
    declare: Any, finding_code: str, words: str
):
    with pytest.raises(NodeDeclarationRefusal, match=words) as refused:
        declare()
    assert refused.value.finding_code == finding_code, refused.value
