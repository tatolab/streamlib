# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What a Python node reads from an unlinked input's pin value, and from a
config field a link drives, while its stream runs.
"""

from __future__ import annotations

import time

import pytest

from helper_placement_observation import (
    StartTatolabdRunningStream,
    the_first_value_batch_read_after,
)
from helper_placement_streams import (
    clamp_with_its_input_linked_to_nothing_into_python_sink,
    held_level_read_by_two_python_sinks,
    level_scaled_by_its_gain_into_python_sink,
    lfo_into_python_sink_reporting_each_batch_it_reads,
    python_class_reporting_its_configured_level,
    python_sink_reading_the_pin_value_of_its_unlinked_input,
    typed_level_scaled_by_the_gain_its_input_values_hold_into_python_sink,
)
from helper_process_observation import (
    helper_process_ids_started_in,
)
from stream_runs_on_tatolabd import TatolabRunOfAProject


pytestmark = pytest.mark.requires_gpu


def test_a_python_class_reads_its_unlinked_inputs_default_then_each_pin_value_set_live(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A Python class whose input is linked to nothing reads the 0.25 its port
    declares; a live `set_pin_value` changes what it reads to the pin, in the
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
            "set_pin_value",
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
    pins = sink_after_the_edit["pins"]
    assert "brightness" not in pins, f"a value set live is no saved pin value: {pins}"

    set_the_pin_live(None)
    defaulted_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert defaulted_again["lowest"] == defaulted_again["highest"] == pytest.approx(0.25), (
        f"taking the pin off reads the declared default again, and the class read {defaulted_again}"
    )


def test_a_function_node_input_no_value_reaches_refuses_the_streams_start_naming_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`Clamp`'s `x` is linked to nothing, unpinned, and declares no default, so
    the stream is refused as it starts, naming the node and the input."""
    tatolabd = start_tatolabd_running_stream(clamp_with_its_input_linked_to_nothing_into_python_sink)
    failed_stream = tatolabd.stream_runs[-1].failed_stream()
    assert failed_stream is not None, "the stream must be recorded failed"
    _, failed_because = failed_stream
    assert "`clamp`'s input `x`" in failed_because, failed_because
    assert "no value reaches" in failed_because, failed_because


