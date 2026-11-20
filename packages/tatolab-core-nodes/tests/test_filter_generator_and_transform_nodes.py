# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The filter, generator and transform nodes take one Color or Vector where they took separate
numbers, each defaulting to the numbers it replaced, a Color opaque."""

from __future__ import annotations

from typing import Any

import pytest
from core_nodes.filters import ChromaKey
from core_nodes.generators import Checkerboard, Circle, Gradient, RadialGradient, Rectangle
from core_nodes.transform import Tile, Transform

COLOR = {"type": "color"}
VECTOR2 = {"type": "vector", "component_count": "two"}


def the_input(declared: Any, name: str) -> dict[str, Any]:
    return next(port for port in declared.__tatolab_node_input_ports__ if port["name"] == name)


def opaque(red: float, green: float, blue: float) -> dict[str, float]:
    return {"red": red, "green": green, "blue": blue, "alpha": 1.0}


@pytest.mark.parametrize(
    ("declared", "input_name", "pin_type", "default"),
    [
        (ChromaKey, "key", COLOR, opaque(0.0, 1.0, 0.0)),
        (Gradient, "start", COLOR, opaque(0.0, 0.0, 0.0)),
        (Gradient, "end", COLOR, opaque(1.0, 1.0, 1.0)),
        (RadialGradient, "center", VECTOR2, {"x": 0.5, "y": 0.5}),
        (RadialGradient, "inner", COLOR, opaque(1.0, 1.0, 1.0)),
        (RadialGradient, "outer", COLOR, opaque(0.0, 0.0, 0.0)),
        (Checkerboard, "scroll", VECTOR2, {"x": 0.0, "y": 0.0}),
        (Circle, "center", VECTOR2, {"x": 0.5, "y": 0.5}),
        (Circle, "color", COLOR, opaque(1.0, 1.0, 1.0)),
        (Rectangle, "center", VECTOR2, {"x": 0.5, "y": 0.5}),
        (Rectangle, "color", COLOR, opaque(1.0, 1.0, 1.0)),
        (Transform, "move", VECTOR2, {"x": 0.0, "y": 0.0}),
        (Tile, "scroll", VECTOR2, {"x": 0.0, "y": 0.0}),
    ],
)
def test_each_typed_input_defaults_to_the_numbers_it_replaced(
    declared: Any, input_name: str, pin_type: dict[str, str], default: dict[str, float]
):
    typed_input = the_input(declared, input_name)
    assert typed_input["pin_declaration"]["pin_type"] == {"kind": "is", "pin_type": pin_type}
    assert typed_input["pin_default"] == default


@pytest.mark.parametrize(
    "declared", [ChromaKey, Gradient, RadialGradient, Checkerboard, Circle, Rectangle, Transform, Tile]
)
def test_no_input_of_a_retired_number_is_left(declared: Any):
    retired_endings = ("_red", "_green", "_blue", "_x", "_y")
    names = [port["name"] for port in declared.__tatolab_node_input_ports__]
    assert not [name for name in names if name.endswith(retired_endings) or name in {"red", "green", "blue"}]
