# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A function node's output into a setting a Python class reads at setup: the
class is first built with the value the function node works out, and rebuilt
once with each value it works out after that, whether the stream's function
node pool evaluates it or it runs in a process of its own.
"""

from __future__ import annotations

import time

import pytest

from helper_placement_observation import StartTatolabdRunningStream
from helper_placement_streams import (
    held_level_into_a_setting_and_read_by_a_python_sink,
    typed_level_into_a_setting_a_python_class_reads_at_setup,
)
from runtime_process_under_test import MarkerLineParser, NOT_A_MARKER_LINE


pytestmark = pytest.mark.requires_gpu

#: Long enough for any rebuild a value asked for to have run, past the window a
#: burst of values settles within.
ANY_REBUILD_HAS_RUN_AFTER_SECONDS = 2.0


def the_levels_built_with(stderr_text: str) -> "list[float]":
    """The `level` each build of `ReportsTheLevelItIsBuiltWith` reported, in order."""
    parser = MarkerLineParser("BUILT_WITH_LEVEL")
    reported = (parser.payload_of(line) for line in stderr_text.splitlines())
    return [payload["level"] for payload in reported if payload is not NOT_A_MARKER_LINE]


def test_a_class_is_first_built_with_the_value_the_pool_works_out_as_it_sets_up(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A typed function node built with `gain` 0.5 feeds `level`, a setting the
    class reads at setup: the pool works 0.5 out before the class is built, so
    its one build reads 0.5 and never its own 0.1.

    Fail-without-fix: the connect is refused with
    `link_from_a_function_node_into_a_setting`; without the pool's setup write
    the class is built with 0.1 and then rebuilt with 0.5.
    """
    tatolabd = start_tatolabd_running_stream(
        typed_level_into_a_setting_a_python_class_reads_at_setup
    )
    built = tatolabd.await_marker("BUILT_WITH_LEVEL")
    assert built["level"] == pytest.approx(0.5), built
    time.sleep(ANY_REBUILD_HAS_RUN_AFTER_SECONDS)
    assert the_levels_built_with(tatolabd.stderr_text) == [pytest.approx(0.5)], (
        f"one build, with the function node's value:\n{tatolabd.recent_stderr()}"
    )


def test_each_value_the_pool_works_out_into_a_setting_rebuilds_its_class_once_with_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A live set of the function node's `gain` changes what it works out, and
    the class is rebuilt once with each new value; setting the same `gain`
    again rebuilds nothing.

    Fail-without-fix: reading the carry without rebuilding leaves the class
    built with 0.5, and a rebuild with anything but the value carried reports
    the wrong level.
    """
    tatolabd = start_tatolabd_running_stream(
        typed_level_into_a_setting_a_python_class_reads_at_setup
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    tatolabd.await_marker("BUILT_WITH_LEVEL")
    (scaled,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "scaled"
    ]

    def set_the_gain_live(gain: float) -> None:
        local_api.call_tool(
            "set_node_input_value",
            {
                "stream": stream_id,
                "node": scaled["id"],
                "input": "gain",
                "value": gain,
                "mode": "live",
            },
        )

    set_the_gain_live(0.75)
    rebuilt = tatolabd.await_marker("BUILT_WITH_LEVEL", occurrence=2)
    assert rebuilt["level"] == pytest.approx(0.75), rebuilt
    set_the_gain_live(0.75)
    time.sleep(ANY_REBUILD_HAS_RUN_AFTER_SECONDS)
    assert the_levels_built_with(tatolabd.stderr_text) == [
        pytest.approx(0.5),
        pytest.approx(0.75),
    ], f"a value the class is built with rebuilds nothing:\n{tatolabd.recent_stderr()}"


def test_a_stateful_function_node_builds_the_class_whose_setting_it_feeds_with_its_value(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A stateful function node read by a setting and by a second class runs
    in a process of its own, as every stateful one does; it works its 0.25 out as it sets up, so the class
    reading it at setup is built once, with 0.25 and never its own 0.1, and the
    second class reads 0.25 too.

    Fail-without-fix: a promoted node not evaluated at its setup builds the
    class with 0.1 and then rebuilds it with 0.25.
    """
    tatolabd = start_tatolabd_running_stream(held_level_into_a_setting_and_read_by_a_python_sink)
    built = tatolabd.await_marker("BUILT_WITH_LEVEL")
    assert built["level"] == pytest.approx(0.25), built
    read = tatolabd.await_marker("FUNCTION_NODE_VALUE_BATCH_READ")
    assert read["lowest"] == read["highest"] == pytest.approx(0.25), read
    time.sleep(ANY_REBUILD_HAS_RUN_AFTER_SECONDS)
    assert the_levels_built_with(tatolabd.stderr_text) == [pytest.approx(0.25)], (
        f"one build, with the held node's value:\n{tatolabd.recent_stderr()}"
    )
