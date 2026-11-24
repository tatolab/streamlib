# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A frozen dataclass on a node under bare `@node` is one bundle pin whose members are its
fields, a `list` of one a List of bundles, and each member a bundle cannot hold is refused by
its name."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Annotated, Any, cast

import pytest

from tatolab.stream import (
    Audio,
    Color,
    Description,
    InputPin,
    NodeDeclarationRefusal,
    NumberRange,
    OutputPin,
    PinMeaning,
    Texture,
    node,
)
from tatolab.stream._class_node_declaration import read_class_node_ports_and_members
from tatolab.stream._contract_pin_types import PinType
from tatolab.stream._function_node_declaration import NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
from tatolab.stream._pin_declaration import NO_DEFAULT, read_pin_declaration
from tatolab.stream._pin_type_choice import pin_type_in_words
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)


@dataclass(frozen=True)
class Tone:
    frequency: Annotated[float, PinMeaning("frequency")]
    gain: float = 1.0
    tint: Color = Color(1.0, 0.0, 0.0)
    levels: list[float] = field(default_factory=lambda: [0.5])


@dataclass(frozen=True)
class Pitch:
    frequency: Annotated[float, PinMeaning("frequency")]
    gain: float = 1.0
    tint: Color = Color(1.0, 0.0, 0.0)
    levels: list[float] = field(default_factory=lambda: [0.5])


@dataclass(frozen=True)
class Dot:
    x: float
    y: float = 0.0


@node
def Louder(tone: Tone = Tone(440.0)) -> Tone:
    return Tone(tone.frequency, tone.gain * 2)


@node
def Scatter(dots: list[Dot] = [Dot(1.0)]) -> list[Dot]:  # noqa: B006
    return dots


TONE_MEMBERS = [
    {"name": "frequency", "member_type": {"type": "float"}, "meaning": "frequency"},
    {"name": "gain", "member_type": {"type": "float"}, "default_value": 1.0},
    {
        "name": "tint",
        "member_type": {"type": "color"},
        "default_value": {"red": 1.0, "green": 0.0, "blue": 0.0, "alpha": 1.0},
    },
    {
        "name": "levels",
        "member_type": {"type": "list_of", "element_type": {"type": "float"}},
        "default_value": [0.5],
    },
]


def ports(function_node: Any, stamp: str) -> "dict[str, dict[str, Any]]":
    return {port["name"]: port for port in getattr(function_node, stamp)}


def the_one_pin_type(port: "dict[str, Any]") -> "dict[str, Any]":
    declared = port["pin_declaration"]["pin_type"]
    assert declared["kind"] == "is"
    return declared["pin_type"]


def test_a_function_taking_and_returning_a_dataclass_has_one_bundle_input_and_one_bundle_output() -> None:
    inputs = ports(Louder, NODE_DECLARATION_INPUT_PORTS_STAMP)
    outputs = ports(Louder, NODE_DECLARATION_OUTPUT_PORTS_STAMP)

    assert list(inputs) == ["tone"], "the dataclass is one input, not one per field"
    assert list(outputs) == ["result"], "the returned dataclass is one output"
    assert the_one_pin_type(inputs["tone"]) == {"type": "bundle", "name": "Tone", "members": TONE_MEMBERS}
    assert the_one_pin_type(outputs["result"]) == {"type": "bundle", "name": "Tone", "members": TONE_MEMBERS}
    assert inputs["tone"]["family"] == {"family": "records"}
    assert inputs["tone"]["pin_default"] == {
        "frequency": 440.0,
        "gain": 1.0,
        "tint": {"red": 1.0, "green": 0.0, "blue": 0.0, "alpha": 1.0},
        "levels": [0.5],
    }, "a bundle's default is its dataclass's value, each member by name"


