# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Turning a declared node class into a running node object.

The engine calls into this module rather than constructing the object itself: building a node
from its fields and pins is Python's job. App code never calls anything here.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Optional


from tatolab.runtime._node_the_wheel_hosts import NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG

if TYPE_CHECKING:
    from tatolab.stream._class_node_declaration import ClassNodeMember, ClassNodePinGroup
    from tatolab.stream._node_context_protocols import (
        LinkInputDataReader,
        LinkOutputDataWriter,
        NodeLinkDataAccess,
    )

    from tatolab.runtime._class_node_fields import ClassNodeFields

__all__ = [
    "class_node_members_of",
    "class_node_pin_groups_of",
    "construct_class_node_from_its_fields_and_pins",
    "the_members_of_each_slot_built",
    "the_slots_its_pin_groups_are_built_with",
    "construct_node_instance",
]


def class_node_members_of(node_class: type) -> "Optional[tuple[ClassNodeMember, ...]]":
    """The field or pin behind each port of `node_class` when it is a class under bare `@node`; `None` for any other node.

    Read through the `tatolab-stream` of the project's venv, which may predate class nodes
    built from their fields and pins; a node declared by one that does is never such a class.
    """
    try:
        from tatolab.stream._class_node_construction import class_node_members
    except ImportError:
        return None
    return class_node_members(node_class)


def class_node_pin_groups_of(node_class: type) -> "tuple[ClassNodePinGroup, ...]":
    """The `PinGroup`s of `node_class` when it is a class under bare `@node`; none for any other
    node, and for a node declared by a `tatolab-stream` that predates pin groups."""
    try:
        from tatolab.stream._class_node_construction import class_node_pin_groups
    except ImportError:
        return ()
    return class_node_pin_groups(node_class)


def the_slots_its_pin_groups_are_built_with(
    node_class: type, port_wiring: "dict[str, Any]"
) -> "tuple[dict[str, int], list[str], list[str]]":
    """How many slots each `PinGroup` of `node_class` is built with, by its name, read off the
    `ports` of its `setup` command, and the input and output ports of those slots; nothing for a
    node with no group."""
    pin_groups = class_node_pin_groups_of(node_class)
    if not pin_groups:
        return {}, [], []
    from tatolab.runtime._class_node_pin_group_slots import (
        the_ports_of_the_slots_built,
        the_slot_count_of_each_pin_group,
    )

    slot_counts = the_slot_count_of_each_pin_group(pin_groups, port_wiring)
    slot_inputs, slot_outputs = the_ports_of_the_slots_built(pin_groups, slot_counts)
    return slot_counts, slot_inputs, slot_outputs


def the_members_of_each_slot_built(
    pin_groups: "tuple[ClassNodePinGroup, ...]", slot_counts: "dict[str, int]"
) -> "dict[str, list[tuple[ClassNodeMember, ...]]]":
    """The members of each slot each of `pin_groups` is built with, by the group's name, each
    port named as the slot's, `layers.0.picture`."""
    return {
        pin_group.name: [pin_group.members_of_slot(slot) for slot in range(slot_counts[pin_group.name])]
        for pin_group in pin_groups
    }


def construct_class_node_from_its_fields_and_pins(
    node_class: type,
    members: "tuple[ClassNodeMember, ...]",
    fields: "ClassNodeFields",
    *,
    link_data_access: "NodeLinkDataAccess",
    inputs: "LinkInputDataReader",
    outputs: "LinkOutputDataWriter",
    node_id: str,
    members_of_each_slot: "Optional[dict[str, list[tuple[ClassNodeMember, ...]]]]" = None,
) -> Any:
    """Build a class under bare `@node`: every field holding the value `fields` builds it with, every pin bound, every pin group holding the slots `members_of_each_slot` names, then `__init__()`."""
    from tatolab.stream._class_node_construction import construct_class_node

    from tatolab.runtime._class_node_bound_pins import bind_the_pins_of_a_class_node

    def bound(each: "tuple[ClassNodeMember, ...]") -> "dict[str, Any]":
        return bind_the_pins_of_a_class_node(
            each, link_data_access=link_data_access, inputs=inputs, outputs=outputs, node_id=node_id
        )

    values_to_build_with = fields.values_to_build_with()
    if not members_of_each_slot:
        return construct_class_node(node_class, field_values=values_to_build_with, pins=bound(members))
    from tatolab.stream._class_node_construction import build_pin_group_slot, class_node_pin_groups

    pin_groups = {pin_group.name: pin_group for pin_group in class_node_pin_groups(node_class)}
    slot_ports = {
        member.port
        for slots in members_of_each_slot.values()
        for slot_members in slots
        for member in slot_members
    }
    slots_of_each_group: "dict[str, list[Any]]" = {}
    for name, slots in members_of_each_slot.items():
        pin_group = pin_groups[name]
        slots_of_each_group[name] = []
        for slot, slot_members in enumerate(slots):
            # Each port of the slot by its name in the slot, `opacity` for `layers.0.opacity`.
            in_the_slot = {
                slot_member.port: member.port
                for slot_member, member in zip(slot_members, pin_group.members)
            }
            pins = bound(slot_members)
            slots_of_each_group[name].append(
                build_pin_group_slot(
                    node_class,
                    pin_group,
                    field_values={
                        in_the_slot[port]: value
                        for port, value in values_to_build_with.items()
                        if port in in_the_slot
                    },
                    pins={in_the_slot[port]: pin for port, pin in pins.items()},
                )
            )
    return construct_class_node(
        node_class,
        field_values={
            port: value for port, value in values_to_build_with.items() if port not in slot_ports
        },
        pins=bound(members),
        pin_groups=slots_of_each_group,
    )


def construct_node_instance(node_class: type, configuration: Optional[Any]) -> Any:
    """Build a node the wheel builds itself: the function node pool, or a function node run as a
    standalone node, which alone is built from `configuration`.

    An author's node is a class under bare `@node`, built from its fields and pins instead.
    """
    if getattr(node_class, NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG, False):
        return node_class(_as_configuration_mapping(node_class, configuration))
    _refuse_a_configuration_with_nowhere_to_go(
        node_class, _as_configuration_mapping(node_class, configuration)
    )
    return node_class()


def _refuse_a_configuration_with_nowhere_to_go(
    node_class: type, configuration: "dict[str, Any]"
) -> None:
    """Refuse a configuration that sets anything on a node that takes none: a node takes each
    value as an input of its own."""
    if not configuration:
        return
    refused_key = next(iter(configuration))
    raise TypeError(
        f"{node_class.__name__} takes no config, so `{refused_key}` has nowhere to go. To fix: "
        f"set it as the input of its name, through `input_values`."
    )


def _as_configuration_mapping(
    node_class: type, configuration: Optional[Any]
) -> "dict[str, Any]":
    if configuration is None:
        return {}
    if not isinstance(configuration, dict):
        raise TypeError(
            f"config for {node_class.__name__} must be a dict, got "
            f"{type(configuration).__name__}"
        )
    return dict(configuration)
