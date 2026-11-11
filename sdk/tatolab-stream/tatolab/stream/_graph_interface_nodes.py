# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`GraphInput` and `GraphOutput`: the ports a graph offers, each named and of one family."""

from __future__ import annotations

from typing import Any, ClassVar, NotRequired, Required, TypedDict

from ._built_in_node import BuiltInNode
from ._port_family_declaration import PortFamily

__all__ = [
    "GraphInput",
    "GraphInputConfig",
    "GraphOutput",
    "GraphOutputConfig",
]


class GraphOutputConfig(TypedDict):
    """The Output a `GraphOutput` declares."""

    port: Required[str]
    """The Output's name, cast as every exposed name is."""
    family: Required["PortFamily[Any] | str"]
    """The Output's family: `Image`, `Array`, `Channels`, `Packets` or `Records`."""


class GraphOutput(BuiltInNode[GraphOutputConfig]):
    """Declares one Output of the graph: the port linked into `value`. A stream's top-level
    Outputs are what Tato Studio displays. It runs nothing and sends nothing off the machine.

    Inputs:
        `value`: The port the Output offers, of the declared family.

    Config: `GraphOutputConfig`.
    """

    type: ClassVar[str] = "tatolab.stream:GraphOutput"


class GraphInputConfig(TypedDict):
    """The Input a `GraphInput` declares."""

    port: Required[str]
    """The Input's name, cast as every exposed name is."""
    family: Required["PortFamily[Any] | str"]
    """The Input's family: `Image`, `Array`, `Channels`, `Packets` or `Records`."""
    default: NotRequired[Any]
    """What the Input's readers see until a client writes it, of its family: a stream's
    top-level Array, Channels or Records Input only."""
    range: NotRequired[tuple[float, float]]
    """`(lowest, highest)`, the span a control moves the Input's number in: a stream's
    top-level Array or Channels Input only."""


class GraphInput(BuiltInNode[GraphInputConfig]):
    """Declares one Input of the graph: every port `value` is linked to reads what enters it.
    A stream's top-level Array, Channels or Records Input runs, and carries what a client
    writes with `write_graph_input`, else its `default`; a fragment's Input carries what the
    graph that adds the fragment wires into it.

    Outputs:
        `value`: What enters the graph, of the declared family.

    Config: `GraphInputConfig`.
    """

    type: ClassVar[str] = "tatolab.stream:GraphInput"
