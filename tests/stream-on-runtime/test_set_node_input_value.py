# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`set_node_input_value` on a running stream: one operation sets an input's
value on a Python class and a setting a Python class holds in its config, live
or saved, and refuses a value its input does not take.
"""

from __future__ import annotations

import time

import pytest

from helper_placement_observation import (
    StartTatolabdRunningStream,
    the_first_value_batch_read_after,
)
from helper_placement_streams import (
    python_class_reporting_its_configured_level,
    python_sink_reading_the_pin_value_of_its_unlinked_input,
)


pytestmark = pytest.mark.requires_gpu


def test_a_python_class_reads_each_input_value_set_live_then_saved(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A Python class whose `brightness` is linked to nothing reads its 0.25
    default; a live set is what it reads next, a saved set replaces it, and
    taking the saved value off reads the default again, in one process."""
    tatolabd = start_tatolabd_running_stream(
        python_sink_reading_the_pin_value_of_its_unlinked_input
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    defaulted, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert defaulted["lowest"] == defaulted["highest"] == pytest.approx(0.25), defaulted
    (sink,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "sink"
    ]

    def set_the_brightness(value: "float | None", mode: str) -> dict:
        return local_api.call_tool(
            "set_node_input_value",
            {
                "stream": local_api.stream_asset_of(stream_id),
                "node": sink["id"],
                "input": "brightness",
                "value": value,
                "mode": mode,
            },
        )

    for value, mode in ((0.75, "live"), (0.5, "commit")):
        set_the_brightness(value, mode)
        read, occurrence = the_first_value_batch_read_after(
            tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
        )
        assert read["lowest"] == read["highest"] == pytest.approx(value), (
            f"a {mode} set of {value} is what the unlinked input reads next: {read}"
        )
        assert read["sink_pid"] == defaulted["sink_pid"], "a value set restarts nothing"

    set_the_brightness(None, "commit")
    defaulted_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert defaulted_again["lowest"] == defaulted_again["highest"] == pytest.approx(0.25), (
        f"taking the saved value off reads the declared default again: {defaulted_again}"
    )


def test_a_setting_a_python_class_holds_in_its_config_is_set_live_and_a_bad_one_refused(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`ReportsEachBatchOfItsConfiguredLevel` reads its config's `level`; a live
    set of `level` is what it reads next, and text for `level` is refused as a
    value the input does not take, naming the node and the input."""
    tatolabd = start_tatolabd_running_stream(python_class_reporting_its_configured_level)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    configured, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert configured["lowest"] == configured["highest"] == pytest.approx(0.25), configured
    (configured_node,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "configured"
    ]

    def the_set_of_the_level(value: object) -> dict:
        return {
            "stream": local_api.stream_asset_of(stream_id),
            "node": configured_node["id"],
            "input": "level",
            "value": value,
            "mode": "live",
        }

    local_api.call_tool("set_node_input_value", the_set_of_the_level(0.6))
    reconfigured, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert reconfigured["lowest"] == reconfigured["highest"] == pytest.approx(0.6), reconfigured

    refused_because = local_api.call_tool_refusal(
        "set_node_input_value", the_set_of_the_level("bright")
    )
    assert "`level`" in refused_because and "`configured`" in refused_because, refused_because
