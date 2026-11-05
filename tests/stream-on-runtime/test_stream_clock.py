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
from stream_clock_processors import (
    CONTINUOUS_TICK_COUNTER_INTERVAL_MS,
    STEPPED_STREAM_STEP_MS,
    STEPPED_STREAM_STEPS,
)
from stream_clock_streams import (
    SLOW_MOTION_SCALE,
    continuous_ticks_in_slow_motion,
    step_times_through_a_slow_relay,
    stream_times_in_real_time,
    stream_times_in_slow_motion,
)

pytestmark = [pytest.mark.requires_gpu]

REPORT_TIMEOUT_SECONDS = 30.0

#: How far a measured rate may stray from the clock's scale: the writer's two
#: reads of the two clocks are a few microseconds apart over half a second.
RATE_TOLERANCE = 0.02

#: How far, in stream nanoseconds, the reader's clock may run ahead of a stamp:
#: one second of delivery at half speed.
READER_AHEAD_OF_STAMP_BOUND_NS = 500_000_000


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
        assert seen["reader_stream_time_ns"] - seen["stamp_ns"] < READER_AHEAD_OF_STAMP_BOUND_NS, (
            f"the reader's stream time runs no further ahead of the stamp than delivery takes: {seen}"
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


#: The scale the test warps the slow-motion stream to while it runs.
LIVE_RESCALED_SCALE = 0.25

#: How far a tick's mean machine interval may stray from the one its scale names.
TICK_INTERVAL_TOLERANCE = 0.25


def assert_ticks_paced_on_stream_time(reported: "dict[str, Any]", scale: float) -> None:
    assert reported["scale"] == scale, reported
    interval_ns = CONTINUOUS_TICK_COUNTER_INTERVAL_MS * 1_000_000
    stream_interval_ns = reported["stream_elapsed_ns"] / reported["ticks"]
    machine_interval_ns = reported["machine_elapsed_ns"] / reported["ticks"]
    assert abs(stream_interval_ns / interval_ns - 1) < TICK_INTERVAL_TOLERANCE, (
        f"at {scale}× a tick took {stream_interval_ns / 1e6:.2f} stream ms, not "
        f"{CONTINUOUS_TICK_COUNTER_INTERVAL_MS}: {reported}"
    )
    assert abs(machine_interval_ns * scale / interval_ns - 1) < TICK_INTERVAL_TOLERANCE, (
        f"at {scale}× a tick took {machine_interval_ns / 1e6:.2f} machine ms, not "
        f"{CONTINUOUS_TICK_COUNTER_INTERVAL_MS / scale}: {reported}"
    )


def test_a_continuous_node_fires_on_stream_time_and_follows_a_live_rescale(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(continuous_ticks_in_slow_motion)
    stream_id = tatolabd.await_the_latest_stream_loaded()

    at_the_declared_scale = tatolabd.await_marker(
        "CONTINUOUS_TICKS_AT_SCALE_0", timeout=REPORT_TIMEOUT_SECONDS
    )
    rescaled = tatolabd.local_api_client().call_tool(
        "set_stream_clock_scale", {"stream": stream_id, "scale": LIVE_RESCALED_SCALE}
    )
    at_the_live_scale = tatolabd.await_marker(
        "CONTINUOUS_TICKS_AT_SCALE_1", timeout=REPORT_TIMEOUT_SECONDS
    )
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert rescaled == {"stream": stream_id, "scale": LIVE_RESCALED_SCALE}
    assert_ticks_paced_on_stream_time(at_the_declared_scale, SLOW_MOTION_SCALE)
    assert_ticks_paced_on_stream_time(at_the_live_scale, LIVE_RESCALED_SCALE)
    assert at_the_live_scale["stream_time_went_backwards"] is False, at_the_live_scale
    assert graph["clock"]["scale"] == SLOW_MOTION_SCALE, graph


def test_a_real_time_streams_clock_is_not_rescaled(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(stream_times_in_real_time)
    stream_id = tatolabd.await_the_latest_stream_loaded()

    refusal = tatolabd.local_api_client().call_tool_refusal(
        "set_stream_clock_scale", {"stream": stream_id, "scale": 0.5}
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert f"`{stream_id}` runs on a real clock" in refusal, refusal


def test_a_stepped_stream_runs_each_step_through_helpers_faster_than_real_time(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(step_times_through_a_slow_relay)
    reported = tatolabd.await_marker("STEPPED_STREAM_RUN", timeout=REPORT_TIMEOUT_SECONDS)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    seen = reported["seen"]
    step_ns = STEPPED_STREAM_STEP_MS * 1_000_000
    base_ns = seen[0]["written_stream_time_ns"] - seen[0]["step_index"] * step_ns
    assert [message["step_index"] for message in seen] == list(range(STEPPED_STREAM_STEPS)), seen
    for message in seen:
        step_time_ns = base_ns + message["step_index"] * step_ns
        assert message["written_stream_time_ns"] == step_time_ns, message
        assert message["stamp_the_relay_read_ns"] == step_time_ns, message
        assert message["relay_stream_time_ns"] == step_time_ns, (
            f"the step stays open while the slow relay holds its message: {message}"
        )
        assert message["stamp_ns"] == step_time_ns, message
        assert message["reader_step_index"] == message["step_index"], message
        assert message["lost_since_last_read"] == 0, message
    stream_elapsed_ns = STEPPED_STREAM_STEPS * step_ns
    assert reported["machine_elapsed_ns"] < stream_elapsed_ns / 2, (
        f"{STEPPED_STREAM_STEPS} unpaced steps of {STEPPED_STREAM_STEP_MS} ms took "
        f"{reported['machine_elapsed_ns'] / 1e6:.0f} machine ms"
    )
