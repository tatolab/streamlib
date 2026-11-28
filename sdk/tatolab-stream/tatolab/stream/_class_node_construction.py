# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Building one slot of a node function's `PinGroup` the way its helper does: each field holding
its value and each pin bound.

A field and a pin are each found by the port name the engine and a stored value give it, the
attribute's name cast.
"""

from __future__ import annotations

import copy
from collections.abc import Mapping
from typing import TYPE_CHECKING, Any

from ._class_node_declaration import ClassNodeMember
from ._class_node_pin_group import ClassNodePinGroup
from ._pin_declaration import NO_DEFAULT

if TYPE_CHECKING:
    from ._node_function import NodeFunction

__all__ = ["build_pin_group_slot"]


def build_pin_group_slot(
    node_function: NodeFunction,
    pin_group: ClassNodePinGroup,
    *,
    field_values: Mapping[str, object],
    pins: Mapping[str, object],
) -> Any:
    """One slot of `pin_group`, its dataclass built with each field holding its value and each pin bound.

    `field_values` and `pins` are by each port's name in the slot, `opacity`. A field with no
    default and no value, a pin with no object, and a name that is no field or no pin are
    refused with a `ValueError` naming them.
    """
    what = f"`{node_function.__name__}`'s pin group `{pin_group.name}`"
    return pin_group.slot_class(
        **_the_attribute_values(what, pin_group.members, field_values=field_values, pins=pins)
    )


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
            f"{', '.join(f'`{port}`' for port in unbound)}; every pin is bound before the node runs"
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
