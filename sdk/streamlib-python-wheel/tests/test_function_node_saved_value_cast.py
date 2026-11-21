# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A saved value cast by a function node conversion, its body run once out of stream as its
reader runs it, or refused saying why."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

import pytest

from tatolab.runtime._function_node_saved_value_cast import (
    SavedValueCastDidNotRun,
    SavedValueNotCast,
    the_saved_value_a_function_node_casts,
)
from tatolab.stream import Color, State, Stateful, Time, node


@node.meta(conversion=True)
@node
def LevelAsPercent(level: float = 0.0) -> str:
    return f"{round(level * 100)}%"


@node.meta(conversion=True)
@node
def LevelInQuarters(level: float = 0.0) -> int:
    return int(level * 4)


@node.meta(conversion=True)
@node
def TextAsAFlag(text: str = "") -> bool:
    return text == "yes"


@node.meta(conversion=True)
@node
def LevelAsGrey(level: float = 0.0) -> Color:
    return Color(level, level, level, 1.0)


@node.meta(conversion=True)
@node
def LevelChecked(level: float = 0.0) -> str:
    if level > 1.0:
        raise ValueError("a level is at most 1")
    return str(level)


@node.meta(conversion=True)
@node
def LevelAtTheTime(now: Time, level: float = 0.0) -> int:
    return int(level) + now


@dataclass
class Calls:
    count: int = 0


@node.meta(conversion=True)
@node
def LevelCounted(state: State[Calls], level: float = 0.0) -> Stateful[int, Calls]:
    return int(level) + state.count, Calls(state.count + 1)


class LevelInAStream:
    """Stands in for a node class conversion, whose body is its `process`."""

    def process(self, ctx: Any) -> None: ...


def cast(function_node: Any, input_port: str, saved_value: Any) -> Any:
    return the_saved_value_a_function_node_casts(
        function_node, f"test_function_node_saved_value_cast:{function_node.__name__}", input_port,
        saved_value,
    )


def test_a_float_is_cast_into_the_string_the_body_gives():
    """Mental-revert: answering the saved value as it is gives 0.1, no String."""
    assert cast(LevelAsPercent, "level", 0.1) == "10%"


def test_an_integer_and_a_boolean_are_given_in_stored_form():
    """An Integer is a whole JSON number and a Boolean `true` or `false`, never the f64 an island
    carries."""
    whole = cast(LevelInQuarters, "level", 1.0)
    assert whole == 4 and type(whole) is int
    assert cast(TextAsAFlag, "text", "yes") is True


def test_a_value_of_parts_is_given_in_stored_form():
    assert cast(LevelAsGrey, "level", 0.5) == Color(0.5, 0.5, 0.5, 1.0).stored_value()


def test_a_body_that_raises_is_refused_naming_what_it_raised():
    with pytest.raises(
        SavedValueNotCast, match=r"LevelChecked`'s body raised ValueError: a level is at most 1"
    ) as refused:
        cast(LevelChecked, "level", 2.0)
    assert not isinstance(refused.value, SavedValueCastDidNotRun)


def test_a_value_its_input_does_not_take_is_refused():
    with pytest.raises(SavedValueNotCast, match="input `text`") as refused:
        cast(TextAsAFlag, "text", 3)
    assert not isinstance(refused.value, SavedValueCastDidNotRun)


def test_a_body_taking_the_stream_time_is_refused_saying_so():
    with pytest.raises(SavedValueCastDidNotRun, match="takes the stream's time as `now`"):
        cast(LevelAtTheTime, "level", 1.0)


def test_a_body_keeping_state_is_refused_saying_so():
    with pytest.raises(SavedValueCastDidNotRun, match="keeps state between calls"):
        cast(LevelCounted, "level", 1.0)


def test_an_input_the_conversion_does_not_have_is_refused():
    with pytest.raises(SavedValueCastDidNotRun, match="has no input `volume`"):
        cast(LevelAsPercent, "volume", 1.0)


def test_a_node_class_conversion_is_refused_since_its_body_runs_only_in_a_stream():
    with pytest.raises(SavedValueCastDidNotRun, match="is no function node"):
        cast(LevelInAStream, "level", 1.0)


def test_a_value_a_number_input_does_not_take_is_refused_naming_it():
    with pytest.raises(SavedValueNotCast, match=r"input `level` does not take 'x'"):
        cast(LevelAsPercent, "level", "x")
