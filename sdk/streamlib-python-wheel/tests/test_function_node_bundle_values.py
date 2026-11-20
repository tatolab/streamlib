# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A bundle a function node takes or gives: the body is handed the frozen dataclass built
straight from the message's members, each one left out given its default, and gives one back;
a member left out that has no default, or a key that is no member, is refused by its name. The
node runs as Python in its reader's interpreter, as it does not compile."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated, Any

import pytest

from tatolab.runtime._function_node_body_run_as_python import FunctionNodeBodyRunAsPython
from tatolab.runtime._function_node_carried_values import FunctionNodeDoesNotCompile
from tatolab.runtime._function_node_evaluate_entry_compiler import (
    compile_function_node_to_its_evaluate_entry,
)
from tatolab.runtime._function_node_preparation import function_node_run_as_python_in_its_reader
from tatolab.runtime._function_node_stored_values import (
    the_record_an_island_carries,
    the_stored_value_of,
    the_value_a_body_takes,
)
from tatolab.stream import PinMeaning, node


@dataclass(frozen=True)
class Tone:
    frequency: Annotated[float, PinMeaning("frequency")]
    gain: float = 1.0


@dataclass(frozen=True)
class Dot:
    x: float
    y: float = 0.0


@node
def louder(tone: Tone = Tone(440.0), dots: list[Dot] = [Dot(1.0)]) -> Tone:  # noqa: B006
    return Tone(tone.frequency * 2 + sum(dot.x for dot in dots), tone.gain + 1)


TONE: "dict[str, Any]" = {
    "type": "bundle",
    "members": [
        {"name": "frequency", "member_type": {"type": "float"}, "meaning": "frequency"},
        {"name": "gain", "member_type": {"type": "float"}, "default_value": 1.0},
    ],
}


def the_body_of_louder() -> FunctionNodeBodyRunAsPython:
    run_as_python = function_node_run_as_python_in_its_reader(louder, f"{__name__}:louder", "")
    return FunctionNodeBodyRunAsPython(run_as_python, "louder", None)


def test_a_bundle_is_handed_to_the_body_as_its_dataclass_each_member_left_out_given_its_default() -> None:
    assert the_value_a_body_takes(TONE, {"frequency": 220.0}, "the tone", Tone) == Tone(220.0, 1.0)


def test_a_bundle_leaving_out_a_member_with_no_default_is_refused_naming_it() -> None:
    with pytest.raises(ValueError, match="leaves out the member `frequency`, which has no default"):
        the_value_a_body_takes(TONE, {"gain": 2.0}, "the tone", Tone)


def test_a_bundle_holding_a_key_that_is_no_member_is_refused_naming_it() -> None:
    with pytest.raises(ValueError, match="holds `volume`, which is no member"):
        the_value_a_body_takes(TONE, {"frequency": 1.0, "volume": 2.0}, "the tone", Tone)


def test_the_dataclass_a_body_gives_is_its_bundles_stored_value_and_another_shape_is_refused() -> None:
    assert the_stored_value_of(TONE, Tone(110.0, 0.5), "the tone") == {"frequency": 110.0, "gain": 0.5}
    with pytest.raises(ValueError, match="the body gave a `Dot` of `x`, `y`"):
        the_stored_value_of(TONE, Dot(1.0), "the tone")
    with pytest.raises(TypeError, match="the body gave float"):
        the_stored_value_of(TONE, 1.0, "the tone")


def test_the_record_an_island_carries_holds_the_bundle_whole() -> None:
    assert the_record_an_island_carries(TONE, {"frequency": 220.0}, "the tone") == {
        "value": {"frequency": 220.0, "gain": 1.0}
    }


def test_a_node_taking_a_bundle_and_a_list_of_them_runs_as_python_handed_its_dataclasses() -> None:
    with pytest.raises(FunctionNodeDoesNotCompile, match="is bundle"):
        compile_function_node_to_its_evaluate_entry(louder, f"{__name__}:louder")

    answered = the_body_of_louder().evaluate(
        [{"value": {"frequency": 100.0}}, {"value": [{"x": 2.0}, {"x": 3.0, "y": 1.0}]}], 0
    )

    assert answered == [{"value": {"frequency": 205.0, "gain": 2.0}}]


def test_a_node_handed_a_bundle_of_unknown_members_names_the_input_and_the_member() -> None:
    with pytest.raises(ValueError, match="`louder`'s input `tone` holds `volume`, which is no member"):
        the_body_of_louder().evaluate([{"value": {"frequency": 1.0, "volume": 2.0}}, {"value": []}], 0)
