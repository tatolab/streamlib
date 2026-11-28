# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node function's output its Python body does not write binds all the same: one its
`PinTypeChoice` value input types, written by the node's native half, and one giving the node out
as a connection, which each wire from it brings."""

from __future__ import annotations

from typing import Annotated, Any, cast

import pytest

from tatolab.runtime._class_node_bound_pins import BoundOutputPin, bind_the_pins_of_a_class_node
from tatolab.runtime._class_node_fields import ClassNodeFields
from tatolab.stream import (
    Connection,
    EncodedAudio,
    EncodedCodecs,
    EncodedVideo,
    Input,
    OutputPin,
    PinTypeChoice,
    PinTypeFromSetting,
    connection_type,
    node,
)

H264 = Annotated[EncodedVideo, EncodedCodecs("h264")]
OPUS = Annotated[EncodedAudio, EncodedCodecs("opus")]


@node(execution="manual")
def Receive(
    *,
    content_type: Input[PinTypeChoice] = Input(PinTypeChoice(H264)),
    content: OutputPin[Annotated[H264 | OPUS, PinTypeFromSetting("content_type")]],
) -> None: ...


def a_receive_handed(stored_input_values: "dict[str, Any]") -> "tuple[dict[str, Any], dict[str, Any]]":
    """The values `Receive` is built with and its pins."""
    fields = ClassNodeFields(
        Receive.members,
        link_data_access=cast(Any, None),
        inputs=cast(Any, None),
        node_id="receive-1",
        stored_input_values=stored_input_values,
        assign_field_values=lambda node, values: None,
    )
    pins = bind_the_pins_of_a_class_node(
        Receive.members,
        link_data_access=cast(Any, None),
        inputs=cast(Any, None),
        outputs=cast(Any, None),
        node_id="receive-1",
    )
    return fields.values_to_build_with(), pins


def test_the_choice_holds_the_type_stored_and_its_default_while_none_is():
    values, _ = a_receive_handed({})
    assert values.get("content_type", PinTypeChoice(H264)) == PinTypeChoice(H264)
    opus = {"type": "encoded_audio", "codecs": ["opus"]}
    values, _ = a_receive_handed({"content_type": opus})
    assert values["content_type"].chosen == opus


def test_the_output_its_setting_types_binds_and_refuses_a_write_from_python():
    _, pins = a_receive_handed({})
    with pytest.raises(TypeError, match="of the type its setting `content_type` chooses"):
        pins["content"].write(b"\x00")


@connection_type
class _HubConnection(Connection):
    """A node a test gives out."""


@node(execution="manual")
def GivesItselfOut(*, connection: OutputPin[_HubConnection]) -> None: ...


def test_a_connection_output_binds_and_refuses_a_write_since_each_wire_brings_the_node():
    """Mental-revert: binding a connection output as a value it writes refuses the node at setup,
    as the MoQ gateway and track were refused on a running stream."""
    pins = bind_the_pins_of_a_class_node(
        GivesItselfOut.members,
        link_data_access=cast(Any, None),
        inputs=cast(Any, None),
        outputs=cast(Any, None),
        node_id="hub-1",
    )
    connection = pins["connection"]
    assert isinstance(connection, BoundOutputPin)
    with pytest.raises(TypeError, match="gives its node out as a connection"):
        connection.write(object())
