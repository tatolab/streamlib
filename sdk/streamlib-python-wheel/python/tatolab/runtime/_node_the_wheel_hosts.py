# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What the wheel's own hosts reach on a node: the ports of a node it builds itself, the function
node pool or a function node run as a standalone node, stamped as the helper reads a node class's
ports, and the reader and writer a context keeps for the hosts that bind a node's pins.

A body the wheel builds reads and writes each port by name; the parent names its execution.
"""

from __future__ import annotations

from typing import Any, Sequence

from tatolab.stream import LinkInputDataReader, LinkOutputDataWriter
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

__all__ = [
    "NODE_THE_WHEEL_BUILDS_ITSELF",
    "NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG",
    "is_a_node_the_wheel_builds_itself",
    "link_input_data_reader_of",
    "link_output_data_writer_of",
    "stamp_the_ports_of_a_node_the_wheel_hosts",
]


NODE_THE_WHEEL_BUILDS_ITSELF = "_tatolab_built_by_the_wheel"
"""The attribute set on each node class the wheel builds itself, which no author declares."""


NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG = "_tatolab_takes_its_config_at_construction"
"""The attribute set on a node the wheel builds itself that is built from, and takes, the config
the parent hands it, as a function node run as a standalone node is."""


def is_a_node_the_wheel_builds_itself(node_class: type) -> bool:
    """Whether the wheel built `node_class` itself, rather than an author declaring it."""
    return vars(node_class).get(NODE_THE_WHEEL_BUILDS_ITSELF, False) is True


def stamp_the_ports_of_a_node_the_wheel_hosts(
    node_class: type, *, input_ports: "Sequence[str]", output_ports: "Sequence[str]"
) -> type:
    """Stamp `node_class` with `input_ports` and `output_ports`, in addition to any it holds,
    and as a node the wheel builds itself."""
    setattr(node_class, NODE_THE_WHEEL_BUILDS_ITSELF, True)
    for stamp, ports in (
        (NODE_DECLARATION_INPUT_PORTS_STAMP, input_ports),
        (NODE_DECLARATION_OUTPUT_PORTS_STAMP, output_ports),
    ):
        held = list(vars(node_class).get(stamp, []))
        setattr(node_class, stamp, [*held, *({"name": port} for port in ports)])
    return node_class


def link_input_data_reader_of(context: Any) -> LinkInputDataReader:
    """The reader `context` keeps for the wheel's hosts, which node code reads through its pins."""
    return context._link_input_data_reader


def link_output_data_writer_of(context: Any) -> LinkOutputDataWriter:
    """The writer `context` keeps for the wheel's hosts, which node code writes through its pins."""
    return context._link_output_data_writer
