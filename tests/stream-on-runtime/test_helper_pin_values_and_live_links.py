# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What a Python node reads from an unlinked input's pin value, and from an
input a link made live drives, while its stream runs.
"""

from __future__ import annotations

import time

import pytest

from helper_placement_observation import (
    StartTatolabdRunningStream,
    the_first_value_batch_read_after,
)
from helper_placement_streams import (
    held_level_read_by_two_python_sinks,
    level_scaled_by_its_gain_into_python_sink,
    python_class_reporting_its_level,
    python_sink_reading_the_pin_value_of_its_unlinked_input,
    one_width_source_alone,
    typed_level_scaled_by_the_gain_its_input_values_hold_into_python_sink,
)
from helper_placement_nodes import WIDTH_HELD_UNLINKED, WIDTH_WRITTEN
from helper_process_observation import (
    helper_process_ids_started_in,
)
from stream_runs_on_tatolabd import TatolabRunOfAProject


pytestmark = pytest.mark.requires_gpu

#: How many batches a sink reads after a live `connect` before its link must drive the setting:
#: the first may have started before the link was wired.
BATCHES_A_LINK_MADE_LIVE_IS_GIVEN_TO_DRIVE_A_SETTING = 3


def test_a_python_class_reads_its_unlinked_inputs_default_then_each_pin_value_set_live(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A Python class whose input is linked to nothing reads the 0.25 its port
    declares; a live `set_node_input_value` changes what it reads to the pin, in the
    one process it started in, and taking the pin off reads the default again.
    """
    tatolabd = start_tatolabd_running_stream(
        python_sink_reading_the_pin_value_of_its_unlinked_input
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    defaulted, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert defaulted["lowest"] == defaulted["highest"] == pytest.approx(0.25), (
        f"an unlinked input reads the default its port declares, and the class read {defaulted}"
    )

    graph = local_api.call_tool("graph", {"stream": stream_id})
    (sink,) = [node for node in graph["nodes"] if node["name"] == "sink"]

    def set_the_pin_live(value: "float | None") -> None:
        local_api.call_tool(
            "set_node_input_value",
            {
                "stream": stream_id,
                "node": sink["id"],
                "input": "brightness",
                "value": value,
                "mode": "live",
            },
        )

    set_the_pin_live(0.75)
    pinned, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert pinned["lowest"] == pinned["highest"] == pytest.approx(0.75), (
        f"a pin value set live is what the unlinked input reads next, and the class read {pinned}"
    )
    assert pinned["sink_pid"] == defaulted["sink_pid"], "a pin edit restarts nothing"
    (sink_after_the_edit,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "sink"
    ]
    (brightness,) = [
        port for port in sink_after_the_edit["ports"]["inputs"] if port["name"] == "brightness"
    ]
    assert brightness["value"] == pytest.approx(0.75), (
        f"`graph` shows the value set live as the value the input reads: {brightness}"
    )
    assert brightness["value_comes_from"] == "live", brightness
    input_values = sink_after_the_edit["input_values"]
    assert "brightness" not in input_values, (
        f"a value set live is no saved input value: {input_values}"
    )

    set_the_pin_live(None)
    defaulted_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert defaulted_again["lowest"] == defaulted_again["highest"] == pytest.approx(0.25), (
        f"taking the pin off reads the declared default again, and the class read {defaulted_again}"
    )


def test_a_function_node_input_reads_its_pin_value_until_a_link_made_live_drives_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`LevelScaledByItsGain`, its `gain` input 0.25, read by a running Python
    class. With nothing linked into `gain` the node reads its pin value. Linking
    an `Lfo` into `gain` live drives it, so the class reads the level scaled by
    the `Lfo`'s -1..1 in place of 0.25, and disconnecting it reads 0.25 again.
    """
    tatolabd = start_tatolabd_running_stream(level_scaled_by_its_gain_into_python_sink)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    configured, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert configured["lowest"] == configured["highest"] == pytest.approx(0.25), (
        f"the level 1.0 scaled by the gain 0.25, and the class read {configured}"
    )

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(
            tool, {**arguments, "stream": stream_id, "mode": "live"}
        )

    def the_scaled_node() -> dict:
        (scaled_node,) = [
            node
            for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
            if node["name"] == "scaled"
        ]
        return scaled_node

    scaled_node = the_scaled_node()
    assert "gain" in [port["name"] for port in scaled_node["ports"]["inputs"]], scaled_node
    assert "inputs_waiting_for_a_value" not in scaled_node, (
        f"an input no link drives reads its pin value, so nothing waits: {scaled_node}"
    )

    (lfo,) = edit_live("add_node", {"type": "helper_placement_function_nodes:Lfo", "name": "lfo"})["added_nodes"]
    edit_live(
        "connect",
        {
            "source": {"node": lfo["node"], "port": "result"},
            "target": {"node": scaled_node["id"], "port": "gain"},
        },
    )
    driven, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert driven["highest"] - driven["lowest"] > 0.5, (
        f"the `Lfo` linked into `gain` drives it in place of 0.25, and the class read {driven}"
    )

    edit_live(
        "disconnect",
        {
            "source": {"node": lfo["node"], "port": "result"},
            "target": {"node": scaled_node["id"], "port": "gain"},
        },
    )
    unlinked_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert unlinked_again["lowest"] == unlinked_again["highest"] == pytest.approx(0.25), (
        f"an input its link stops driving reads its pin value again, and the class read {unlinked_again}"
    )


def test_an_input_of_a_promoted_function_node_reads_its_pin_value_until_a_link_made_live_drives_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`HoldsItsLevelCountingItsCalls`, its `level` input 0.25, is stateful and
    read by two Python classes, so it runs in a helper of its own. Linking an
    `Lfo` into `level` live drives it, so the classes read the `Lfo`'s -1..1 in
    place of 0.25, and disconnecting it reads 0.25 again.
    """
    tatolabd = start_tatolabd_running_stream(held_level_read_by_two_python_sinks)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    configured, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert configured["lowest"] == configured["highest"] == pytest.approx(0.25), (
        f"the level 0.25, and a class read {configured}"
    )
    assert len(set(helper_process_ids_started_in(tatolabd.stderr_text))) == 3, (
        f"the two readers and the held node each get a helper:\n{tatolabd.recent_stderr()}"
    )

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(
            tool, {**arguments, "stream": stream_id, "mode": "live"}
        )

    (held_node,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "held"
    ]
    (lfo,) = edit_live("add_node", {"type": "helper_placement_function_nodes:Lfo", "name": "lfo"})["added_nodes"]
    edit_live(
        "connect",
        {
            "source": {"node": lfo["node"], "port": "result"},
            "target": {"node": held_node["id"], "port": "level"},
        },
    )
    driven, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    edit_live(
        "disconnect",
        {
            "source": {"node": lfo["node"], "port": "result"},
            "target": {"node": held_node["id"], "port": "level"},
        },
    )
    unlinked_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert driven["highest"] - driven["lowest"] > 0.5, (
        f"the `Lfo` linked into `level` drives it in place of 0.25, and a class read {driven}"
    )
    assert unlinked_again["lowest"] == unlinked_again["highest"] == pytest.approx(0.25), (
        f"an input its link stops driving reads its pin value again, and a class read {unlinked_again}"
    )


def test_a_setting_a_python_class_holds_reads_its_value_again_once_its_link_is_removed(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A Python class holding its `level` setting, 0.25. Linking an `Lfo` into
    `level` live drives it, and disconnecting the `Lfo` hands the class its
    0.25 again.

    Fail-without-fix: the class keeps the last value the `Lfo` gave it.
    """
    tatolabd = start_tatolabd_running_stream(python_class_reporting_its_level)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    configured, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert configured["lowest"] == configured["highest"] == pytest.approx(0.25), configured

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(
            tool, {**arguments, "stream": stream_id, "mode": "live"}
        )

    (configured_node,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "configured"
    ]
    (lfo,) = edit_live("add_node", {"type": "helper_placement_function_nodes:Lfo", "name": "lfo"})["added_nodes"]
    lfo_into_the_level = {
        "source": {"node": lfo["node"], "port": "result"},
        "target": {"node": configured_node["id"], "port": "level"},
    }
    edit_live("connect", lfo_into_the_level)
    driven, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    edit_live("disconnect", lfo_into_the_level)
    unlinked_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert driven["highest"] - driven["lowest"] > 0.5, (
        f"the `Lfo` linked into `level` drives it in place of 0.25, and the class read {driven}"
    )
    assert unlinked_again["lowest"] == unlinked_again["highest"] == pytest.approx(0.25), (
        f"a setting its link stops driving reads its value again, and the class read {unlinked_again}"
    )


def test_a_typed_node_built_with_input_values_starts_with_them_and_reopens_with_them(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A typed function node the stream builder gave `gain` 0.5 through `input_values`
    scales its level 1.0 to 0.5 from its first call, `graph` shows the 0.5 as the value its
    document saved, and the project stopped and opened again reads 0.5 again.
    """
    tatolabd = start_tatolabd_running_stream(
        typed_level_scaled_by_the_gain_its_input_values_hold_into_python_sink
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    started, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert started["lowest"] == started["highest"] == pytest.approx(0.5), (
        f"the level 1.0 scaled by the gain 0.5 its input values hold, and the class read {started}"
    )

    def the_gain_of_the_scaled_node(stream: str) -> "tuple[dict, dict]":
        (scaled,) = [
            node
            for node in local_api.call_tool("graph", {"stream": stream})["nodes"]
            if node["name"] == "scaled"
        ]
        (gain,) = [port for port in scaled["ports"]["inputs"] if port["name"] == "gain"]
        return scaled, gain

    scaled, gain = the_gain_of_the_scaled_node(stream_id)
    assert scaled["input_values"] == {"gain": 0.5}, scaled
    assert gain["value"] == pytest.approx(0.5) and gain["value_comes_from"] == "saved", gain

    first_run = tatolabd.stream_runs[-1]
    assert first_run.stop_the_stream() == 0
    project_directory = first_run.await_loaded()["project_directory"]
    queued_close = local_api.call_tool("close_project", {"project_directory": project_directory})
    assert local_api.stream_action_once_ended(queued_close["action"])["state"] == "done"
    reopened_at = time.monotonic_ns()
    tatolabd.run_stream(
        TatolabRunOfAProject(
            working_directory=first_run.working_directory,
            dir_as_typed=first_run.project_dir_argument,
        )
    )
    reopened_stream_id = tatolabd.await_the_latest_stream_loaded()
    reopened, _ = the_first_value_batch_read_after(
        tatolabd, reopened_at, from_occurrence=occurrence + 1
    )
    _, gain_reopened = the_gain_of_the_scaled_node(reopened_stream_id)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert reopened["lowest"] == reopened["highest"] == pytest.approx(0.5), (
        f"the reopened project reads the gain its document saved, and the class read {reopened}"
    )
    assert gain_reopened["value_comes_from"] == "saved", gain_reopened


def test_a_setting_of_a_python_class_added_while_playing_takes_a_link_made_in_a_later_edit(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A class reading its `width` setting, as `SolidColor` reads its size, added while the
    stream plays reads the 4 it holds; a link from a running source's `width`, made in a later
    edit, is wired and drives the setting, so the class reads 7.

    Fail-without-fix: the helper takes the setting it reads unlinked for a port already opened,
    opens no mailbox, and the link errors with `Unknown input port: width`.
    """
    tatolabd = start_tatolabd_running_stream(one_width_source_alone)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(tool, {**arguments, "stream": stream_id, "mode": "live"})

    (reader,) = edit_live(
        "add_node",
        {"type": "helper_placement_nodes:ReportsEachBatchOfTheWidthItHolds", "name": "reader"},
    )["added_nodes"]
    held, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert held["lowest"] == held["highest"] == WIDTH_HELD_UNLINKED, (
        f"the class added while playing reads the width it holds, and read {held}"
    )

    (widths,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "widths"
    ]
    edit_live(
        "connect",
        {
            "source": {"node": widths["id"], "port": "width"},
            "target": {"node": reader["node"], "port": "width"},
        },
    )
    connected_at_monotonic_ns = time.monotonic_ns()
    batches_since: "list[dict]" = []
    while len(batches_since) < BATCHES_A_LINK_MADE_LIVE_IS_GIVEN_TO_DRIVE_A_SETTING:
        batch, occurrence = the_first_value_batch_read_after(
            tatolabd, connected_at_monotonic_ns, from_occurrence=occurrence + 1
        )
        batches_since.append(batch)
        if batch["lowest"] == batch["highest"] == WIDTH_WRITTEN:
            break
    assert batches_since[-1]["lowest"] == batches_since[-1]["highest"] == WIDTH_WRITTEN, (
        f"the link made later drives the width, and the class read {batches_since}"
    )
    links = local_api.call_tool("graph", {"stream": stream_id})["links"]
    assert [link["state"] for link in links] == ["wired"], links
