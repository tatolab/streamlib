# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Tint and Duotone take a Color where they took three numbers, defaulting to those numbers' defaults, opaque."""

from __future__ import annotations

from typing import Any

import pytest
from core_nodes.color import Duotone, Tint

COLOR_INPUT_DECLARATION = {"pin_type": {"kind": "is", "pin_type": {"type": "color"}}, "flow": "value"}


def the_input(declared: Any, name: str) -> dict[str, Any]:
    return next(port for port in declared.__tatolab_node_input_ports__ if port["name"] == name)


@pytest.mark.parametrize(
    ("declared", "input_name", "default"),
    [
        (Tint, "color", {"red": 1.0, "green": 0.5, "blue": 0.2, "alpha": 1.0}),
        (Duotone, "dark", {"red": 0.05, "green": 0.0, "blue": 0.3, "alpha": 1.0}),
        (Duotone, "light", {"red": 1.0, "green": 0.8, "blue": 0.4, "alpha": 1.0}),
    ],
)
def test_each_color_input_defaults_to_the_numbers_it_replaced(
    declared: Any, input_name: str, default: dict[str, float]
):
    color_input = the_input(declared, input_name)
    assert color_input["pin_declaration"] == COLOR_INPUT_DECLARATION
    assert color_input["pin_default"] == default


@pytest.mark.parametrize(
    ("declared", "inputs"),
    [(Tint, ["image", "color", "amount"]), (Duotone, ["image", "dark", "light"])],
)
def test_no_input_of_a_retired_number_is_left(declared: Any, inputs: list[str]):
    assert [port["name"] for port in declared.__tatolab_node_input_ports__] == inputs
