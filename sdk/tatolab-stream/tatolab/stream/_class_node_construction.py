# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Building a class under bare `@node` the way its helper does: every field holding its value, every pin bound, then `__init__()`.

A node's helper calls `construct_class_node` inside its `setup` command, and a test calls it the
same way with a stand-in for each pin. A field and a pin are each found by the port name the
engine and a stored value give it, the attribute's name cast. A `PinGroup` holds the slots it is
built with, each built by `build_pin_group_slot` from its own fields and pins, and a value set
on a slot's field, `layers.0.opacity`, gives the group that slot again with the value.
"""

from __future__ import annotations

import copy
import dataclasses
from collections.abc import Mapping, Sequence
from typing import Any, Optional

from ._class_node_declaration import ClassNodeMember
from ._class_node_pin_group import PIN_GROUP_SLOT_PORT_NAME_SEPARATOR, ClassNodePinGroup
from ._node_declaration import (
    NODE_DECLARATION_CLASS_NODE_MEMBERS_STAMP,
    NODE_DECLARATION_CLASS_NODE_PIN_GROUPS_STAMP,
)
from ._pin_declaration import NO_DEFAULT

__all__ = [
    "assign_live_field_values",
    "build_pin_group_slot",
    "class_node_members",
    "class_node_pin_groups",
    "construct_class_node",
]


def class_node_members(node_class: object) -> Optional[tuple[ClassNodeMember, ...]]:
    """The field or pin behind each port of `node_class` when it is a class under bare `@node`; `None` for any other node."""
    members = getattr(node_class, NODE_DECLARATION_CLASS_NODE_MEMBERS_STAMP, None)
    return members if isinstance(members, tuple) else None


def class_node_pin_groups(node_class: object) -> tuple[ClassNodePinGroup, ...]:
    """The `PinGroup`s `node_class` declares, in declaration order; none for any other node."""
    pin_groups = getattr(node_class, NODE_DECLARATION_CLASS_NODE_PIN_GROUPS_STAMP, None)
    return pin_groups if isinstance(pin_groups, tuple) else ()


def build_pin_group_slot(
    node_class: type,
    pin_group: ClassNodePinGroup,
    *,
    field_values: Mapping[str, object],
    pins: Mapping[str, object],
) -> Any:
    """One slot of `pin_group`, its dataclass built with each field holding its value and each pin bound.

    `field_values` and `pins` are by each port's name in the slot, `opacity`, and are refused as
    `construct_class_node` refuses a node's own.
    """
    what = f"`{node_class.__name__}`'s pin group `{pin_group.name}`"
    return pin_group.slot_class(
        **_the_attribute_values(what, pin_group.members, field_values=field_values, pins=pins)
    )


def construct_class_node(
    node_class: type,
    *,
    field_values: Mapping[str, object],
    pins: Mapping[str, object],
    pin_groups: Mapping[str, Sequence[object]] = {},
) -> Any:
    """`node_class` built in its helper's order: made, every field assigned, every pin bound, every pin group holding its slots, then `__init__()`.

    `field_values` holds the value of each field by its port's name, in its Python form; a field it
    leaves out holds a copy of its default. `pins` holds the object each pin reads or writes
    through, by its port's name, and names every pin. `pin_groups` holds the slots of each
    `PinGroup`, by its name, in order, and names every group. A field with no default and no
    value, a pin with no object, a group with no slots, and a name that is no field, no pin or no
    group are refused with a `ValueError` naming them. What `__init__()` raises reaches the
    caller.
    """
    groups = {pin_group.name: pin_group for pin_group in class_node_pin_groups(node_class)}
    _refuse_names_it_does_not_declare(f"`{node_class.__name__}`", "pin group", pin_groups, groups)
    unbuilt = sorted(set(groups) - set(pin_groups))
    if unbuilt:
        raise ValueError(
            f"`{node_class.__name__}` is built with no slots for its pin group "
            f"{', '.join(f'`{name}`' for name in unbuilt)}; every group holds its slots before "
            f"`__init__()`"
        )
    node_instance = object.__new__(node_class)
    for attribute, value in _the_attribute_values(
        f"`{node_class.__name__}`", _the_members_of(node_class), field_values=field_values, pins=pins
    ).items():
        setattr(node_instance, attribute, value)
    for name, pin_group in groups.items():
        setattr(node_instance, pin_group.attribute, tuple(pin_groups[name]))
    node_instance.__init__()
    return node_instance


def _the_attribute_values(
    what: str,
    members: tuple[ClassNodeMember, ...],
    *,
    field_values: Mapping[str, object],
    pins: Mapping[str, object],
) -> dict[str, object]:
    """The value of each of `members` by its attribute: a field's value or a copy of its default,
    and a pin's object."""
    fields = {member.port: member for member in members if member.form == "field"}
    pin_members = {member.port: member for member in members if member.form != "field"}
    _refuse_names_it_does_not_declare(what, "field", field_values, fields)
    _refuse_names_it_does_not_declare(what, "pin", pins, pin_members)
    unbound = sorted(set(pin_members) - set(pins))
    if unbound:
        raise ValueError(
            f"{what} is built with no object for its pin "
            f"{', '.join(f'`{port}`' for port in unbound)}; every pin is bound before `__init__()`"
        )
    values: dict[str, object] = {}
    for port, member in fields.items():
        if port in field_values:
            values[member.attribute] = field_values[port]
        elif member.default is not NO_DEFAULT:
            values[member.attribute] = copy.deepcopy(member.default)
        else:
            raise ValueError(f"{what}'s field `{member.attribute}` has no default and was given no value")
    for port, member in pin_members.items():
        values[member.attribute] = pins[port]
    return values


