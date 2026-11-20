# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A function node taking or giving a Color, a Vector or a Rectangle compiles: each value
travels through the evaluate entry as one f64 per part, in the order its type packs them, and
reaches the body as a value it reads by field."""

from __future__ import annotations

import ctypes
from typing import Any, NamedTuple, Optional

import pytest

from tatolab.runtime._function_node_compilation import (
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    compile_function_node_to_its_evaluate_entry,
    prepare_function_node_to_evaluate,
)
from tatolab.stream import Color, Rectangle, Transform, Vector2, Vector4, node

from function_node_test_nodes import _EVALUATE_ENTRY_TYPE

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


@node
def tint_a_spot(spot: Vector2 = Vector2(0.5, 0.25), brightness: float = 1.0) -> Color:
    return Color(spot.x * brightness, spot.y * brightness, brightness)


@node
def combine_a_vector(x: float = 0.0, y: float = 0.0, z: float = 0.0, w: float = 0.0) -> Vector4:
    return Vector4(x, y, z, w)


class ColourAndArea(NamedTuple):
    colour: Color
    size: float


@node
def colour_of_an_area(area: Rectangle = Rectangle(0.0, 0.0, 1.0, 1.0)) -> ColourAndArea:
    return ColourAndArea(colour=Color(area.x, area.y, 0.0, 0.5), size=area.width * area.height)


@node
def move_nothing(move: Transform = Transform.identity()) -> float:
    return move.numbers[0]


@node
def a_colour_now_and_then(level: float = 0.0) -> Optional[Color]:
    return Color(level, level, level) if level > 0.0 else None


def compiled(function_node: Any) -> FunctionNodeCompiledToItsEvaluateEntry:
    return compile_function_node_to_its_evaluate_entry(
        function_node, f"{__name__}:{function_node.__qualname__}"
    )


def the_slots_the_entry_writes(
    compiled_node: FunctionNodeCompiledToItsEvaluateEntry, input_slots: "list[float]", output_slot_count: int
) -> "list[float]":
    input_values = (ctypes.c_double * max(len(input_slots), 1))(*input_slots)
    unused = (ctypes.c_double * 1)()
    output_values = (ctypes.c_double * output_slot_count)()
    status = _EVALUATE_ENTRY_TYPE(compiled_node.evaluate_entry_address)(
        input_values, unused, unused, output_values, 0
    )
    assert status == 0
    return list(output_values)


def test_a_vector_input_and_a_colour_output_each_take_one_slot_per_part():
    tint_compiled = compiled(tint_a_spot)

    assert tint_compiled.how_its_values_are_carried["inputs_carried_in_parts"] == [(0, "vector2")]
    assert tint_compiled.how_its_values_are_carried["outputs_carried_in_parts"] == [(0, "color")]
    assert tint_compiled.pin_defaults_by_input_port["spot"] == (0.5, 0.25)
    assert the_slots_the_entry_writes(tint_compiled, [0.5, 0.25, 2.0], 4) == [1.0, 0.5, 2.0, 1.0]


def test_a_node_combining_numbers_into_a_vector_compiles():
    assert the_slots_the_entry_writes(compiled(combine_a_vector), [1.0, 2.0, 3.0, 4.0], 4) == [
        1.0,
        2.0,
        3.0,
        4.0,
    ]


def test_a_rectangle_input_and_several_outputs_one_a_colour_compile():
    colour_compiled = compiled(colour_of_an_area)

    assert colour_compiled.output_ports == ("colour", "size")
    assert colour_compiled.how_its_values_are_carried["outputs_carried_in_parts"] == [(0, "color")]
    assert the_slots_the_entry_writes(colour_compiled, [0.25, 0.75, 2.0, 3.0], 5) == [
        0.25,
        0.75,
        0.0,
        0.5,
        6.0,
    ]


def test_a_transform_is_refused_by_name_and_runs_as_python():
    with pytest.raises(FunctionNodeDoesNotCompile, match="input `move` is transform"):
        compiled(move_nothing)
    prepared = prepare_function_node_to_evaluate(move_nothing, f"{__name__}:move_nothing")
    assert not isinstance(prepared, FunctionNodeCompiledToItsEvaluateEntry)


def test_a_value_of_parts_a_node_may_hand_out_nothing_on_is_refused_by_name():
    with pytest.raises(FunctionNodeDoesNotCompile, match="output `result` is an optional color"):
        compiled(a_colour_now_and_then)
