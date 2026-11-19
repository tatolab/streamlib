# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The node the stream's function node pool runs.

Each island the pool carries feeds one of its inputs and is evaluated at that
input's read, as in any reader; the pool writes the value to the matching
output, whose channel reaches the reader that could not evaluate the island
itself. A control message, a number, a bool or the parts of a Color, a Vector,
a Rectangle or a Transform one on each channel, and a message the island hands
on unread are written exactly as the island answered them, their descriptor
unchanged; a `dict` is written as a Records message.
"""

from __future__ import annotations

from typing import Any, Mapping, Sequence

from tatolab.stream import (
    Array,
    Channels,
    Image,
    Input,
    NodeLinkDataAccess,
    Output,
    Packets,
    PortFamily,
    Records,
    node,
)

from tatolab.runtime._engine import hand_an_unread_message_on, read_the_message_at_an_input_port_unread

# What a carry's `family` names, besides `"channels"`, a control message: a record, or a message
# handed on unread.
_CARRIES_A_RECORD = "records"
_CARRIES_A_MESSAGE_HANDED_ON_UNREAD = "message"

_PORT_FAMILIES_BY_WIRE_NAME: "dict[str, PortFamily[Any]]" = {
    "array": Array,
    "channels": Channels,
    "image": Image,
    "packets": Packets,
    "records": Records,
}


class FunctionNodePool:
    """What the pool's helper loads; its node is built from the pool's config at setup."""


def the_node_class_the_function_node_pool_runs_as(
    pool_config: "Mapping[str, Any]", link_data_access: NodeLinkDataAccess
) -> type:
    """A `@node.process` class with one input and one output per island `pool_config["carries"]` names.

    Wire contract with the engine: `{"carries": [{"input_port", "output_port",
    "family": "channels" | "records" | "message", "message_family"}]}`, a
    `"message"` carry naming the family of the message it hands on.
    """
    carries: "Sequence[Mapping[str, Any]]" = pool_config.get("carries") or []
    class_attributes: "dict[str, Any]" = {}
    for carry in carries:
        family = _the_port_family_a_carry_declares(carry)
        class_attributes[carry["input_port"]] = Input(family, delivery="newest")
        class_attributes[carry["output_port"]] = Output(family)
    class_attributes["__doc__"] = "The stream's function node pool."
    class_attributes["__module__"] = __name__
    pool_class = type("FunctionNodePoolNode", (_FunctionNodePoolBase,), class_attributes)
    pool_class._carries = tuple(
        (carry["input_port"], carry["output_port"], carry["family"]) for carry in carries
    )
    pool_class._link_data_access = link_data_access
    return node.process(execution="continuous")(pool_class)


def _the_port_family_a_carry_declares(carry: "Mapping[str, Any]") -> "PortFamily[Any]":
    if carry["family"] == _CARRIES_A_MESSAGE_HANDED_ON_UNREAD:
        return _PORT_FAMILIES_BY_WIRE_NAME[carry["message_family"]]
    if carry["family"] == _CARRIES_A_RECORD:
        return Records
    return Channels


class _FunctionNodePoolBase:
    """Reads each carried island and writes its value to the island's output."""

    _carries: "tuple[tuple[str, str, str], ...]"
    _link_data_access: NodeLinkDataAccess

    def process(self, ctx: Any) -> None:
        for input_port, output_port, carries in self._carries:
            if carries == _CARRIES_A_RECORD:
                record = ctx.inputs.read(getattr(type(self), input_port))
                if record is not None:
                    ctx.outputs.write(getattr(type(self), output_port), record)
                continue
            message = read_the_message_at_an_input_port_unread(
                link_data_access=self._link_data_access, input_port=input_port
            )
            if message is not None:
                hand_an_unread_message_on(
                    link_data_access=self._link_data_access,
                    output_port=output_port,
                    message=message,
                )
