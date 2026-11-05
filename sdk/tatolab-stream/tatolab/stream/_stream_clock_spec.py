# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The clocks a stream can name with `stream_builder.run_on_clock`, as plain values."""

from __future__ import annotations

import math
from dataclasses import dataclass
from typing import Any, Union

__all__ = [
    "DEFAULT_SCALED_STREAM_CLOCK_STEP_NS",
    "RealTimeStreamClock",
    "ScaledStreamClock",
    "SteppedStreamClock",
    "StreamClock",
]


DEFAULT_SCALED_STREAM_CLOCK_STEP_NS = 16_666_667
"""Stream nanoseconds per step a scaled clock takes when none is given: one 60 Hz frame."""


_LARGEST_INTEGER_THE_ENGINE_READS = 2**64 - 1
"""The engine reads `step_ns` and `stop_after_steps` as `u64`."""


def _refuse_unless_a_positive_finite_number(value: float, what_it_is: str) -> None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TypeError(f"{what_it_is} is a number, and {value!r} is not one")
    if not math.isfinite(value) or value <= 0:
        raise ValueError(f"{what_it_is} is a finite number greater than zero, not {value!r}")


def _refuse_unless_a_positive_integer(value: int, what_it_is: str) -> None:
    if isinstance(value, bool) or not isinstance(value, int):
        raise TypeError(f"{what_it_is} is an integer, not {value!r}")
    if value <= 0:
        raise ValueError(f"{what_it_is} is greater than zero, not {value!r}")
    if value > _LARGEST_INTEGER_THE_ENGINE_READS:
        raise ValueError(f"{what_it_is} is at most 2**64 - 1, not {value!r}")


@dataclass(frozen=True)
class RealTimeStreamClock:
    """The machine's monotonic clock: what a stream runs on unless it names another."""

    def to_graph_document(self) -> dict[str, Any]:
        """The graph document's `clock` key for this clock."""
        return {"mode": "real"}


@dataclass(frozen=True)
class ScaledStreamClock:
    """Stream time runs at `scale` stream seconds per machine second: slow motion below 1.

    Above 1 it advances in steps of `step_ns` behind the step barrier, paced at
    `scale`, as a stepped clock does.
    """

    scale: float
    step_ns: int = DEFAULT_SCALED_STREAM_CLOCK_STEP_NS

    def __post_init__(self) -> None:
        _refuse_unless_a_positive_finite_number(self.scale, "a scaled clock's scale")
        _refuse_unless_a_positive_integer(self.step_ns, "a scaled clock's step_ns")

    def to_graph_document(self) -> dict[str, Any]:
        """The graph document's `clock` key for this clock."""
        return {"mode": "scaled", "scale": float(self.scale), "step_ns": self.step_ns}


@dataclass(frozen=True)
class SteppedStreamClock:
    """Stream time advances `step_ns` at a time, once every node finished the step.

    `pace` paces steps to that multiple of real time; `None` runs them as fast
    as the graph allows. `stop_after_steps` stops the stream after that step.
    """

    step_ns: int
    pace: float | None = None
    stop_after_steps: int | None = None

    def __post_init__(self) -> None:
        _refuse_unless_a_positive_integer(self.step_ns, "a stepped clock's step_ns")
        if self.pace is not None:
            _refuse_unless_a_positive_finite_number(self.pace, "a stepped clock's pace")
        if self.stop_after_steps is not None:
            _refuse_unless_a_positive_integer(
                self.stop_after_steps, "a stepped clock's stop_after_steps"
            )

    def to_graph_document(self) -> dict[str, Any]:
        """The graph document's `clock` key for this clock."""
        return {
            "mode": "stepped",
            "step_ns": self.step_ns,
            "pace": None if self.pace is None else float(self.pace),
            "stop_after_steps": self.stop_after_steps,
        }


StreamClock = Union[RealTimeStreamClock, ScaledStreamClock, SteppedStreamClock]
"""Any clock `stream_builder.run_on_clock` takes."""
