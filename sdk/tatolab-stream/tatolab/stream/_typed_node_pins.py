# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The pins a typed node declares, as `StreamBuilder` checks a node's input values and port names against them.

A typed node is one whose every port carries the `pin_declaration` bare `@node` reads, so its
declaration names each input's type. A node whose ports declare no type is recorded as it is
given, unchecked. A port of a slot of a node's pin group,
`layers.2.picture`, is found as the slot's field it names, in any slot.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Final, Literal, Optional

from ._contract_pin_types import (
    BUILT_IN_PIN_TYPES,
    BuiltInPinTypeName,
    PinDeclarationOnTheWire,
    PinType,
)
from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe
from ._class_node_pin_group import PIN_GROUP_SLOT_PORT_NAME_SEPARATOR
from ._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
    NODE_DECLARATION_PIN_GROUPS_STAMP,
)
from ._function_node_declaration import NODE_DECLARATION_RUST_CONFIG_SCHEMA_STAMP
from ._pin_declaration import PinValueNotTaken, stored_form_of_a_pin_value
from ._pin_type_choice import pin_type_in_words

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
    _slot_inputs_by_group: Mapping[str, Mapping[str, _DeclaredPort]]
    _slot_outputs_by_group: Mapping[str, Mapping[str, _DeclaredPort]]
    _config_field_names: frozenset[str]
    """The fields of a Rust function node's config, whose values its native half checks."""

    @classmethod
    def of(cls, node_class: object, node: str) -> Optional[TypedNodePins]:
        """The pins `node_class` declares when it is a typed node; `None` when it is not."""
        input_ports: list[dict[str, Any]] = getattr(node_class, NODE_DECLARATION_INPUT_PORTS_STAMP, None) or []
        output_ports: list[dict[str, Any]] = getattr(node_class, NODE_DECLARATION_OUTPUT_PORTS_STAMP, None) or []
        pin_groups: list[dict[str, Any]] = getattr(node_class, NODE_DECLARATION_PIN_GROUPS_STAMP, None) or []
        slot_ports = [port for group in pin_groups for port in (*group["inputs"], *group["outputs"])]
        ports = [*input_ports, *output_ports, *slot_ports]
        if not ports or not all(_PIN_DECLARATION_KEY in port for port in ports):
            return None
        config_schema: Optional[dict[str, Any]] = getattr(
            node_class, NODE_DECLARATION_RUST_CONFIG_SCHEMA_STAMP, None
        )
        return cls(
            node,
            _found_by_each_name(input_ports),
            _found_by_each_name(output_ports),
            {group["name"]: _found_by_each_name(group["inputs"]) for group in pin_groups},
            {group["name"]: _found_by_each_name(group["outputs"]) for group in pin_groups},
            frozenset(config_schema.get("properties", {})) if config_schema is not None else frozenset(),
        )

    def port_named(self, name: str, direction: PortDirection) -> str:
        """The name of the port of `direction` that `name`, cast, finds; refused naming the ports declared."""
        return self._declared_port(name, direction).name

    def member_named(
        self,
        port_name: str,
        direction: PortDirection,
        member: str,
        input_values: Mapping[str, Any],
    ) -> str:
        """`member`, when the bundle the port of `direction` named `port_name` carries has it.

        A port whose type another input chooses carries the bundle `input_values`, as recorded,
        holds for that input; a port typed by its graph keeps `member` unchecked, for the runtime
        to judge at load. Refused naming the bundle's members, or the type the port carries when
        it is no bundle.
        """
        port = self._declared_port(port_name, direction)
        pin_type_declared = port.declaration["pin_type"]
        if pin_type_declared["kind"] == "is":
            pin_type: PinType = pin_type_declared["pin_type"]
        elif pin_type_declared["kind"] == "set_by_input":
            choosing_input = pin_type_declared["input_name"]
            chosen = input_values.get(choosing_input)
            if chosen is None:
                raise ValueError(
                    f"`{self.node}`'s {direction} `{port.name}` carries the bundle "
                    f"`{choosing_input}` chooses, and `{choosing_input}` is not set, so it has no "
                    f"member `{member}`. To fix: set `{choosing_input}` a `PinTypeChoice` of the "
                    f"bundle"
                )
            pin_type = chosen
        else:
            return member
        if pin_type["type"] != "bundle":
            raise TypeError(
                f"`{self.node}`'s {direction} `{port.name}` carries the type "
                f"{pin_type_in_words(pin_type)}, not a bundle, so it has no member `{member}`. To fix: "
                f"wire the whole port"
            )
        member_names = [declared["name"] for declared in pin_type["members"]]
        if member not in member_names:
            listed = ", ".join(f"`{name}`" for name in member_names)
            raise ValueError(
                f"`{self.node}`'s {direction} `{port.name}` carries a bundle with no member "
                f"`{member}`; its members are: {listed}"
            )
        return member

    def stored_input_values(self, input_values: Mapping[str, Any]) -> dict[str, Any]:
        """`input_values` keyed by the inputs they set, each in the stored form its pin type holds.

        A value on an input whose type another input chooses is checked against the type
        `input_values` chooses for it, and kept as given when they choose none. A value naming a
        field of a Rust function node's config is kept as given, for its native half to check.
        """
        ports_and_values: list[tuple[_DeclaredPort, object]] = []
        config_values: dict[str, Any] = {}
        for name, value in input_values.items():
            if not isinstance(name, str):
                raise TypeError(
                    f"`{self.node}` is set an input value under {name!r}, of type "
                    f"`{type(name).__name__}`, and an input is named by a str"
                )
            if name in self._config_field_names:
                config_values[name] = value
                continue
            port = self._declared_port(name, "input")
            if any(set_port.name == port.name for set_port, _ in ports_and_values):
                raise ValueError(
                    f"`{self.node}` is set input `{port.name}` twice, once as {name!r}: set "
                    f"each input once"
                )
            ports_and_values.append((port, value))
        stored: dict[str, Any] = {}
        for port, value in ports_and_values:
            if port.declaration["pin_type"]["kind"] != "set_by_input":
                stored[port.name] = self._stored_value(port, value)
        for port, value in ports_and_values:
            pin_type_declared = port.declaration["pin_type"]
            if pin_type_declared["kind"] == "set_by_input":
                stored[port.name] = self._stored_value(
                    port, value, chosen=stored.get(pin_type_declared["input_name"])
                )
        return {**{port.name: stored[port.name] for port, _ in ports_and_values}, **config_values}

    def _stored_value(self, port: _DeclaredPort, value: object, *, chosen: object = None) -> object:
        declaration = port.declaration
        pin_type_declared = declaration["pin_type"]
        if pin_type_declared["kind"] == "set_by_input":
            if chosen is None:
                return value
            pin_type: PinType = chosen  # pyright: ignore[reportAssignmentType]
        elif pin_type_declared["kind"] != "is":
            raise TypeError(
                f"`{self.node}`'s input `{port.name}` finds its type from its graph, so no value "
                f"is checked against it here. To fix: set it once the stream is placed"
            )
        else:
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
            port = self._slot_port(name, direction)
        if port is None:
            slot_ports = (
                self._slot_inputs_by_group if direction == "input" else self._slot_outputs_by_group
            )
            names = sorted(
                {port.name for port in declared.values()}
                | {
                    f"{group}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}<slot>"
                    f"{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{port.name}"
                    for group, ports in slot_ports.items()
                    for port in ports.values()
                }
            )
            listed = ", ".join(f"`{port_name}`" for port_name in names) or "none"
            raise ValueError(
                f"`{self.node}` declares no {direction} `{name}`; its {direction}s are: {listed}"
            )
        return port


    def _slot_port(self, name: str, direction: PortDirection) -> Optional[_DeclaredPort]:
        """The port `name` finds in a slot of a pin group, `layers.2.picture` the field `picture`
        of slot 2 of `layers`; `None` when it names none."""
        parts = name.split(PIN_GROUP_SLOT_PORT_NAME_SEPARATOR, 2)
        if len(parts) != 3:
            return None
        group, slot, field_name = parts
        if not slot.isdigit() or (slot != "0" and slot.startswith("0")):
            return None
        slot_ports = self._slot_inputs_by_group if direction == "input" else self._slot_outputs_by_group
        try:
            field_port = slot_ports.get(group, {}).get(cast_exposed_name_to_url_safe(field_name))
        except ExposedNameCastsToNothingError:
            return None
        if field_port is None:
            return None
        return _DeclaredPort(
            f"{group}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{slot}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{field_port.name}",
            field_port.declaration,
        )


def _found_by_each_name(ports: list[dict[str, Any]]) -> dict[str, _DeclaredPort]:
    found: dict[str, _DeclaredPort] = {}
    for port in ports:
        declared = _DeclaredPort(port["name"], port[_PIN_DECLARATION_KEY])
        for name in (declared.name, *declared.declaration.get("former_names", [])):
            found[cast_exposed_name_to_url_safe(name)] = declared
    return found
