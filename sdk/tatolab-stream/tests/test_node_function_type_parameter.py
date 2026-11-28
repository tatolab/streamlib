# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node function's type parameter bound to one pin type types its `InputPin` by that type, and an
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
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)


@node.meta(description="Hands on each picture it reads.")
@node(execution="reactive")
def HandsOnAPicture[Frame: Texture](
    *,
    picture_out: OutputPin[Annotated[Frame, Description("Each picture read, handed on.")]],
    picture_in: InputPin[Annotated[Frame, StreamDelivery("newest"), Description("The pictures.")]],
) -> None: ...


def the_one_port(declared: Any, stamp: str) -> dict[str, Any]:
    (port,) = getattr(declared, stamp)
    return port


def test_an_input_pin_typed_by_the_parameter_streams_the_type_it_is_bound_to() -> None:
    picture_in = the_one_port(HandsOnAPicture, NODE_DECLARATION_INPUT_PORTS_STAMP)

    assert picture_in == {
        "name": "picture_in",
        "description": "The pictures.",
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
        "pin_declaration": {
            "pin_type": {"kind": "same_as_input", "input_port_name": "picture_in"},
            "flow": "stream",
        },
    }


@dataclass(frozen=True)
class Framed:
    picture: Texture


@node(execution="reactive")
def HandsOnAFramedPicture[Picture: Framed](
    *, framed_in: InputPin[Picture], framed_out: OutputPin[Picture]
) -> None: ...


def test_a_pin_typed_by_a_parameter_bound_to_a_bundle_is_read_and_written_as_that_bundle() -> None:
    assert [member.bundle_class for member in HandsOnAFramedPicture.members] == [Framed, Framed]


def Unbound[Frame](*, picture_in: InputPin[Frame], picture_out: OutputPin[Frame]) -> None: ...


def Constrained[Frame: (Texture, Audio)](
    *, picture_in: InputPin[Frame], picture_out: OutputPin[Frame]
) -> None: ...


def BoundToAUnion[Frame: Texture | Audio](
    *, picture_in: InputPin[Frame], picture_out: OutputPin[Frame]
) -> None: ...


def MayBeUnset[Frame: Texture](*, picture_in: InputPin[Frame | None]) -> None: ...


def TypesNoInput[Frame: Texture](*, picture_out: OutputPin[Frame]) -> None: ...


def TypesTwoInputs[Frame: Texture](
    *, left: InputPin[Frame], right: InputPin[Frame], picture_out: OutputPin[Frame]
) -> None: ...


def NarrowsTheFollowingOutput[Frame: Texture](
    *,
    picture_in: InputPin[Frame],
    picture_out: OutputPin[Annotated[Frame, TexturePixelFormats("rgba8_unorm")]],
) -> None: ...


@pytest.mark.parametrize(
    ("function", "pin", "finding_code", "fix"),
    [
        (Unbound, "picture_in", "pin_type_not_read_yet", "`def Unbound[Frame: Texture](...)`"),
        (Constrained, "picture_in", "pin_type_not_read_yet", "bind it to one type"),
        (BoundToAUnion, "picture_in", "pin_type_not_read_yet", "bind it to one type"),
        (MayBeUnset, "picture_in", "pin_type_not_read_yet", "type the pin `Frame`"),
        (TypesNoInput, "picture_out", "output_type_found_from_no_input", "type the input"),
        (TypesTwoInputs, "picture_out", "type_parameter_types_several_inputs", "a type parameter for each input"),
        (NarrowsTheFollowingOutput, "picture_out", "pin_annotation_not_taken", "drop it"),
    ],
)
def test_each_mistake_with_a_type_parameter_is_refused_with_what_to_write(
    function: Any, pin: str, finding_code: str, fix: str
) -> None:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        node(execution="reactive")(function)

    assert refused.value.finding_code == finding_code
    assert refused.value.pin == pin
    assert fix in str(refused.value)