def assign_live_field_values(node_instance: object, field_values: Mapping[str, object]) -> None:
    """Assign each of `field_values`, by its port's name and in its Python form, to the field it names on a running node.

    A field of a slot, `layers.0.opacity`, gives its group that slot again holding the value. A
    setting is left as it is, since a change to one rebuilds the node, and a name that is no
    field is refused with a `ValueError` before anything is assigned.
    """
    node_class = type(node_instance)
    fields = {member.port: member for member in _the_members_of(node_class) if member.form == "field"}
    slot_fields = _the_fields_of_each_slot_it_holds(node_instance)
    _refuse_names_it_does_not_declare(
        f"`{node_class.__name__}`", "field", field_values, {**fields, **slot_fields}
    )
    # The values given each slot, by its group's attribute and its index.
    slots_given: dict[tuple[str, int], dict[str, object]] = {}
    for port, value in field_values.items():
        if port in slot_fields:
            pin_group, slot, member = _the_slot_field_named(node_instance, port)
            if not member.a_setting:
                slots_given.setdefault((pin_group.attribute, slot), {})[member.attribute] = value
            continue
        member = fields[port]
        if not member.a_setting:
            setattr(node_instance, member.attribute, value)
    for (attribute, slot), values in slots_given.items():
        slots = list(getattr(node_instance, attribute))
        slots[slot] = dataclasses.replace(slots[slot], **values)
        setattr(node_instance, attribute, tuple(slots))


def _the_fields_of_each_slot_it_holds(node_instance: object) -> dict[str, ClassNodeMember]:
    """Each field of each slot `node_instance`'s groups hold, by its port, `layers.0.opacity`."""
    return {
        pin_group.slot_port(slot, member.port): member
        for pin_group in class_node_pin_groups(type(node_instance))
        for slot in range(len(getattr(node_instance, pin_group.attribute, ())))
        for member in pin_group.members
        if member.form == "field"
    }


def _the_slot_field_named(
    node_instance: object, port: str
) -> tuple[ClassNodePinGroup, int, ClassNodeMember]:
    group_name, slot, field_port = port.split(PIN_GROUP_SLOT_PORT_NAME_SEPARATOR, 2)
    pin_group = next(
        each for each in class_node_pin_groups(type(node_instance)) if each.name == group_name
    )
    member = next(member for member in pin_group.members if member.port == field_port)
    return pin_group, int(slot), member


def _the_members_of(node_class: type) -> tuple[ClassNodeMember, ...]:
    members = class_node_members(node_class)
    if members is None:
        raise TypeError(
            f"`{node_class.__name__}` is not a class under bare `@node`, so it has no fields and "
            f"pins to build it from"
        )
    return members


def _refuse_names_it_does_not_declare(
    declarer: str,
    what: str,
    named: Mapping[str, object],
    declared: Mapping[str, object],
) -> None:
    undeclared = sorted(set(named) - set(declared))
    if undeclared:
        raise ValueError(
            f"{declarer} declares no {what} "
            f"{', '.join(f'`{port}`' for port in undeclared)}; its {what}s are "
            f"{', '.join(f'`{port}`' for port in declared) or 'none'}"
        )
