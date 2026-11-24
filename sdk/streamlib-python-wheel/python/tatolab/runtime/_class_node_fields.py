# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The fields of a running class node under bare `@node`, each holding its input's current value.

A field holds the value its wire gave last while a wire feeds it, and the value set on it
otherwise: its stored value from the `setup` command, then each one `set_input_values` hands
over. A value arrives in its stored JSON form and is held in its Python form, a `Color` for a
Color, an `Asset` for an Asset. A setting is assigned once, before `__init__()`, since a change
to one rebuilds the node.
"""

from __future__ import annotations

from collections.abc import Collection
from typing import Any, Optional

from tatolab.stream._class_node_construction import assign_live_field_values
from tatolab.stream._class_node_declaration import NO_DEFAULT, ClassNodeMember
from tatolab.stream._node_context_protocols import LinkInputDataReader, NodeLinkDataAccess

from tatolab.runtime._class_node_bound_pins import (
    ValueReadAtAnInputPort,
    the_reader_of_a_value_at_an_input_port,
)
from tatolab.runtime._function_node_stored_values import the_value_a_body_takes

__all__ = ["ClassNodeFields"]


class ClassNodeFields:
    """The value each field of one class node holds, followed from its wire or its set value."""

    def __init__(
        self,
        members: "tuple[ClassNodeMember, ...]",
        *,
        link_data_access: NodeLinkDataAccess,
        inputs: LinkInputDataReader,
        node_id: str,
        stored_input_values: "Optional[dict[str, Any]]",
    ) -> None:
        self._inputs = inputs
        self._node_id = node_id
        self._fields = {member.port: member for member in members if member.form == "field"}
        self._read_from_its_wire: "dict[str, ValueReadAtAnInputPort]" = {}
        for member in self._fields.values():
            declared_pin_type: Any = member.pin_declaration["pin_type"]
            if member.a_setting or declared_pin_type["kind"] != "is":
                continue
            read = the_reader_of_a_value_at_an_input_port(
                declared_pin_type["pin_type"],
                link_data_access=link_data_access,
                inputs=inputs,
                node_id=node_id,
                input_port=member.port,
                bundle_class=member.bundle_class,
            )
            if read is not None:
                self._read_from_its_wire[member.port] = read
        self._set_values = self._python_forms_of(stored_input_values or {})
        self._each_held_from_its_wire: "set[str]" = set()

    def values_to_build_with(self) -> "dict[str, Any]":
        """The value each field is built with, by its port's name: its set value, or its default when it has none."""
        return dict(self._set_values)

    def take_set_values(self, node_instance: object, stored_input_values: "dict[str, Any]") -> None:
        """Hold each of `stored_input_values` a field takes as its set value, and assign it to each field no wire feeds.

        `stored_input_values` is every value the node's unlinked inputs read, so a field it
        leaves out had its value taken off, and holds its default again.
        """
        taken = self._python_forms_of(stored_input_values)
        for port, member in self._fields.items():
            if (
                port not in stored_input_values
                and port in self._set_values
                and not member.a_setting
                and member.default is not NO_DEFAULT
            ):
                del self._set_values[port]
                taken[port] = member.default
        self._set_values.update(
            {port: value for port, value in taken.items() if port in stored_input_values}
        )
        assign_live_field_values(
            node_instance,
            {port: value for port, value in taken.items() if port not in self._each_held_from_its_wire},
        )

    def take_each_new_value_a_wire_gives(
        self, node_instance: object, *, fed_by_an_island: "Collection[str]" = ()
    ) -> None:
        """Assign each field a wire feeds the newest value its wire gave, before the node's next `process`.

        A wire is a link, or the island of function nodes that `fed_by_an_island` names its
        port of, which is evaluated where the field is read.
        """
        given: "dict[str, Any]" = {}
        for port, read in self._read_from_its_wire.items():
            if not self._a_wire_feeds(port, fed_by_an_island):
                continue
            value = read()
            if value is not None:
                given[port] = value
                self._each_held_from_its_wire.add(port)
        if given:
            assign_live_field_values(node_instance, given)

    def hand_back_the_set_value_of(
        self, node_instance: object, port: str, *, fed_by_an_island: "Collection[str]" = ()
    ) -> None:
        """Assign the field of `port` its set value again, once the last wire feeding it is gone."""
        if port not in self._each_held_from_its_wire or self._a_wire_feeds(port, fed_by_an_island):
            return
        self._each_held_from_its_wire.discard(port)
        member = self._fields[port]
        value = self._set_values.get(port, member.default)
        assign_live_field_values(node_instance, {port: value})

    def _a_wire_feeds(self, port: str, fed_by_an_island: "Collection[str]") -> bool:
        return port in fed_by_an_island or bool(self._inputs.inbound_link_names(port))

    def _python_forms_of(self, stored_input_values: "dict[str, Any]") -> "dict[str, Any]":
        """Each of `stored_input_values` that names a field, in that field's Python form."""
        return {
            port: self._python_form_of(self._fields[port], stored_value)
            for port, stored_value in stored_input_values.items()
            if port in self._fields
        }

    def _python_form_of(self, member: ClassNodeMember, stored_value: Any) -> Any:
        what_it_is = f"`{self._node_id}`'s field `{member.attribute}`"
        if stored_value is None and member.pin_declaration.get("may_be_unset"):
            return None
        declared_pin_type: Any = member.pin_declaration["pin_type"]
        if declared_pin_type["kind"] != "is":
            return stored_value
        pin_type = declared_pin_type["pin_type"]
        if pin_type["type"] == "pin_type_choice":
            from tatolab.stream._pin_type_choice import PinTypeChoice

            if not isinstance(stored_value, dict):
                raise ValueError(f"{what_it_is} is {stored_value!r}, which names no pin type")
            chosen: Any = stored_value
            return PinTypeChoice.of_the_chosen_type(chosen)
        if pin_type["type"] == "node_reference":
            return stored_value
        if pin_type["type"] == "connection":
            return _the_node_a_connection_brings(pin_type["connection_type"], stored_value, what_it_is)
        return the_value_a_body_takes(pin_type, stored_value, what_it_is, member.bundle_class)


def _the_node_a_connection_brings(connection_type: str, stored_value: Any, what_it_is: str) -> Any:
    """The `Connection` naming the node a connection field's wire comes from, as the runtime stores it, `{"node": name}`."""
    from tatolab.stream._connection_type_declaration import the_connection_to

    node_name = stored_value.get("node") if isinstance(stored_value, dict) else None
    if not isinstance(node_name, str):
        raise ValueError(f"{what_it_is} is {stored_value!r}, which names no node a wire brings")
    return the_connection_to(connection_type, node_name)
