# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The nodes that make a Color or a Vector from numbers and split one into them: their typed
pins as the node catalog lists them, and each called as the plain function it is."""

from __future__ import annotations

from typing import Any

from tatolab.stream import Color, Vector4

from core_nodes.combine_and_separate import (
    ColorComponents,
    CombineColor,
    CombineVector,
    SeparateColor,
    SeparateVector,
    VectorComponents,
)


def pin_types_of(declared: Any, stamp: str) -> "dict[str, Any]":
    return {
        port["name"]: (port["pin_declaration"]["pin_type"]["pin_type"], port.get("pin_default"))
        for port in getattr(declared, stamp)
    }


def inputs_of(declared: Any) -> "dict[str, Any]":
    return pin_types_of(declared, "__tatolab_node_input_ports__")


def outputs_of(declared: Any) -> "dict[str, Any]":
    return {name: pin_type for name, (pin_type, _) in pin_types_of(declared, "__tatolab_node_output_ports__").items()}


FLOAT = {"type": "float"}
COLOR = {"type": "color"}
VECTOR_OF_FOUR = {"type": "vector", "component_count": "four"}


def test_combine_color_takes_four_floats_and_gives_a_color():
    assert inputs_of(CombineColor) == {
        "red": (FLOAT, 0.0),
        "green": (FLOAT, 0.0),
        "blue": (FLOAT, 0.0),
        "alpha": (FLOAT, 1.0),
    }
    assert outputs_of(CombineColor) == {"result": COLOR}
    assert CombineColor(0.2, 0.4, 0.6) == Color(0.2, 0.4, 0.6, 1.0)


def test_separate_color_takes_opaque_black_by_default_and_gives_four_floats():
    assert inputs_of(SeparateColor) == {
        "color": (COLOR, {"red": 0.0, "green": 0.0, "blue": 0.0, "alpha": 1.0})
    }
    assert outputs_of(SeparateColor) == {"red": FLOAT, "green": FLOAT, "blue": FLOAT, "alpha": FLOAT}
    assert SeparateColor(Color(0.2, 0.4, 0.6, 0.8)) == ColorComponents(0.2, 0.4, 0.6, 0.8)


def test_combine_vector_takes_four_floats_and_gives_a_vector_of_four():
    assert inputs_of(CombineVector) == {name: (FLOAT, 0.0) for name in ("x", "y", "z", "w")}
    assert outputs_of(CombineVector) == {"result": VECTOR_OF_FOUR}
    assert CombineVector(0.3, 0.7) == Vector4(0.3, 0.7, 0.0, 0.0)


def test_separate_vector_takes_a_vector_of_four_at_zero_and_gives_four_floats():
    assert inputs_of(SeparateVector) == {
        "vector": (VECTOR_OF_FOUR, {"x": 0.0, "y": 0.0, "z": 0.0, "w": 0.0})
    }
    assert outputs_of(SeparateVector) == {"x": FLOAT, "y": FLOAT, "z": FLOAT, "w": FLOAT}
    assert SeparateVector(Vector4(1.0, 2.0, 3.0, 4.0)) == VectorComponents(1.0, 2.0, 3.0, 4.0)


def test_every_pin_is_a_value_with_no_range():
    for declared in (CombineColor, SeparateColor, CombineVector, SeparateVector):
        for stamp in ("__tatolab_node_input_ports__", "__tatolab_node_output_ports__"):
            for port in getattr(declared, stamp):
                assert port["pin_declaration"]["flow"] == "value", (declared, port["name"])
                assert "annotations" not in port["pin_declaration"], (declared, port["name"])
