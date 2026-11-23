# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class node's type parameter bound to one pin type types its `InputPin` by that type, and an
`OutputPin` typed by the same parameter carries the type linked into that input."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, Any

import pytest

from tatolab.stream import (
    Audio,
    Description,
    InputPin,
    NodeDeclarationRefusal,
    OutputPin,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    node,
)
from tatolab.stream._class_node_declaration import read_class_node_ports_and_members
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)


@node.meta(description="Hands on each picture it reads.")
@node
class HandsOnAPicture[Frame: Texture]:
    picture_out: OutputPin[Annotated[Frame, Description("Each picture read, handed on.")]]
    picture_in: InputPin[Annotated[Frame, StreamDelivery("newest"), Description("The pictures.")]]

    def process(self) -> None: ...


def the_one_port(declared: Any, stamp: str) -> dict[str, Any]:
    (port,) = getattr(declared, stamp)
    return port


def test_an_input_pin_typed_by_the_parameter_streams_the_type_it_is_bound_to() -> None:
    picture_in = the_one_port(HandsOnAPicture, NODE_DECLARATION_INPUT_PORTS_STAMP)

    assert picture_in == {
        "name": "picture_in",
        "description": "The pictures.",
        "family": {"family": "image"},
        "pin_declaration": {
            "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
            "flow": "stream",
        },
        "delivery_profile": "newest",
    }


def test_an_output_pin_typed_by_the_parameter_follows_the_input_it_types_declared_after_it() -> None:
    picture_out = the_one_port(HandsOnAPicture, NODE_DECLARATION_OUTPUT_PORTS_STAMP)

    assert picture_out == {
        "name": "picture_out",
        "description": "Each picture read, handed on.",
        "family": {"family": "image", "family_follows_input_port": "picture_in"},
        "pin_declaration": {
            "pin_type": {"kind": "same_as_input", "input_port_name": "picture_in"},
            "flow": "stream",
        },
    }


@dataclass(frozen=True)
class Framed:
    picture: Texture


class HandsOnAFramedPicture[Picture: Framed]:
    framed_in: InputPin[Picture]
    framed_out: OutputPin[Picture]


def test_a_pin_typed_by_a_parameter_bound_to_a_bundle_is_read_and_written_as_that_bundle() -> None:
    _, _, members = read_class_node_ports_and_members(HandsOnAFramedPicture)

    assert [member.bundle_class for member in members] == [Framed, Framed]


class ReadsThroughAClassLocalAlias:
    _Picture = Texture
    picture_in: InputPin[_Picture]


def test_a_class_with_no_type_parameter_still_resolves_its_class_local_names() -> None:
    picture_in = the_one_port(node(ReadsThroughAClassLocalAlias), NODE_DECLARATION_INPUT_PORTS_STAMP)

    assert picture_in["pin_declaration"]["pin_type"] == {"kind": "is", "pin_type": {"type": "texture"}}


class Unbound[Frame]:
    picture_in: InputPin[Frame]
    picture_out: OutputPin[Frame]


class Constrained[Frame: (Texture, Audio)]:
    picture_in: InputPin[Frame]
    picture_out: OutputPin[Frame]


class BoundToAUnion[Frame: Texture | Audio]:
    picture_in: InputPin[Frame]
    picture_out: OutputPin[Frame]


class MayBeUnset[Frame: Texture]:
    picture_in: InputPin[Frame | None]


class TypesNoInput[Frame: Texture]:
    picture_out: OutputPin[Frame]


class TypesTwoInputs[Frame: Texture]:
    left: InputPin[Frame]
    right: InputPin[Frame]
    picture_out: OutputPin[Frame]


class NarrowsTheFollowingOutput[Frame: Texture]:
    picture_in: InputPin[Frame]
    picture_out: OutputPin[Annotated[Frame, TexturePixelFormats("rgba8_unorm")]]


@pytest.mark.parametrize(
    ("node_class", "pin", "finding_code", "fix"),
    [
        (Unbound, "picture_in", "pin_type_not_read_yet", "`class Unbound[Frame: Texture]`"),
        (Constrained, "picture_in", "pin_type_not_read_yet", "bind it to one type"),
        (BoundToAUnion, "picture_in", "pin_type_not_read_yet", "bind it to one type"),
        (MayBeUnset, "picture_in", "pin_type_not_read_yet", "type the pin `Frame`"),
        (TypesNoInput, "picture_out", "output_type_found_from_no_input", "type the input"),
        (TypesTwoInputs, "picture_out", "type_parameter_types_several_inputs", "a type parameter for each input"),
        (NarrowsTheFollowingOutput, "picture_out", "pin_annotation_not_taken", "drop it"),
    ],
)
def test_each_mistake_with_a_type_parameter_is_refused_with_what_to_write(
    node_class: type, pin: str, finding_code: str, fix: str
) -> None:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node(node_class)

    assert refused.value.finding_code == finding_code
    assert refused.value.pin == pin
    assert fix in str(refused.value)
