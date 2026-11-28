# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node's `PinGroup[Slot]`: its slot, a frozen dataclass of pins and values, read as the engine declares a pin group.

Each field of the slot is read as a class node's own field or pin is, so its pin types, modes,
defaults and refusals are the same, and a refusal names the node and `<group>.<field>`. The
group is stamped as the engine reads one: its name, description, and the inputs and outputs of
one slot, each named as its field. Which slot count the group has follows from its slot, as the
engine finds it: one empty slot after the last wired one when the slot holds an input that is
no setting, else as many as the node's Integer input named after the group says.
"""

from __future__ import annotations

import dataclasses
import typing
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, Final, Optional

from ._class_node_pins import PinGroup
from ._node_declaration_refusal import NodeDeclarationRefusal

if typing.TYPE_CHECKING:
    from ._class_node_declaration import ClassNodeMember, DeclaredPort

__all__ = [
    "ClassNodePinGroup",
    "PIN_GROUP_SLOT_PORT_NAME_SEPARATOR",
    "a_pin_group_written_in",
    "read_a_pin_group",
    "refuse_a_pin_group_on_a_function_node",
]

PIN_GROUP_SLOT_PORT_NAME_SEPARATOR: Final = "."
"""What separates a group's name, a slot's index and a field's name in a slot's port, `layers.0.picture`."""

type DeclaredPinGroup = dict[str, Any]
"""One pin group as the engine reads it off a described node type."""

type SlotRead = tuple[list["DeclaredPort"], list["DeclaredPort"], tuple["ClassNodeMember", ...]]
"""The inputs, the outputs and the members one slot's fields declare."""


@dataclass(frozen=True, slots=True)
class ClassNodePinGroup:
    """One `PinGroup` of a class node: the attribute that holds its slots, and the slot it repeats."""

    attribute: str
    """The attribute's name, as the class spells it."""

    name: str
    """The group's name, the attribute's name cast, which each slot's port starts with."""

    slot_class: type
    """The frozen dataclass each slot is built as."""

    members: tuple[ClassNodeMember, ...]
    """The field or pin behind each port of one slot, each port named as its field."""

    declared: DeclaredPinGroup
    """The group as the engine reads it."""

    @property
    def follows_its_wires(self) -> bool:
        """Whether the group has one empty slot after the last wired one, rather than as many as
        the node's input named after it says: its slot holds an input that is no setting."""
        return any(
            port.get("input_mode", {}).get("mode") != "setting_only"
            for port in self.declared["inputs"]
        )

    def slot_port(self, slot: int, port: str) -> str:
        """The port of `port` in slot `slot`, `layers.0.picture`."""
        return f"{self.name}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{slot}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{port}"

    def members_of_slot(self, slot: int) -> tuple[ClassNodeMember, ...]:
        """The members of slot `slot`, each port named `layers.0.picture`, an output that follows
        an input of the slot following that slot's input."""
        slot_member_ports = {member.port for member in self.members}

        def of_this_slot(member: ClassNodeMember) -> ClassNodeMember:
            pin_declaration: Any = dict(member.pin_declaration)
            declared_pin_type = pin_declaration["pin_type"]
            if (
                declared_pin_type["kind"] == "same_as_input"
                and declared_pin_type["input_port_name"] in slot_member_ports
            ):
                pin_declaration["pin_type"] = {
                    **declared_pin_type,
                    "input_port_name": self.slot_port(slot, declared_pin_type["input_port_name"]),
                }
            return dataclasses.replace(
                member, port=self.slot_port(slot, member.port), pin_declaration=pin_declaration
            )

        return tuple(of_this_slot(member) for member in self.members)

    def slot_ports(self, slot_count: int) -> tuple[list[str], list[str]]:
        """The input and the output ports of `slot_count` slots, slot by slot."""
        inputs = [
            self.slot_port(slot, port["name"])
            for slot in range(slot_count)
            for port in self.declared["inputs"]
        ]
        outputs = [
            self.slot_port(slot, port["name"])
            for slot in range(slot_count)
            for port in self.declared["outputs"]
        ]
        return inputs, outputs


def a_pin_group_written_in(annotation: Any) -> bool:
    """Whether `annotation` is a `PinGroup`, bare or `PinGroup[Slot]`, inside any `Annotated[...]`."""
    written = annotation
    while typing.get_origin(written) is typing.Annotated:
        written = typing.get_args(written)[0]
    return (typing.get_origin(written) or written) is PinGroup


