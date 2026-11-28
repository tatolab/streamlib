# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Turning a declared node into a running node object.

The engine calls into this module rather than constructing the object itself: building a node
from its value inputs and pins is Python's job. App code never calls anything here.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Optional


from tatolab.runtime._node_the_wheel_hosts import NODE_THE_WHEEL_BUILDS_TAKES_ITS_CONFIG

if TYPE_CHECKING:
    from tatolab.stream._class_node_declaration import ClassNodeMember, ClassNodePinGroup

__all__ = [
    "class_node_pin_groups_of",
    "is_a_node_function",
    "the_members_of_each_slot_built",
    "the_slots_its_pin_groups_are_built_with",
    "construct_node_instance",
]


def is_a_node_function(node: object) -> bool:
    """Whether `node` is a function under `@node(...)`, a node written as one function.

    Read through the `tatolab-stream` of the project's venv, which may predate node functions;
    nothing one that does declares is one.
    """
    try:
        from tatolab.stream._node_function import NodeFunction
    except ImportError:
        return False
    return isinstance(node, NodeFunction)


def class_node_pin_groups_of(node_class: object) -> "tuple[ClassNodePinGroup, ...]":
    """The `PinGroup`s of `node_class` when it is a node function; none for any other node."""
    if is_a_node_function(node_class):
        return getattr(node_class, "pin_groups", ())
    return ()


def the_slots_its_pin_groups_are_built_with(
    node_class: object, port_wiring: "dict[str, Any]"
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


def construct_node_instance(node_class: type, configuration: Optional[Any]) -> Any:
    """Build a node the wheel builds itself: the function node pool, or a function node run as a
    standalone node, which alone is built from `configuration`.

    An author's node is written as one function, built from its value inputs and pins instead.
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