def test_a_function_node_linked_live_with_an_input_no_value_reaches_holds_until_its_pin_is_set(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`Clamp` linked live into a running Python class with its `x` linked to
    nothing holds: `graph` names `x` as waiting, and the class reads nothing
    from it. Setting `x`'s pin live is what the class reads next, and linking
    the `Lfo` into `x` takes the waiting input off `graph`.
    """
    tatolabd = start_tatolabd_running_stream(lfo_into_python_sink_reporting_each_batch_it_reads)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    _, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    uuid_of = {
        node["name"]: node["id"]
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
    }

    def end_of(node_uuid: str, port: str) -> "dict[str, str]":
        return {"node": node_uuid, "port": port}

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(
            tool, {**arguments, "stream": stream_id, "mode": "live"}
        )

    def the_clamp_node() -> dict:
        (clamp_node,) = [
            node
            for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
            if node["name"] == "clamp"
        ]
        return clamp_node

    (clamp,) = edit_live("add_node", {"type": "helper_placement_function_nodes:Clamp", "name": "clamp"})[
        "added_nodes"
    ]
    edit_live(
        "disconnect",
        {
            "source": end_of(uuid_of["lfo"], "result"),
            "target": end_of(uuid_of["sink"], "brightness"),
        },
    )
    edit_live(
        "connect",
        {
            "source": end_of(clamp["node"], "result"),
            "target": end_of(uuid_of["sink"], "brightness"),
        },
    )
    assert the_clamp_node().get("inputs_waiting_for_a_value") == ["x"], (
        f"`graph` names the input the held node waits on: {the_clamp_node()}"
    )

    pinned_from_ns = time.monotonic_ns()
    edit_live("set_pin_value", {"node": clamp["node"], "input": "x", "value": 0.3})
    pinned, occurrence = the_first_value_batch_read_after(
        tatolabd, pinned_from_ns, from_occurrence=occurrence + 1
    )
    assert pinned["lowest"] == pinned["highest"] == pytest.approx(0.3), (
        f"the held node evaluates once its input has a value, and the class read {pinned}"
    )
    assert "inputs_waiting_for_a_value" not in the_clamp_node()

    edit_live(
        "connect",
        {"source": end_of(uuid_of["lfo"], "result"), "target": end_of(clamp["node"], "x")},
    )
    clamped, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert 0.0 <= clamped["lowest"] and clamped["highest"] <= 1.0 and (
        clamped["highest"] - clamped["lowest"] > 0.5
    ), f"a link into `x` wins over its pin, clamped to 0..1, and the class read {clamped}"


def test_a_config_field_exposed_live_reads_the_config_until_a_link_drives_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`LevelScaledByItsGain`, its config's `gain` 0.25, read by a running Python
    class. Exposing `gain` live adds a `gain` input that `graph` shows, and with
    nothing linked into it the node still reads the config. Linking an `Lfo` into
    `gain` drives the field, so the class reads the level scaled by the `Lfo`'s
    -1..1 in place of 0.25, and disconnecting it reads the config's 0.25 again.
    """
    tatolabd = start_tatolabd_running_stream(level_scaled_by_its_gain_into_python_sink)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    configured, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert configured["lowest"] == configured["highest"] == pytest.approx(0.25), (
        f"the level 1.0 scaled by the config's gain 0.25, and the class read {configured}"
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

    edit_live(
        "set_exposed_config_fields",
        {"node": the_scaled_node()["id"], "exposed_config_fields": ["gain"]},
    )
    scaled_node = the_scaled_node()
    assert scaled_node.get("exposed_config_fields") == ["gain"], scaled_node
    assert "gain" in [port["name"] for port in scaled_node["ports"]["inputs"]], scaled_node
    assert "inputs_waiting_for_a_value" not in scaled_node, (
        f"an exposed field no link drives reads the config, so nothing waits: {scaled_node}"
    )
    exposed_unlinked, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert exposed_unlinked["lowest"] == exposed_unlinked["highest"] == pytest.approx(0.25), (
        f"an exposed field no link drives reads the config, and the class read {exposed_unlinked}"
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
        f"the `Lfo` linked into `gain` drives the field in place of 0.25, and the class read {driven}"
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
        f"a field its link stops driving reads the config again, and the class read {unlinked_again}"
    )


def test_a_config_field_a_promoted_function_node_exposes_reads_the_config_until_a_link_drives_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`HoldsItsConfiguredLevelCountingItsCalls`, its config's `level` 0.25, is
    stateful and read by two Python classes, so it runs in a helper of its own.
    Exposing `level` live with nothing linked into it leaves the node reading the
    config. Linking an `Lfo` into `level` drives the field, so the classes read
    the `Lfo`'s -1..1 in place of 0.25, and disconnecting it reads the config's
    0.25 again.

    Fail-without-fix: the promoted node keeps its config's 0.25 with the `Lfo`
    linked, and its helper logs that it cannot be reconfigured while running.
    """
    tatolabd = start_tatolabd_running_stream(held_level_read_by_two_python_sinks)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    configured, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert configured["lowest"] == configured["highest"] == pytest.approx(0.25), (
        f"the config's level 0.25, and a class read {configured}"
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
    edit_live(
        "set_exposed_config_fields",
        {"node": held_node["id"], "exposed_config_fields": ["level"]},
    )
    exposed_unlinked, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert exposed_unlinked["lowest"] == exposed_unlinked["highest"] == pytest.approx(0.25), (
        f"an exposed field no link drives reads the config, and a class read {exposed_unlinked}"
    )

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
        f"the `Lfo` linked into `level` drives the field in place of 0.25, and a class read {driven}"
    )
    assert unlinked_again["lowest"] == unlinked_again["highest"] == pytest.approx(0.25), (
        f"a field its link stops driving reads the config again, and a class read {unlinked_again}"
    )
    assert "cannot be reconfigured while running" not in tatolabd.stderr_text, (
        f"the promoted node takes the linked value as its config:\n{tatolabd.recent_stderr()}"
    )


def test_a_config_field_a_python_class_exposes_reads_the_config_again_once_its_link_is_removed(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A Python class taking live config updates, its config's `level` 0.25.
    Exposing `level` live and linking an `Lfo` into it drives the field, and
    disconnecting the `Lfo` hands the class its config's 0.25 again.

    Fail-without-fix: the class keeps the last value the `Lfo` gave it.
    """
    tatolabd = start_tatolabd_running_stream(python_class_reporting_its_configured_level)
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
    edit_live(
        "set_exposed_config_fields",
        {"node": configured_node["id"], "exposed_config_fields": ["level"]},
    )
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
        f"the `Lfo` linked into `level` drives the field in place of 0.25, and the class read {driven}"
    )
    assert unlinked_again["lowest"] == unlinked_again["highest"] == pytest.approx(0.25), (
        f"a field its link stops driving reads the config again, and the class read {unlinked_again}"
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
