# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The slots each `PinGroup` of a node function holds while it plays, followed from its wiring.

The engine shapes a node's groups from its links and its values: a group that follows its wires
up to the last slot a link or an island reaches, its empty slot drawn and never built, and a
group whose count is set as many as its count input says, which the engine never lets leave
out a linked slot. A node function's helper follows the same wiring as it changes, so a link wired
into a new slot, the last slot's link taken out or a count set gives the node its slots while
it plays, and nothing rebuilds it: each new slot's ports are declared, its fields held, and the
group's input in the node's scope written with the slots it holds now.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Mapping

from tatolab.runtime._class_node_pin_group_slots import (
    the_ports_of_the_slots_built,
    the_slot_count_of_each_pin_group,
)

if TYPE_CHECKING:
    from tatolab.stream._class_node_declaration import ClassNodePinGroup
    from tatolab.stream._node_context_protocols import NodeLinkDataAccess

    from tatolab.runtime._class_node_fields import ClassNodeFields
    from tatolab.runtime._node_function_running import NodeFunctionRunning

__all__ = ["NodeFunctionPinGroupSlotsFollowed"]


class NodeFunctionPinGroupSlotsFollowed:
    """The links, islands and values a node function's pin groups are shaped by, followed from
    its `setup` command on, and the slots they give each group."""

    def __init__(
        self,
        pin_groups: "tuple[ClassNodePinGroup, ...]",
        port_wiring: "Mapping[str, Any]",
        *,
        link_data_access: "NodeLinkDataAccess",
    ) -> None:
        self._pin_groups = pin_groups
        self._link_data_access = link_data_access
        self._port_of_each_link: "dict[str, str]" = {}
        for link in [*(port_wiring.get("inputs") or []), *(port_wiring.get("outputs") or [])]:
            self._port_of_each_link[link["link_id"]] = link["name"]
        for island in port_wiring.get("islands") or []:
            self._port_of_each_link[island["link_id"]] = island["input_port"]
        self._input_values: "dict[str, Any]" = dict(port_wiring.get("input_values") or {})
        # The slots of the `setup` command, whose ports the helper declares as it sets up.
        slot_inputs, slot_outputs = the_ports_of_the_slots_built(pin_groups, self.slot_counts())
        self._ports_declared: "set[str]" = {*slot_inputs, *slot_outputs}

    def slot_counts(self) -> "dict[str, int]":
        """How many slots each group holds by the wiring followed, by its name."""
        return the_slot_count_of_each_pin_group(
            self._pin_groups,
            {
                "inputs": [{"name": port} for port in self._port_of_each_link.values()],
                "input_values": self._input_values,
            },
        )

    def note_a_link_wired(self, link_id: str, port: str) -> None:
        """Follow link or island `link_id`, which reaches port `port` of the node."""
        self._port_of_each_link[link_id] = port

    def note_a_link_unwired(self, link_id: str) -> None:
        """Stop following link or island `link_id`."""
        self._port_of_each_link.pop(link_id, None)

    def note_the_input_values(self, input_values: "Mapping[str, Any]") -> None:
        """Follow `input_values`, every value the node's unlinked inputs read."""
        self._input_values = dict(input_values)

    def give_the_node_its_slots(
        self, running: "NodeFunctionRunning", fields: "ClassNodeFields"
    ) -> None:
        """Give `running` the slots the wiring followed gives each group, when another number
        than it holds: each new slot's ports declared, and its fields held by `fields`."""
        slot_counts = self.slot_counts()
        if slot_counts == running.slot_counts():
            return
        self._declare_the_ports_of(slot_counts)
        values = fields.hold_the_fields_of(
            tuple(
                member
                for pin_group in self._pin_groups
                for slot in range(slot_counts[pin_group.name])
                for member in pin_group.members_of_slot(slot)
            )
        )
        running.hold_slot_counts(slot_counts, values)

    def _declare_the_ports_of(self, slot_counts: "dict[str, int]") -> None:
        """Declare each port of the slots `slot_counts` gives that is not declared yet; a port of
        a slot dropped stays declared, for that slot held again."""
        slot_inputs, slot_outputs = the_ports_of_the_slots_built(self._pin_groups, slot_counts)
        new_inputs = [port for port in slot_inputs if port not in self._ports_declared]
        new_outputs = [port for port in slot_outputs if port not in self._ports_declared]
        if not new_inputs and not new_outputs:
            return
        self._link_data_access.declare_ports(new_inputs, new_outputs)
        self._ports_declared.update(new_inputs)
        self._ports_declared.update(new_outputs)
