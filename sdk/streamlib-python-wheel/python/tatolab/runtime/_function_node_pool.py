# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The node the stream's function node pool runs.

Each island the pool carries feeds one of its inputs and is evaluated at that
input's read, as in any reader; the pool writes the value to the matching
output, whose channel reaches the reader that could not evaluate the island
itself. A control message, a number, a bool or the parts of a Color, a Vector,
a Rectangle or a Transform one on each channel, and a message the island hands
on unread are written exactly as the island answered them, their descriptor
unchanged; a `dict` is written as a Records message. An island carried into a
setting its reader reads at setup is written as the pool sets up too, so the
reader, which the engine sets up after the pool, is built with its value; an
island that raises there is logged, and its reader is built with the setting's
own value. Such an island is written again only once its value changes, since
each value the engine reads into a setting may rebuild its node.
"""

from __future__ import annotations

from typing import Any, Mapping, Optional, Sequence

from tatolab.stream import NodeLinkDataAccess, log

from tatolab.runtime._engine import (
    an_unread_message_carries_what_another_does,
    hand_an_unread_message_on,
    read_the_message_at_an_input_port_unread,
)
from tatolab.runtime._node_the_wheel_hosts import stamp_the_ports_of_a_node_the_wheel_hosts

# What a carry's `family` names, besides `"channels"`, a control message: a record, or a message
# handed on unread.
_CARRIES_A_RECORD = "records"
_CARRIES_A_MESSAGE_HANDED_ON_UNREAD = "message"


class FunctionNodePool:
    """What the pool's helper loads; its node is built from the pool's config at setup."""


def the_node_class_the_function_node_pool_runs_as(
    pool_config: "Mapping[str, Any]", link_data_access: NodeLinkDataAccess
) -> type:
    """A node class with one input and one output per island `pool_config["carries"]` names, run continuously.

    Wire contract with the engine: `{"carries": [{"input_port", "output_port",
    "family": "channels" | "records" | "message", "message_family",
    "written_at_setup"}]}`, a `"message"` carry naming the family of the message
    it hands on, and a carry into a setting read at setup `"written_at_setup":
    true`.
    """
    carries: "Sequence[Mapping[str, Any]]" = pool_config.get("carries") or []
    pool_class = type(
        "FunctionNodePoolNode",
        (_FunctionNodePoolBase,),
        {"__doc__": "The stream's function node pool.", "__module__": __name__},
    )
    pool_class._carries = tuple(
        (carry["input_port"], carry["output_port"], carry["family"]) for carry in carries
    )
    pool_class._input_ports_written_at_setup = frozenset(
        carry["input_port"] for carry in carries if carry.get("written_at_setup")
    )
    pool_class._link_data_access = link_data_access
    return stamp_the_ports_of_a_node_the_wheel_hosts(
        pool_class,
        input_ports=[carry["input_port"] for carry in carries],
        output_ports=[carry["output_port"] for carry in carries],
    )


def take_the_carries_a_running_pool_is_handed(
    pool_class: type, pool_config: "Mapping[str, Any]", link_data_access: NodeLinkDataAccess
) -> None:
    """Take each carry `pool_config["carries"]` names that the running pool does not carry yet.

    Its two ports are declared before the engine wires the links into and out of them, and
    the pool reads and writes them from its next `process` on, so a carry joins a running
    pool without a restart. A carry the pool already holds is left as it is.
    """
    carried_input_ports = {input_port for input_port, _, _ in pool_class._carries}
    new_carries = [
        carry
        for carry in (pool_config.get("carries") or [])
        if carry["input_port"] not in carried_input_ports
    ]
    stamp_the_ports_of_a_node_the_wheel_hosts(
        pool_class,
        input_ports=[carry["input_port"] for carry in new_carries],
        output_ports=[carry["output_port"] for carry in new_carries],
    )
    link_data_access.declare_ports(
        [carry["input_port"] for carry in new_carries],
        [carry["output_port"] for carry in new_carries],
    )
    pool_class._carries = pool_class._carries + tuple(
        (carry["input_port"], carry["output_port"], carry["family"]) for carry in new_carries
    )
    pool_class._input_ports_written_at_setup = pool_class._input_ports_written_at_setup | frozenset(
        carry["input_port"] for carry in new_carries if carry.get("written_at_setup")
    )


class _FunctionNodePoolBase:
    """Reads each carried island and writes its value to the island's output."""

    _carries: "tuple[tuple[str, str, str], ...]"
    _input_ports_written_at_setup: "frozenset[str]"
    _link_data_access: NodeLinkDataAccess

    def setup(self, ctx: Any) -> None:
        for carry in self._carries:
            input_port = carry[0]
            if input_port not in self._input_ports_written_at_setup:
                continue
            try:
                self._write_one_carry(ctx, *carry)
            except Exception as island_failure:
                log.warn(
                    "an island carried into a setting raised as the pool set up, so its "
                    "reader is built with the setting's own value",
                    input_port=input_port,
                    error=str(island_failure),
                )

    def process(self, ctx: Any) -> None:
        for carry in self._carries:
            self._write_one_carry(ctx, *carry)

    def _write_one_carry(self, ctx: Any, input_port: str, output_port: str, carries: str) -> None:
        """Write the value of the island `input_port` reads to `output_port`; an island into a setting only once it changed."""
        if carries == _CARRIES_A_RECORD:
            record = ctx._link_input_data_reader.read(input_port, families=[_CARRIES_A_RECORD])
            if record is not None:
                ctx._link_output_data_writer.write(output_port, record, family=_CARRIES_A_RECORD)
            return
        message = read_the_message_at_an_input_port_unread(
            link_data_access=self._link_data_access, input_port=input_port
        )
        if message is None:
            return
        if input_port in self._input_ports_written_at_setup:
            written_last = self._messages_written_into_settings().get(input_port)
            if written_last is not None and an_unread_message_carries_what_another_does(
                message=message, other=written_last
            ):
                return
            self._messages_written_into_settings()[input_port] = message
        hand_an_unread_message_on(
            link_data_access=self._link_data_access,
            output_port=output_port,
            message=message,
        )

    def _messages_written_into_settings(self) -> "dict[str, Any]":
        """The message last written for each island carried into a setting, by input port."""
        written: "Optional[dict[str, Any]]" = getattr(self, "_last_message_written_into_a_setting", None)
        if written is None:
            written = {}
            self._last_message_written_into_a_setting = written
        return written
