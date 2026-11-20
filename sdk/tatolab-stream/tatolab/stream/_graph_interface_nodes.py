# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`GraphInput` and `GraphOutput`: the ports a graph offers, each named, and of the type its
`pin_type` setting chooses or the family its `family` names."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Annotated, Any, ClassVar, Final, Literal

from ._built_in_node import BuiltInNode
from ._contract_pin_types import PinDeclarationOnTheWire, PinType
from ._formerly_named import FormerlyNamed
from ._input_declaration_annotations import SettingOnly
from ._node_declaration import NODE_DECLARATION_INPUT_PORTS_STAMP, NODE_DECLARATION_OUTPUT_PORTS_STAMP
from ._pin_type_choice import PinTypeChoice
from ._port_family_declaration import PortFamily

__all__ = [
    "GraphInput",
    "GraphOutput",
]

_FAMILY_NAMES: Final = ["image", "array", "channels", "packets", "records"]


def _setting(name: str, pin_type: PinType, *, may_be_unset: bool) -> dict[str, Any]:
    declaration: PinDeclarationOnTheWire = {
        "pin_type": {"kind": "is", "pin_type": pin_type},
        "flow": "value",
        "may_be_unset": may_be_unset,
    }
    return {"name": name, "pin_declaration": declaration}


def _value_port(former_names: list[str]) -> dict[str, Any]:
    declaration: PinDeclarationOnTheWire = {
        "pin_type": {"kind": "set_by_input", "input_name": "pin_type"},
        "flow": "value",
    }
    if former_names:
        declaration["former_names"] = former_names
    return {"name": "value", "pin_declaration": declaration}


_PORT: Final = _setting("port", {"type": "string"}, may_be_unset=False)
_PIN_TYPE: Final = _setting("pin_type", {"type": "pin_type_choice"}, may_be_unset=True)
_FAMILY: Final = _setting("family", {"type": "menu", "option_names": _FAMILY_NAMES}, may_be_unset=True)


class GraphOutput(BuiltInNode[Mapping[str, Any]]):
    """Declares one Output of the graph: the port linked into `value`. A stream's top-level
    Outputs are what Tato Studio displays. It runs nothing and sends nothing off the machine.

    Inputs:
        `value`: The port the Output offers, of the type `pin_type` chooses.
    """

    type: ClassVar[str] = "tatolab.stream:GraphOutput"

    port: Annotated[str, SettingOnly()]
    """The Output's name, cast as every exposed name is."""
    pin_type: PinTypeChoice | None = None
    """The type of what leaves the Output, any type. Unset, `family` decides it."""
    family: Annotated[PortFamily[Any] | str, SettingOnly()] | None = None
    """The Output's family while no `pin_type` is chosen: `Image`, `Array`, `Channels`,
    `Packets` or `Records`. Left out with it, the Output takes the family of the port linked
    into it, and runs nothing until one is."""


setattr(GraphOutput, NODE_DECLARATION_INPUT_PORTS_STAMP, [_PORT, _PIN_TYPE, _FAMILY, _value_port([])])
setattr(GraphOutput, NODE_DECLARATION_OUTPUT_PORTS_STAMP, [])


class GraphInput(BuiltInNode[Mapping[str, Any]]):
    """Declares one Input of the graph: every port `value` is linked to reads what enters it.
    A stream's top-level Input of a type that holds a value, or of the Array, Channels or
    Records family, runs, and carries the value a client sets with `set_node_input_value`
    naming the Input and no node, kept as its `lifetime` says, else its saved `value`; a
    fragment's Input carries what the graph that adds the fragment wires into it.

    Outputs:
        `value`: What enters the graph, of the type `pin_type` chooses.
    """

    type: ClassVar[str] = "tatolab.stream:GraphInput"

    port: Annotated[str, SettingOnly()]
    """The Input's name, cast as every exposed name is."""
    pin_type: PinTypeChoice | None = None
    """The type of what enters the Input, any type. Unset, `family` decides it."""
    family: Annotated[PortFamily[Any] | str, SettingOnly()] | None = None
    """The Input's family while no `pin_type` is chosen: `Image`, `Array`, `Channels`,
    `Packets` or `Records`. Left out with it, the Input takes the family of its first
    reader's port, and runs nothing until one is linked."""
    value: Annotated[object, FormerlyNamed("default")] | None = None
    """What the Input's readers see until a client sets it, of the type `pin_type` chooses:
    a stream's top-level Input only. Its name was `default`."""
    range: Annotated[list[float], SettingOnly()] | None = None
    """`[lowest, highest]`, the span a control moves the Input's number in: a stream's
    top-level Input of a number only."""
    lifetime: Annotated[Literal["kept", "reset"], SettingOnly()] = "kept"
    """How long a value a client sets lasts: `kept` keeps it in the project's store through
    Stop, Play and a restart; `reset` returns to the saved `value` at Stop. A stream's
    top-level Input only."""


setattr(
    GraphInput,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    [
        _PORT,
        _PIN_TYPE,
        _FAMILY,
        _setting("range", {"type": "list_of", "element_type": {"type": "float"}}, may_be_unset=True),
        _setting("lifetime", {"type": "menu", "option_names": ["kept", "reset"]}, may_be_unset=False),
        _value_port(["default"]),
    ],
)
setattr(GraphInput, NODE_DECLARATION_OUTPUT_PORTS_STAMP, [_value_port([])])
