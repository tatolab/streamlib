# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A node function's connection input holds the node its wire brings, as an instance of its
connection class, and `None` while no wire reaches it.

The runtime hands a connection input the reference its wire was lowered to, `{"node": name}`,
among the node's unlinked input values; the input holds it in its Python form.
"""

from __future__ import annotations

from typing import Any, cast

import pytest

from tatolab.runtime._class_node_fields import ClassNodeFields
from tatolab.stream import Connection, Input, SoleReferrer, connection_type, node


@connection_type
class HubConnection(Connection):
    pass


@node(execution="manual")
def Spoke(
    *,
    hub: Input[HubConnection | None] = Input(None),
    only: Input[SoleReferrer[HubConnection] | None] = Input(None),
) -> None:
    """Takes a hub on `hub`, and one more on `only`, which no other spoke may take."""


class ValuesWritten:
    """What a running spoke's value inputs were last written, by port."""

    def __init__(self, values: "dict[str, Any]") -> None:
        self.values = dict(values)

    def write(self, values: "Any") -> None:
        self.values.update(values)


def the_fields_of_a_spoke_handed(
    stored_input_values: "dict[str, Any]",
) -> "tuple[ClassNodeFields, ValuesWritten]":
    fields = ClassNodeFields(
        Spoke.members,
        link_data_access=cast(Any, None),
        inputs=cast(Any, None),
        node_id="spoke-1",
        stored_input_values=stored_input_values,
        assign_field_values=ValuesWritten.write,
    )
    return fields, ValuesWritten({"hub": None, "only": None, **fields.values_to_build_with()})


def test_a_connection_input_holds_the_node_its_wire_brings_and_none_with_no_wire():
    fields, spoke = the_fields_of_a_spoke_handed({"hub": {"node": "hub-a"}})

    assert spoke.values == {"hub": HubConnection("hub-a"), "only": None}

    fields.take_set_values(spoke, {"hub": {"node": "hub-b"}, "only": {"node": "hub-a"}})
    assert spoke.values == {"hub": HubConnection("hub-b"), "only": HubConnection("hub-a")}

    fields.take_set_values(spoke, {})
    assert spoke.values == {"hub": None, "only": None}, "a wire taken out leaves the input unset"


def test_a_connection_input_handed_no_node_is_refused_naming_it():
    with pytest.raises(ValueError, match="`spoke-1`'s field `hub` is 'hub-a', which names no node"):
        the_fields_of_a_spoke_handed({"hub": "hub-a"})
