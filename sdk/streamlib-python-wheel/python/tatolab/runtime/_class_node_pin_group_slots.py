# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""How many slots each `PinGroup` of a class node is built with, read off its `setup` command.

The engine builds a node for the slots it holds: a group that follows its wires for each slot a
link or an island reaches, its empty slot drawn and never built, and a group whose count is set
for as many slots as its Integer input named after it holds, 1 until one is set. A change in
either rebuilds the node, so the count read here holds for the node's life.
"""

from __future__ import annotations

from typing import Any

from tatolab.stream._class_node_pin_group import (
    PIN_GROUP_SLOT_PORT_NAME_SEPARATOR,
    ClassNodePinGroup,
)

__all__ = ["the_ports_of_the_slots_built", "the_slot_count_of_each_pin_group"]

# The slots a group whose count is set holds until a count is set on it, as the engine declares
# its count input.
_SLOTS_OF_A_GROUP_WHOSE_COUNT_IS_NOT_SET = 1


def the_slot_count_of_each_pin_group(
    pin_groups: "tuple[ClassNodePinGroup, ...]", port_wiring: "dict[str, Any]"
) -> "dict[str, int]":
    """How many slots each of `pin_groups` is built with, by its name, from the `ports` of a
    `setup` command: past the last slot a link or an island reaches, or its count input's value."""
    reached = [
        *(link["name"] for link in port_wiring.get("inputs") or []),
        *(link["name"] for link in port_wiring.get("outputs") or []),
        *(island["input_port"] for island in port_wiring.get("islands") or []),
    ]
    input_values = port_wiring.get("input_values") or {}
    slot_counts: "dict[str, int]" = {}
    for pin_group in pin_groups:
        if pin_group.follows_its_wires:
            slots_reached = [
                slot
                for slot in (_the_slot_a_port_names(pin_group, port) for port in reached)
                if slot is not None
            ]
            slot_counts[pin_group.name] = max(slots_reached) + 1 if slots_reached else 0
        else:
            slot_counts[pin_group.name] = int(
                input_values.get(pin_group.name, _SLOTS_OF_A_GROUP_WHOSE_COUNT_IS_NOT_SET)
            )
    return slot_counts


def the_ports_of_the_slots_built(
    pin_groups: "tuple[ClassNodePinGroup, ...]", slot_counts: "dict[str, int]"
) -> "tuple[list[str], list[str]]":
    """The input and the output ports of every slot each of `pin_groups` is built with."""
    inputs: "list[str]" = []
    outputs: "list[str]" = []
    for pin_group in pin_groups:
        slot_inputs, slot_outputs = pin_group.slot_ports(slot_counts[pin_group.name])
        inputs.extend(slot_inputs)
        outputs.extend(slot_outputs)
    return inputs, outputs


def _the_slot_a_port_names(pin_group: ClassNodePinGroup, port: str) -> "int | None":
    """The slot of `pin_group` port `port` names, `layers.2.picture` naming 2; `None` for a port of
    no slot of it."""
    parts = port.split(PIN_GROUP_SLOT_PORT_NAME_SEPARATOR, 2)
    if len(parts) != 3 or parts[0] != pin_group.name or not parts[1].isdigit():
        return None
    if not any(member.port == parts[2] for member in pin_group.members):
        return None
    return int(parts[1])
