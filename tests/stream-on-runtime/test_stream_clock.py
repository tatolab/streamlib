# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stream's clock, as helper-placed nodes read it and their writes are stamped on it.

The writer and the reporter each run in their own helper process, so the
stream time they agree on reached both through the stream's clock board.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest

from conftest import TatolabdUnderTest
from engine_media_clock import engine_media_clock_now_ns
from stream_clock_streams import (
    SLOW_MOTION_SCALE,
    stream_times_in_real_time,
    stream_times_in_slow_motion,
)

pytestmark = [pytest.mark.requires_gpu]

REPORT_TIMEOUT_SECONDS = 30.0

#: How far a measured rate may stray from the clock's scale: the writer's two
#: reads of the two clocks are a few microseconds apart over half a second.
RATE_TOLERANCE = 0.02


def the_stamps_reported_by(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
    stream_function: Any,
) -> "dict[str, dict[str, Any]]":
    tatolabd = start_tatolabd_running_stream(stream_function)
    reported = tatolabd.await_marker("STREAM_TIME_STAMPS", timeout=REPORT_TIMEOUT_SECONDS)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    return reported


def stream_seconds_per_machine_second(first: "dict[str, Any]", last: "dict[str, Any]") -> float:
    return (last["written_stream_time_ns"] - first["written_stream_time_ns"]) / (
        last["written_machine_time_ns"] - first["written_machine_time_ns"]
    )


def test_a_slow_motion_streams_nodes_read_and_stamp_half_real_time(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    reported = the_stamps_reported_by(start_tatolabd_running_stream, stream_times_in_slow_motion)
    first, last = reported["first"], reported["last"]

    for seen in (first, last):
        assert seen["writer_mode"] == "scaled" and seen["reader_mode"] == "scaled", seen
        assert seen["writer_scale"] == SLOW_MOTION_SCALE, seen
        assert seen["written_stream_time_ns"] <= seen["stamp_ns"] <= seen["reader_stream_time_ns"], (
            f"a write with no stamp is stamped with stream time between the writer's read and "
            f"the reader's: {seen}"
        )
        assert seen["stamp_ns"] < seen["written_machine_time_ns"], (
            f"a half-speed clock started at the machine clock falls behind it: {seen}"
        )

    rate = stream_seconds_per_machine_second(first, last)
    assert abs(rate - SLOW_MOTION_SCALE) < RATE_TOLERANCE, (
        f"stream time advanced {rate:.3f} s per machine second, not {SLOW_MOTION_SCALE}"
    )
    stamp_rate = (last["stamp_ns"] - first["stamp_ns"]) / (
        last["written_machine_time_ns"] - first["written_machine_time_ns"]
    )
    assert abs(stamp_rate - SLOW_MOTION_SCALE) < RATE_TOLERANCE * 5, (
        f"stamps advanced {stamp_rate:.3f} s per machine second, not {SLOW_MOTION_SCALE}"
    )


def test_a_real_time_streams_nodes_read_and_stamp_the_machine_clock(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    before = engine_media_clock_now_ns()
    reported = the_stamps_reported_by(start_tatolabd_running_stream, stream_times_in_real_time)
    after = engine_media_clock_now_ns()

    for seen in (reported["first"], reported["last"]):
        assert seen["writer_mode"] == "real" and seen["reader_mode"] == "real", seen
        assert seen["writer_scale"] == 1.0, seen
        assert before <= seen["written_stream_time_ns"] <= seen["stamp_ns"] <= after, seen
        assert seen["written_stream_time_ns"] <= seen["written_machine_time_ns"], seen
