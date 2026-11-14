# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

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
    run_graph_generator,
    graph_generator,
)


@graph_generator
def on_the_machine_clock(stream_builder: StreamBuilder) -> None:
    stream_builder.add(FrameInverter)


@graph_generator
def in_slow_motion(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(ScaledStreamClock(scale=0.5))
    stream_builder.add(FrameInverter)


@graph_generator
def stepped_for_a_test(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(
        SteppedStreamClock(step_ns=1_000_000, pace=2.0, stop_after_steps=30)
    )
    stream_builder.add(FrameInverter)


@graph_generator
def names_real_time(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(RealTimeStreamClock())
    stream_builder.add(FrameInverter)


@graph_generator
def names_its_clock_twice(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(ScaledStreamClock(scale=0.5))
    stream_builder.run_on_clock(ScaledStreamClock(scale=0.25))


@graph_generator
def names_a_number_as_its_clock(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(0.5)  # type: ignore[arg-type]


def test_a_stream_naming_no_clock_carries_no_clock_key() -> None:
    assert "clock" not in run_graph_generator(on_the_machine_clock)


def test_a_scaled_clock_compiles_to_the_clock_key_beside_the_stream() -> None:
    graph = run_graph_generator(in_slow_motion)

    assert list(graph) == ["stream", "clock", "nodes", "links"]
    assert graph["clock"] == {
        "mode": "scaled",
        "scale": 0.5,
        "step_ns": DEFAULT_SCALED_STREAM_CLOCK_STEP_NS,
    }
    assert json.loads(json.dumps(graph)) == graph


def test_a_stepped_clock_compiles_every_field() -> None:
    assert run_graph_generator(stepped_for_a_test)["clock"] == {
        "mode": "stepped",
        "step_ns": 1_000_000,
        "pace": 2.0,
        "stop_after_steps": 30,
    }


def test_naming_real_time_compiles_to_the_real_mode() -> None:
    assert run_graph_generator(names_real_time)["clock"] == {"mode": "real"}


def test_a_stream_names_its_clock_once() -> None:
    with pytest.raises(ValueError, match="names its clock once"):
        run_graph_generator(names_its_clock_twice)


def test_run_on_clock_refuses_anything_but_a_clock() -> None:
    with pytest.raises(TypeError, match="ScaledStreamClock"):
        run_graph_generator(names_a_number_as_its_clock)


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


@pytest.mark.parametrize("pace", [0, -1.0, math.inf, math.nan])
def test_a_pace_is_a_finite_number_greater_than_zero(pace: float) -> None:
    with pytest.raises(ValueError, match="greater than zero"):
        SteppedStreamClock(step_ns=1_000_000, pace=pace)


@pytest.mark.parametrize("stop_after_steps", [0, -3])
def test_stop_after_steps_is_greater_than_zero(stop_after_steps: int) -> None:
    with pytest.raises(ValueError, match="greater than zero"):
        SteppedStreamClock(step_ns=1_000_000, stop_after_steps=stop_after_steps)


def test_an_integer_the_engine_cannot_read_as_u64_is_refused() -> None:
    with pytest.raises(ValueError, match="at most"):
        SteppedStreamClock(step_ns=2**64)
    with pytest.raises(ValueError, match="at most"):
        SteppedStreamClock(step_ns=1_000_000, stop_after_steps=2**64)
