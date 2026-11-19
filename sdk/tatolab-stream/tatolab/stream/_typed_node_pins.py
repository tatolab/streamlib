# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The pins a typed node declares, as `StreamBuilder` checks a node's input values and port names against them.

A typed node is one whose every port carries the `pin_declaration` bare `@node` reads, so its
declaration names each input's type. A node of the family grammar declares no such thing, and
the builder records what it is given unchecked.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Final, Literal, Optional

from ._contract_pin_types import BUILT_IN_PIN_TYPES, BuiltInPinTypeName, PinDeclarationOnTheWire
from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe
from ._node_declaration import NODE_DECLARATION_INPUT_PORTS_STAMP, NODE_DECLARATION_OUTPUT_PORTS_STAMP
from ._pin_declaration import PinValueNotTaken, stored_form_of_a_pin_value

__all__ = ["TypedNodePins"]

type PortDirection = Literal["input", "output"]

_PIN_DECLARATION_KEY: Final = "pin_declaration"

# The pin types whose pins hold a stored value, so a value set on an input of one is kept.
_PIN_TYPES_HOLDING_A_STORED_VALUE: Final[frozenset[BuiltInPinTypeName]] = frozenset(
    row.name for row in BUILT_IN_PIN_TYPES if row.value_component is not None
)


@dataclass(frozen=True, slots=True)
class _DeclaredPort:
    name: str
    declaration: PinDeclarationOnTheWire


@dataclass(frozen=True, slots=True)
class TypedNodePins:
    """The ports a typed node declares, each found by its name or a name it had."""

    node: str
    """The node type's short name, as a refusal names it."""

    _inputs: Mapping[str, _DeclaredPort]
    _outputs: Mapping[str, _DeclaredPort]

    @classmethod
    def of(cls, node_class: object, node: str) -> Optional[TypedNodePins]:
        """The pins `node_class` declares when it is a typed node; `None` when it is not."""
        input_ports: list[dict[str, Any]] = getattr(node_class, NODE_DECLARATION_INPUT_PORTS_STAMP, None) or []
        output_ports: list[dict[str, Any]] = getattr(node_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP, None) or []
        ports = [*input_ports, *output_ports]
        if not ports or not all(_PIN_DECLARATION_KEY in port for port in ports):
            return None
        return cls(node, _found_by_each_name(input_ports), _found_by_each_name(output_ports))

    def port_named(self, name: str, direction: PortDirection) -> str:
        """The name of the port of `direction` that `name`, cast, finds; refused naming the ports declared."""
        return self._declared_port(name, direction).name

    def stored_input_values(self, input_values: Mapping[str, Any]) -> dict[str, Any]:
        """`input_values` keyed by the inputs they set, each in the stored form its pin type holds."""
        stored: dict[str, Any] = {}
        for name, value in input_values.items():
            if not isinstance(name, str):
                raise TypeError(
                    f"`{self.node}` is set an input value under {name!r}, of type "
                    f"`{type(name).__name__}`, and an input is named by a str"
                )
            port = self._declared_port(name, "input")
            if port.name in stored:
                raise ValueError(
                    f"`{self.node}` is set input `{port.name}` twice, once as {name!r}: set "
                    f"each input once"
                )
            stored[port.name] = self._stored_value(port, value)
        return stored

    def _stored_value(self, port: _DeclaredPort, value: object) -> object:
        declaration = port.declaration
        pin_type_declared = declaration["pin_type"]
        if pin_type_declared["kind"] != "is":
            raise TypeError(
                f"`{self.node}`'s input `{port.name}` finds its type from its graph, so no value "
                f"is checked against it here. To fix: set it once the stream is placed"
            )
        pin_type = pin_type_declared["pin_type"]
        if pin_type["type"] not in _PIN_TYPES_HOLDING_A_STORED_VALUE:
            raise TypeError(
                f"`{self.node}`'s input `{port.name}` carries a stream and holds no value, so "
                f"it reads only what a link brings. To fix: link an output into it instead"
            )
        if value is None:
            if declaration.get("may_be_unset", False):
                return None
            raise TypeError(
                f"`{self.node}`'s input `{port.name}` is set `None`, and it may not be unset. "
                f"To fix: set it a value of its type, or leave it out to read its default"
            )
        try:
            return stored_form_of_a_pin_value(value, pin_type, declaration.get("annotations"))
        except PinValueNotTaken as not_taken:
            raise TypeError(
                f"`{self.node}`'s input `{port.name}` is set {not_taken.value!r}, "
                f"{not_taken.why}. To fix: set it a value its pin holds"
            ) from None

    def _declared_port(self, name: str, direction: PortDirection) -> _DeclaredPort:
        declared = self._inputs if direction == "input" else self._outputs
        try:
            port = declared.get(cast_exposed_name_to_url_safe(name))
        except ExposedNameCastsToNothingError:
            port = None
        if port is None:
            names = sorted({port.name for port in declared.values()})
            listed = ", ".join(f"`{port_name}`" for port_name in names) or "none"
            raise ValueError(
                f"`{self.node}` declares no {direction} `{name}`; its {direction}s are: {listed}"
            )
        return port


def _found_by_each_name(ports: list[dict[str, Any]]) -> dict[str, _DeclaredPort]:
    found: dict[str, _DeclaredPort] = {}
    for port in ports:
        declared = _DeclaredPort(port["name"], port[_PIN_DECLARATION_KEY])
        for name in (declared.name, *declared.declaration.get("former_names", [])):
            found[cast_exposed_name_to_url_safe(name)] = declared
    return found
