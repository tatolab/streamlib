# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The ports of a node the wheel builds and hosts itself, the function node pool or a function
node run as a standalone node, stamped as the helper reads a node class's ports.

Its body reads and writes each port by name; the parent names its execution.
"""

from __future__ import annotations

from typing import Sequence

from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

__all__ = ["stamp_the_ports_of_a_node_the_wheel_hosts"]


def stamp_the_ports_of_a_node_the_wheel_hosts(
    node_class: type, *, input_ports: "Sequence[str]", output_ports: "Sequence[str]"
) -> type:
    """Stamp `node_class` with `input_ports` and `output_ports`, in addition to any it holds."""
    for stamp, ports in (
        (NODE_DECLARATION_INPUT_PORTS_STAMP, input_ports),
        (NODE_DECLARATION_OUTPUT_PORTS_STAMP, output_ports),
    ):
        held = list(vars(node_class).get(stamp, []))
        setattr(node_class, stamp, [*held, *({"name": port} for port in ports)])
    return node_class
