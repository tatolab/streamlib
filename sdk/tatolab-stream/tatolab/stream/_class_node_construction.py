# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Building a class under bare `@node` the way its helper does: every field holding its value, every pin bound, then `__init__()`.

A node's helper calls `construct_class_node` inside its `setup` command, and a test calls it the
same way with a stand-in for each pin. A field and a pin are each found by the port name the
engine and a stored value give it, the attribute's name cast.
"""

from __future__ import annotations

import copy
from collections.abc import Mapping
from typing import Any, Optional

from ._class_node_declaration import ClassNodeMember
from ._node_declaration import NODE_DECLARATION_CLASS_NODE_MEMBERS_STAMP
from ._pin_declaration import NO_DEFAULT

__all__ = ["assign_live_field_values", "class_node_members", "construct_class_node"]


def class_node_members(node_class: object) -> Optional[tuple[ClassNodeMember, ...]]:
    """The field or pin behind each port of `node_class` when it is a class under bare `@node`; `None` for any other node."""
    members = getattr(node_class, NODE_DECLARATION_CLASS_NODE_MEMBERS_STAMP, None)
    return members if isinstance(members, tuple) else None


def construct_class_node(
    node_class: type,
    *,
    field_values: Mapping[str, object],
    pins: Mapping[str, object],
) -> Any:
    """`node_class` built in its helper's order: made, every field assigned, every pin bound, then `__init__()`.

    `field_values` holds the value of each field by its port's name, in its Python form; a field it
    leaves out holds a copy of its default. `pins` holds the object each pin reads or writes
    through, by its port's name, and names every pin. A field with no default and no value, a
    pin with no object, and a name that is no field or no pin are refused with a `ValueError`
    naming them. What `__init__()` raises reaches the caller.
    """
    members = _the_members_of(node_class)
    fields = {member.port: member for member in members if member.form == "field"}
    pin_members = {member.port: member for member in members if member.form != "field"}
    _refuse_names_it_does_not_declare(node_class, "field", field_values, fields)
    _refuse_names_it_does_not_declare(node_class, "pin", pins, pin_members)
    unbound = sorted(set(pin_members) - set(pins))
    if unbound:
        raise ValueError(
            f"`{node_class.__name__}` is built with no object for its pin "
            f"{', '.join(f'`{port}`' for port in unbound)}; every pin is bound before `__init__()`"
        )
    node_instance = object.__new__(node_class)
    for port, member in fields.items():
        if port in field_values:
            value = field_values[port]
        elif member.default is not NO_DEFAULT:
            value = copy.deepcopy(member.default)
        else:
            raise ValueError(
                f"`{node_class.__name__}`'s field `{member.attribute}` has no default and was "
                f"given no value"
            )
        setattr(node_instance, member.attribute, value)
    for port, member in pin_members.items():
        setattr(node_instance, member.attribute, pins[port])
    node_instance.__init__()
    return node_instance


def assign_live_field_values(node_instance: object, field_values: Mapping[str, object]) -> None:
    """Assign each of `field_values`, by its port's name and in its Python form, to the field it names on a running node.

    A setting is left as it is, since a change to one rebuilds the node, and a name that is no
    field is refused with a `ValueError` before anything is assigned.
    """
    node_class = type(node_instance)
    fields = {member.port: member for member in _the_members_of(node_class) if member.form == "field"}
    _refuse_names_it_does_not_declare(node_class, "field", field_values, fields)
    for port, value in field_values.items():
        member = fields[port]
        if not member.a_setting:
            setattr(node_instance, member.attribute, value)


def _the_members_of(node_class: type) -> tuple[ClassNodeMember, ...]:
    members = class_node_members(node_class)
    if members is None:
        raise TypeError(
            f"`{node_class.__name__}` is not a class under bare `@node`, so it has no fields and "
            f"pins to build it from"
        )
    return members


def _refuse_names_it_does_not_declare(
    node_class: type,
    what: str,
    named: Mapping[str, object],
    declared: Mapping[str, ClassNodeMember],
) -> None:
    undeclared = sorted(set(named) - set(declared))
    if undeclared:
        raise ValueError(
            f"`{node_class.__name__}` declares no {what} "
            f"{', '.join(f'`{port}`' for port in undeclared)}; its {what}s are "
            f"{', '.join(f'`{port}`' for port in declared) or 'none'}"
        )
