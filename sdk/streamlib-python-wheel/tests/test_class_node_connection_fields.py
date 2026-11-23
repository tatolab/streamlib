# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class node's connection field holds the node its wire brings, as an instance of its connection class, and `None` while no wire reaches it.

The runtime hands a connection input the reference its wire was lowered to, `{"node": name}`,
among the node's unlinked input values; the field holds it in its Python form.
"""

from __future__ import annotations

from typing import Any, cast

import pytest

from tatolab.runtime._class_node_fields import ClassNodeFields
from tatolab.runtime._node_hosting import class_node_members_of
from tatolab.stream import Connection, SoleReferrer, connection_type, node
from tatolab.stream._class_node_construction import construct_class_node


@connection_type
class HubConnection(Connection):
    pass


@node(execution="manual")
class Spoke:
    """Takes a hub on `hub`, and one more on `only`, which no other spoke may take."""

    hub: HubConnection | None = None
    only: SoleReferrer[HubConnection] | None = None

    def process(self, ctx: Any) -> None:
        del ctx


def the_fields_of_a_spoke_handed(stored_input_values: "dict[str, Any]") -> "tuple[ClassNodeFields, Any]":
    members = class_node_members_of(Spoke)
    assert members is not None
    fields = ClassNodeFields(
        members,
        link_data_access=cast(Any, None),
        inputs=cast(Any, None),
        node_id="spoke-1",
        stored_input_values=stored_input_values,
    )
    spoke = construct_class_node(Spoke, field_values=fields.values_to_build_with(), pins={})
    return fields, spoke


def test_a_connection_field_holds_the_node_its_wire_brings_and_none_with_no_wire():
    fields, spoke = the_fields_of_a_spoke_handed({"hub": {"node": "hub-a"}})

    assert spoke.hub == HubConnection("hub-a")
    assert spoke.only is None

    fields.take_set_values(spoke, {"hub": {"node": "hub-b"}, "only": {"node": "hub-a"}})
    assert (spoke.hub, spoke.only) == (HubConnection("hub-b"), HubConnection("hub-a"))

    fields.take_set_values(spoke, {})
    assert (spoke.hub, spoke.only) == (None, None), "a wire taken out leaves the field unset"


def test_a_connection_field_handed_no_node_is_refused_naming_it():
    with pytest.raises(ValueError, match="`spoke-1`'s field `hub` is 'hub-a', which names no node"):
        the_fields_of_a_spoke_handed({"hub": "hub-a"})