def test_the_signature_names_the_dataclass_the_body_is_handed_and_gives() -> None:
    signature = getattr(Louder, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    [tone] = signature["inputs"]
    [result] = signature["outputs"]

    assert tone["value_type"] == "bundle"
    assert tone["bundle_class"] == f"{__name__}:Tone"
    assert result["value_type"] == "bundle"
    assert result["bundle_class"] == f"{__name__}:Tone"
    assert result["field"] is None, "a returned dataclass is handed out whole"


@node
def Retune(pitch: Pitch = Pitch(220.0)) -> Pitch:
    return pitch


def test_a_bundle_is_named_by_its_dataclass_and_two_with_the_same_members_differ_by_name() -> None:
    pitch = the_one_pin_type(ports(Retune, NODE_DECLARATION_INPUT_PORTS_STAMP)["pitch"])
    tone = the_one_pin_type(ports(Louder, NODE_DECLARATION_INPUT_PORTS_STAMP)["tone"])

    assert (pitch["name"], tone["name"]) == ("Pitch", "Tone"), "each names its dataclass"
    assert pitch["members"] == tone["members"], "the members alone are what a link compares"
    words = pin_type_in_words(cast(PinType, pitch))
    assert words.startswith("Bundle `Pitch` (frequency: "), words


def test_a_list_of_a_dataclass_is_one_list_pin_whose_elements_are_bundles() -> None:
    dots = ports(Scatter, NODE_DECLARATION_INPUT_PORTS_STAMP)["dots"]
    [signature_input] = getattr(Scatter, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)["inputs"]

    assert the_one_pin_type(dots) == {
        "type": "list_of",
        "element_type": {
            "type": "bundle",
            "name": "Dot",
            "members": [
                {"name": "x", "member_type": {"type": "float"}},
                {"name": "y", "member_type": {"type": "float"}, "default_value": 0.0},
            ],
        },
    }
    assert dots["pin_default"] == [{"x": 1.0, "y": 0.0}]
    assert signature_input["value_type"] == "list_of"
    assert signature_input["bundle_class"] == f"{__name__}:Dot"


def test_a_class_node_reads_a_bundle_as_a_field_an_input_pin_and_an_output_pin() -> None:
    class Mixer:
        tone: Tone = Tone(220.0)
        incoming: InputPin[Tone]
        outgoing: OutputPin[Tone]

        def process(self, ctx: Any) -> None: ...

    input_ports, output_ports, members = read_class_node_ports_and_members(Mixer)
    by_name = {port["name"]: port for port in (*input_ports, *output_ports)}

    assert [(name, port["pin_declaration"]["flow"]) for name, port in by_name.items()] == [
        ("tone", "value"),
        ("incoming", "stream"),
        ("outgoing", "value"),
    ], "a field is read as a value, an `InputPin` message by message"
    for port in by_name.values():
        assert the_one_pin_type(port) == {"type": "bundle", "name": "Tone", "members": TONE_MEMBERS}
    assert [member.bundle_class for member in members] == [Tone, Tone, Tone]


@dataclass(frozen=True)
class HoldsAPicture:
    level: float = 0.0
    picture: Texture | None = None


@dataclass(frozen=True)
class HoldsADot:
    dot: Dot = Dot(0.0)


@dataclass(frozen=True)
class HoldsAList:
    levels: list[float] = field(default_factory=list)


@dataclass(frozen=True)
class MayHoldNothing:
    level: float | None = None


@dataclass(frozen=True)
class StatesARange:
    level: Annotated[float, NumberRange(minimum=0.0, maximum=1.0)] = 0.0


@dataclass(frozen=True)
class DescribesALevel:
    level: Annotated[float, Description("The level.")] = 0.0


@dataclass(frozen=True)
class HoldsAVoice:
    voice: Audio


@dataclass
class Changes:
    level: float = 0.0


@dataclass(frozen=True)
class HoldsNothing:
    pass


def refusal_of(annotation: Any, default: Any) -> NodeDeclarationRefusal:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        read_pin_declaration("Probe", "value", annotation, default=default, direction="input")
    return refused.value


def test_a_member_a_bundle_does_not_carry_yet_is_refused_by_its_name_with_its_own_output_as_the_fix() -> None:
    refusal = refusal_of(HoldsAVoice, None)

    assert refusal.finding_code == "pin_type_not_read_yet"
    assert refusal.pin == "value"
    assert str(refusal) == (
        "`Probe`'s input `value`'s member `voice` is typed `Audio`, a Audio, which a "
        "bundle does not carry yet. To fix: give it an output of its own beside `value`."
    )


def test_a_nested_bundle_is_refused_with_its_members_written_on_the_outer_dataclass_as_the_fix() -> None:
    refusal = refusal_of(HoldsADot, HoldsADot())

    assert refusal.finding_code == "pin_type_not_read_yet"
    assert str(refusal) == (
        "`Probe`'s input `value`'s member `dot` is typed `Dot`, a dataclass, and a bundle holds "
        "no bundle. To fix: write the fields of `Dot` on the dataclass of `value` itself."
    )


@pytest.mark.parametrize(
    ("annotation", "default", "finding_code", "words"),
    [
        (HoldsAPicture, HoldsAPicture(), "pin_type_unknown", "member `picture` is typed `T | None`"),
        (MayHoldNothing, MayHoldNothing(), "pin_type_unknown", "member `level` is typed `T | None`"),
        (StatesARange, StatesARange(), "pin_annotation_not_taken", "member `level` carries NumberRange"),
        (DescribesALevel, DescribesALevel(), "pin_annotation_not_taken", "member `level` carries"),
        (list[HoldsAList], [], "list_element_not_taken", "member `levels` is typed `list[float]`"),
        (Changes, Changes(), "pin_type_unknown", "a dataclass that is not frozen"),
        (HoldsNothing, HoldsNothing(), "pin_type_unknown", "a dataclass with no field"),
    ],
)
def test_each_member_a_bundle_cannot_hold_is_refused_at_declaration_by_name(
    annotation: Any, default: Any, finding_code: str, words: str
) -> None:
    refusal = refusal_of(annotation, default)

    assert refusal.finding_code == finding_code
    assert words in str(refusal)
    assert "To fix:" in str(refusal)


def test_a_bundle_input_with_no_default_names_its_dataclass_in_the_fix() -> None:
    refused = refusal_of(Dot, NO_DEFAULT)

    assert refused.finding_code == "value_input_has_no_default"
    assert "`Dot | None = None`" in str(refused)


def test_a_bundle_default_of_other_fields_is_refused_naming_its_members() -> None:
    refusal = refusal_of(Dot, Tone(1.0))

    assert refusal.finding_code == "pin_default_not_of_its_type"
    assert "whose fields are not its members, 'x', 'y'" in str(refusal)
