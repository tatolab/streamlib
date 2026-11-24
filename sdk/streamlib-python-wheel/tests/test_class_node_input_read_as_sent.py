# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class node's input pin hands a gateway each message with its payload's bytes as they were
sent, whichever of the pin's types it carries, and a pin that carries a value refuses that read."""

from __future__ import annotations

from typing import Annotated, Any, cast

import pytest

from tatolab.runtime._class_node_bound_pins import bind_the_pins_of_a_class_node
from tatolab.runtime._node_hosting import class_node_members_of
from tatolab.stream import Audio, EncodedVideo, InputPin, StreamDelivery, node


@node(execution="manual")
class Gateway:
    content: InputPin[Annotated[EncodedVideo | Audio, StreamDelivery("ordered")]]
    level: InputPin[Annotated[float, StreamDelivery("newest")]]

    def process(self, ctx: Any) -> None:
        del ctx


class InputsReadByName:
    """Inputs that record what each read names and hand back what it was asked for."""

    def __init__(self) -> None:
        self.reads: "list[tuple[Any, ...]]" = []

    def read_message(self, port: Any) -> Any:
        self.reads.append(("read_message", port))
        return f"message on {port}"


def the_pins_of_a_gateway(inputs: InputsReadByName) -> "dict[str, Any]":
    members = class_node_members_of(Gateway)
    assert members is not None
    return bind_the_pins_of_a_class_node(
        members,
        link_data_access=cast(Any, None),
        inputs=cast(Any, inputs),
        outputs=cast(Any, None),
        node_id="gateway-1",
    )


def test_a_read_as_sent_names_the_port_so_its_payload_is_the_bytes_sent():
    inputs = InputsReadByName()
    assert the_pins_of_a_gateway(inputs)["content"].read_message_as_sent() == "message on content"
    assert inputs.reads == [("read_message", "content")]


def test_a_pin_carrying_a_value_refuses_a_read_as_sent():
    pins = the_pins_of_a_gateway(InputsReadByName())
    with pytest.raises(TypeError, match="`gateway-1`'s pin `level` carries a value"):
        pins["level"].read_message_as_sent()
