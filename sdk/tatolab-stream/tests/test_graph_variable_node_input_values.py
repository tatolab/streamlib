# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`SetVariable` and `GetVariable` declare their settings and their `value` pin as fields and
pins, so `add` checks the input values they are given and records a node reference as the
runtime reads one. What only the runtime's own nodes declare is refused on an author's class."""

from __future__ import annotations

import pytest

from tatolab.stream import (
    GetVariable,
    InputPin,
    NodeDeclarationRefusal,
    NodeReference,
    SetVariable,
    StreamBuilder,
    node,
)


def test_a_set_variable_records_the_node_it_holds_as_a_reference() -> None:
    stream_builder = StreamBuilder("rig")
    held = stream_builder.add(GetVariable, name="held", input_values={"variable": "other"})
    stream_builder.add(
        SetVariable, name="setter", input_values={"variable": "gateway", "node_reference": held}
    )

    recorded = {
        recorded["name"]: recorded.get("input_values")
        for recorded in stream_builder._compiled_graph()["nodes"]
    }

    assert recorded["setter"] == {"variable": "gateway", "node_reference": {"node": "held"}}
    assert recorded["held"] == {"variable": "other"}


def test_a_variable_node_refuses_a_value_its_settings_do_not_take() -> None:
    stream_builder = StreamBuilder("rig")

    with pytest.raises(TypeError, match="`SetVariable`'s input `node_reference`"):
        stream_builder.add(
            SetVariable, input_values={"variable": "gateway", "node_reference": "held"}
        )
    with pytest.raises(TypeError, match="`variable`"):
        stream_builder.add(GetVariable, input_values={"variable": 3})


def test_a_variable_nodes_value_pin_is_named_by_a_link() -> None:
    stream_builder = StreamBuilder("rig")
    getter = stream_builder.add(GetVariable, input_values={"variable": "camera"})
    setter = stream_builder.add(SetVariable, input_values={"variable": "camera"})

    assert getter.output("value").port_name == "value"
    assert setter.input("value").port_name == "value"
    with pytest.raises(ValueError, match="`video`"):
        getter.output("video")


def test_an_authors_class_node_typing_a_pin_object_or_a_field_a_node_reference_is_refused() -> None:
    with pytest.raises(NodeDeclarationRefusal, match="`object`"):

        @node
        class Passes:
            value: InputPin[object]

            def process(self) -> None: ...

    with pytest.raises(NodeDeclarationRefusal, match="`NodeReference`"):

        @node
        class Names:
            target: NodeReference | None = None

            def process(self) -> None: ...

