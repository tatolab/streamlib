# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A module's class node whose `PinTypeChoice` field types an output: the field holds the
`PinTypeChoice` of the type the runtime stores, and the output binds, written by the node's
native half and refused to a Python body."""

from __future__ import annotations

from typing import Annotated, Any, cast

import pytest

from tatolab.runtime._class_node_bound_pins import bind_the_pins_of_a_class_node
from tatolab.runtime._class_node_fields import ClassNodeFields
from tatolab.runtime._node_hosting import class_node_members_of
from tatolab.stream import (
    EncodedAudio,
    EncodedCodecs,
    EncodedVideo,
    OutputPin,
    PinTypeChoice,
    PinTypeFromSetting,
    node,
)
from tatolab.stream._class_node_construction import construct_class_node

H264 = Annotated[EncodedVideo, EncodedCodecs("h264")]
OPUS = Annotated[EncodedAudio, EncodedCodecs("opus")]


@node(execution="manual")
class Receive:
    content_type: PinTypeChoice = PinTypeChoice(H264)
    content: OutputPin[Annotated[H264 | OPUS, PinTypeFromSetting("content_type")]]

    def process(self, ctx: Any) -> None:
        del ctx


def a_receive_handed(stored_input_values: "dict[str, Any]") -> Any:
    members = class_node_members_of(Receive)
    assert members is not None
    fields = ClassNodeFields(
        members,
        link_data_access=cast(Any, None),
        inputs=cast(Any, None),
        node_id="receive-1",
        stored_input_values=stored_input_values,
    )
    pins = bind_the_pins_of_a_class_node(
        members,
        link_data_access=cast(Any, None),
        inputs=cast(Any, None),
        outputs=cast(Any, None),
        node_id="receive-1",
    )
    return construct_class_node(Receive, field_values=fields.values_to_build_with(), pins=pins)


def test_the_choice_holds_the_type_stored_and_its_default_while_none_is():
    assert a_receive_handed({}).content_type == PinTypeChoice(H264)
    opus = {"type": "encoded_audio", "codecs": ["opus"]}
    assert a_receive_handed({"content_type": opus}).content_type.chosen == opus


def test_the_output_its_setting_types_binds_and_refuses_a_write_from_python():
    receive = a_receive_handed({})
    with pytest.raises(TypeError, match="of the type its setting `content_type` chooses"):
        receive.content.write(b"\x00")
