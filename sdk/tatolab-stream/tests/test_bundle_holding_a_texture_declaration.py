# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A frozen dataclass with a Texture field is one bundle pin a node function's `InputPin` and
`OutputPin` carry, as a stream; a value input, a pure function's pin and a List of bundles refuse it by
name, as it holds no stored value."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import pytest

from tatolab.stream import InputPin, NodeDeclarationRefusal, OutputPin, Rectangle, Texture, node
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.stream._pin_declaration import PinForm, read_pin_declaration


@dataclass(frozen=True)
class Framed:
    picture: Texture
    region: Rectangle = Rectangle(0.0, 0.0, 1.0, 1.0)


FRAMED = {
    "type": "bundle",
    "name": "Framed",
    "members": [
        {"name": "picture", "member_type": {"type": "texture"}},
        {
            "name": "region",
            "member_type": {"type": "rectangle"},
            "default_value": {"x": 0.0, "y": 0.0, "width": 1.0, "height": 1.0},
        },
    ],
}


@node(execution="reactive")
def Crop(*, framed: InputPin[Framed], cropped: OutputPin[Framed]) -> None: ...


def test_a_node_function_carries_a_bundle_holding_a_texture_on_one_input_and_one_output_pin() -> None:
    input_ports = getattr(Crop, NODE_DECLARATION_INPUT_PORTS_STAMP)
    output_ports = getattr(Crop, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    members = Crop.members
    by_name = {port["name"]: port for port in (*input_ports, *output_ports)}

    assert list(by_name) == ["framed", "cropped"], "the dataclass is one pin, not one per field"
    for port in by_name.values():
        assert port["pin_declaration"]["pin_type"] == {"kind": "is", "pin_type": FRAMED}
        assert port["pin_declaration"]["flow"] == "stream", (
            "a bundle holding a Texture travels as a stream, read newest, as a Texture does"
        )
    assert [member.bundle_class for member in members] == [Framed, Framed]


def refusal_of(annotation: Any, *, form: PinForm) -> NodeDeclarationRefusal:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        read_pin_declaration("Crop", "framed", annotation, direction="input", form=form)
    return refused.value


def test_a_field_refuses_a_bundle_holding_a_texture_with_an_input_pin_as_the_fix() -> None:
    refusal = refusal_of(Framed, form="field")

    assert refusal.finding_code == "field_typed_as_a_stream"
    assert str(refusal) == (
        "`Crop`'s input `framed` is a field typed `Framed`, a bundle whose member `picture` is a "
        "Texture, which holds no stored value, so only a pin carries it. To fix: "
        "`framed: InputPin[Framed]`."
    )


def test_a_pure_function_refuses_a_bundle_holding_a_texture_with_a_node_function_as_the_fix() -> None:
    refusal = refusal_of(Framed, form="function")

    assert refusal.finding_code == "pin_type_not_read_yet"
    assert "a function's body is handed values" in str(refusal)
    assert "`framed: InputPin[Framed]`" in str(refusal)


def test_a_list_of_bundles_holding_a_texture_is_refused_by_the_members_name() -> None:
    refusal = refusal_of(list[Framed], form="pin")

    assert refusal.finding_code == "list_element_not_taken"
    assert "member `picture` is typed `Texture`" in str(refusal)
    assert "a List of bundles, whose members are small types" in str(refusal)


@dataclass(frozen=True)
class Pictures:
    pictures: list[Texture]


def test_a_list_of_textures_as_a_member_is_refused() -> None:
    refusal = refusal_of(Pictures, form="pin")

    assert refusal.finding_code == "list_element_not_taken"
    assert "member `pictures`" in str(refusal)