def read_a_pin_group(
    *,
    node: str,
    attribute_name: str,
    group_name: str,
    annotation: Any,
    description: Optional[str],
    declares_a_value: bool,
    read_the_slot: Callable[[type], SlotRead],
) -> ClassNodePinGroup:
    """The pin group `attribute_name: PinGroup[Slot]` of class node `node` declares, its slot read
    by `read_the_slot` as a class node's fields and pins are.

    Refuses, as a `NodeDeclarationRefusal`, a group given a value, one written with anything
    beside it inside its `Annotated[...]` but a `Description`, a bare `PinGroup`, a slot that is
    no frozen dataclass, a slot holding no pin or value, a group inside a slot, and each field
    of the slot as its own reading refuses it, named `<group>.<field>`.
    """

    def refusal(words: str, finding_code: Any, pin: str = attribute_name) -> NodeDeclarationRefusal:
        return NodeDeclarationRefusal(words, finding_code=finding_code, node=node, pin=pin)

    if declares_a_value:
        raise refusal(
            f"`{node}`'s pin group `{attribute_name}` is given a value, and a group holds the "
            f"slots its node is built with. To fix: drop the value.",
            "pin_has_a_value",
        )
    written = annotation
    if typing.get_origin(written) is typing.Annotated:
        written, *metadata = typing.get_args(written)
        raise refusal(
            f"`{node}`'s pin group `{attribute_name}` carries "
            f"{', '.join(f'`{entry!r}`' for entry in metadata)}, and a group takes only a "
            f"`Description`. To fix: state each pin's and each value's annotations on its "
            f"field of the slot.",
            "pin_annotation_not_taken",
        )
    slot_arguments = typing.get_args(written)
    if not slot_arguments:
        raise refusal(
            f"`{node}`'s pin group `{attribute_name}` is a bare `PinGroup`, so nothing names its "
            f"slot. To fix: `{attribute_name}: PinGroup[Layer]`, where `Layer` is a frozen "
            f"dataclass of the pins and values each slot holds.",
            "pin_group_slot_not_a_frozen_dataclass",
        )
    slot_class = slot_arguments[0]
    if not (
        isinstance(slot_class, type)
        and dataclasses.is_dataclass(slot_class)
        and slot_class.__dataclass_params__.frozen  # pyright: ignore[reportAttributeAccessIssue]
    ):
        raise refusal(
            f"`{node}`'s pin group `{attribute_name}` repeats {_named(slot_class)}, and a slot "
            f"is a frozen dataclass of pins and values. To fix: `@dataclass(frozen=True)` "
            f"on a class whose fields are pins, `picture: InputPin[Texture]`, and values with "
            f"their defaults, `opacity: float = 1.0`.",
            "pin_group_slot_not_a_frozen_dataclass",
        )
    _refuse_a_pin_group_inside_the_slot(node, attribute_name, slot_class)
    _refuse_a_field_its_slot_is_not_built_with(node, attribute_name, slot_class)
    try:
        inputs, outputs, members = read_the_slot(slot_class)
    except NodeDeclarationRefusal as slot_refusal:
        member = slot_refusal.pin
        raise refusal(
            f"`{node}`'s pin group `{attribute_name}`: {slot_refusal}",
            slot_refusal.finding_code,
            pin=attribute_name if member is None else f"{attribute_name}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{member}",
        ) from slot_refusal
    if not members:
        raise refusal(
            f"`{node}`'s pin group `{attribute_name}` repeats `{slot_class.__name__}`, which "
            f"holds no pin or value. To fix: give it a field for each pin and value one slot "
            f"holds, `picture: InputPin[Texture]`.",
            "pin_group_slot_holds_no_pin",
        )
    return ClassNodePinGroup(
        attribute=attribute_name,
        name=group_name,
        slot_class=slot_class,
        members=members,
        declared={
            "name": group_name,
            "description": description or "",
            "inputs": inputs,
            "outputs": outputs,
        },
    )


def refuse_a_pin_group_on_a_function_node(function_name: str, parameter: str, annotation: Any) -> None:
    """Refuse `parameter` of pure function node `function_name` when it is a `PinGroup`, which
    only a node function under `@node(...)` takes."""
    if not a_pin_group_written_in(annotation):
        return
    raise NodeDeclarationRefusal(
        f"`{function_name}` takes `{parameter}` as a `PinGroup`, and a pure function node's "
        f"inputs are fixed by its signature: only a node function under `@node(...)` takes a "
        f"pin group. "
        f"To fix: write it under `@node(...)`, its `{parameter}: PinGroup[...]` read with "
        f"`.get()`, or take a `list[...]` value.",
        finding_code="pin_group_on_a_function_node",
        node=function_name,
        pin=parameter,
    )


def _refuse_a_pin_group_inside_the_slot(node: str, attribute_name: str, slot_class: type) -> None:
    for field in dataclasses.fields(slot_class):
        if a_pin_group_written_in(_the_field_annotation(slot_class, field)):
            raise NodeDeclarationRefusal(
                f"`{node}`'s pin group `{attribute_name}` repeats `{slot_class.__name__}`, whose "
                f"field `{field.name}` is a `PinGroup`, and a slot holds pins and values, not a "
                f"group of its own. To fix: declare `{field.name}` as a second group on "
                f"`{node}`.",
                finding_code="pin_group_inside_a_slot",
                node=node,
                pin=f"{attribute_name}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{field.name}",
            )


def _refuse_a_field_its_slot_is_not_built_with(node: str, attribute_name: str, slot_class: type) -> None:
    for field in dataclasses.fields(slot_class):
        if not field.init:
            raise NodeDeclarationRefusal(
                f"`{node}`'s pin group `{attribute_name}` repeats `{slot_class.__name__}`, whose "
                f"field `{field.name}` is left out of its `__init__`, and each slot is built "
                f"with every pin and value it holds. To fix: drop `init=False` from "
                f"`{field.name}`, or make it a property of the slot.",
                finding_code="pin_group_slot_not_a_frozen_dataclass",
                node=node,
                pin=f"{attribute_name}{PIN_GROUP_SLOT_PORT_NAME_SEPARATOR}{field.name}",
            )


def _the_field_annotation(slot_class: type, field: dataclasses.Field[Any]) -> Any:
    try:
        return typing.get_type_hints(slot_class, include_extras=True).get(field.name, field.type)
    except Exception:
        return field.type


def _named(slot: Any) -> str:
    return f"`{slot.__name__}`" if isinstance(slot, type) else f"`{slot!r}`"
