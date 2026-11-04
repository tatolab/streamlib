# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stream's clock as `run_on_clock` names it and the graph document carries it."""

import json
import math

import pytest

from stream_graph_builder_nodes import FrameInverter
from tatolab.stream import (
    DEFAULT_SCALED_STREAM_CLOCK_STEP_NS,
    RealTimeStreamClock,
    ScaledStreamClock,
    SteppedStreamClock,
    StreamBuilder,
    compile_stream_to_graph,
    stream,
)


@stream
def on_the_machine_clock(stream_builder: StreamBuilder) -> None:
    stream_builder.add(FrameInverter)


@stream
def in_slow_motion(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(ScaledStreamClock(scale=0.5))
    stream_builder.add(FrameInverter)


@stream
def stepped_for_a_test(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(
        SteppedStreamClock(step_ns=1_000_000, pace=2.0, stop_after_steps=30)
    )
    stream_builder.add(FrameInverter)


@stream
def names_real_time(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(RealTimeStreamClock())
    stream_builder.add(FrameInverter)


@stream
def names_its_clock_twice(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(ScaledStreamClock(scale=0.5))
    stream_builder.run_on_clock(ScaledStreamClock(scale=0.25))


@stream
def names_a_number_as_its_clock(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(0.5)  # type: ignore[arg-type]


def test_a_stream_naming_no_clock_carries_no_clock_key() -> None:
    assert "clock" not in compile_stream_to_graph(on_the_machine_clock)


def test_a_scaled_clock_compiles_to_the_clock_key_beside_the_stream() -> None:
    graph = compile_stream_to_graph(in_slow_motion)

    assert list(graph) == ["stream", "clock", "nodes", "links", "exposed"]
    assert graph["clock"] == {
        "mode": "scaled",
        "scale": 0.5,
        "step_ns": DEFAULT_SCALED_STREAM_CLOCK_STEP_NS,
    }
    assert json.loads(json.dumps(graph)) == graph


def test_a_stepped_clock_compiles_every_field() -> None:
    assert compile_stream_to_graph(stepped_for_a_test)["clock"] == {
        "mode": "stepped",
        "step_ns": 1_000_000,
        "pace": 2.0,
        "stop_after_steps": 30,
    }


def test_naming_real_time_compiles_to_the_real_mode() -> None:
    assert compile_stream_to_graph(names_real_time)["clock"] == {"mode": "real"}


def test_a_stream_names_its_clock_once() -> None:
    with pytest.raises(ValueError, match="names its clock once"):
        compile_stream_to_graph(names_its_clock_twice)


def test_run_on_clock_refuses_anything_but_a_clock() -> None:
    with pytest.raises(TypeError, match="ScaledStreamClock"):
        compile_stream_to_graph(names_a_number_as_its_clock)


@pytest.mark.parametrize("scale", [0, -1.0, math.inf, math.nan])
def test_a_scale_is_a_finite_number_greater_than_zero(scale: float) -> None:
    with pytest.raises(ValueError, match="greater than zero"):
        ScaledStreamClock(scale=scale)


@pytest.mark.parametrize("step_ns", [0, -5])
def test_a_step_is_greater_than_zero(step_ns: int) -> None:
    with pytest.raises(ValueError, match="greater than zero"):
        SteppedStreamClock(step_ns=step_ns)


def test_a_step_is_an_integer_number_of_nanoseconds() -> None:
    with pytest.raises(TypeError, match="integer"):
        SteppedStreamClock(step_ns=1.5)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="integer"):
        ScaledStreamClock(scale=0.5, step_ns=True)


def test_a_clock_is_a_frozen_value() -> None:
    clock = ScaledStreamClock(scale=0.5)
    with pytest.raises(AttributeError):
        clock.scale = 2.0  # type: ignore[misc]
    assert clock == ScaledStreamClock(scale=0.5)
