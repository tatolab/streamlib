# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""An input of a Python node set through `transact` while its stream plays restarts nothing:
the probe reads the new value in the process it set up in, ran its setup once and no teardown,
took its stub exclusive device, a file lock, once and was never refused it, the stream still
runs healthy, and its graph document equals its running graph.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from helper_placement_observation import StartTatolabdRunningStream
from live_input_probe_nodes import (
    STUB_EXCLUSIVE_DEVICE_PATH_ENVIRONMENT_VARIABLE,
    StubExclusiveDevice,
)
from runtime_process_under_test import NOT_A_MARKER_LINE, MarkerLineParser
from stream_runs_on_tatolabd import write_a_hand_written_graph_project

#: The probe, its `level` saved at 0.25, as the stream `main`'s one node.
GRAPH_HOLDING_THE_PROBE = {
    "stream": "main",
    "nodes": [
        {
            "name": "probe",
            "type": "live_input_probe_nodes:CountsItsBuildsHoldingAStubDevice",
            "input_values": {"level": 0.25},
        }
    ],
    "links": [],
}

#: The markers the probe reports about its builds and its stub exclusive device.
BUILD_AND_DEVICE_MARKERS = (
    "PROBE_SETUP",
    "PROBE_TEARDOWN",
    "STUB_DEVICE_OPENED",
    "STUB_DEVICE_REFUSED",
    "STUB_DEVICE_CLOSED",
)


def the_markers_reported_in_order(stderr_text: str) -> list[tuple[str, Any]]:
    """Each build and device marker in `stderr_text`, with its payload, in the order reported."""
    parsers = [
        MarkerLineParser(marker_name) for marker_name in BUILD_AND_DEVICE_MARKERS
    ]
    reported = []
    for line in stderr_text.splitlines():
        for parser in parsers:
            payload = parser.payload_of(line)
            if payload is not NOT_A_MARKER_LINE:
                reported.append((parser.marker_name, payload))
    return reported


def assert_the_stub_device_never_had_two_holders(
    reported: list[tuple[str, Any]],
) -> None:
    """Fail when a second holder asked for the stub exclusive device while one held it."""
    refused = [
        payload
        for marker_name, payload in reported
        if marker_name == "STUB_DEVICE_REFUSED"
    ]
    assert refused == [], (
        f"two holders asked for the stub exclusive device at once: {refused}"
    )


def the_nodes_and_links_held_in(graph: dict[str, Any]) -> dict[str, Any]:
    """The nodes `graph` holds, by name, each with its type and saved input values, and its links
    by node name and port; `graph` is a graph document or a `graph` answer."""
    nodes = [
        node
        for node in graph["nodes"]
        if not node.get("components", {}).get("pending_deletion", False)
    ]
    name_of = {node.get("id", node["name"]): node["name"] for node in nodes}

    def end_of(end: dict[str, Any]) -> tuple[str, str, str | None]:
        return (name_of.get(end["node"], end["node"]), end["port"], end.get("member"))

    return {
        "nodes": {
            node["name"]: {
                "type": node["type"],
                "input_values": node.get("input_values") or {},
            }
            for node in nodes
        },
        "links": sorted(
            (end_of(link["source"]), end_of(link["target"]))
            for link in graph["links"]
            if not link.get("components", {}).get("pending_deletion", False)
        ),
    }


def assert_the_graph_document_equals_the_running_graph(
    graph_document: dict[str, Any], running: dict[str, Any]
) -> None:
    """Fail unless the document holds the nodes and links that run, with their saved values, and
    no input runs a live value the document does not hold."""
    live_inputs = [
        (node["name"], held)
        for node in running["nodes"]
        for port in node.get("ports", {}).get("inputs", [])
        for held in (port, *port.get("members", []))
        if held.get("value_comes_from") == "live"
    ]
    assert live_inputs == [], (
        f"inputs run live values the document does not hold: {live_inputs}"
    )
    assert the_nodes_and_links_held_in(graph_document) == the_nodes_and_links_held_in(
        running
    )


def test_a_second_holder_of_the_stub_device_is_refused_until_the_first_closes_it(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    """The probe's stub exclusive device takes one holder at a time, as a busy device does."""
    monkeypatch.setenv(
        STUB_EXCLUSIVE_DEVICE_PATH_ENVIRONMENT_VARIABLE,
        str(tmp_path / "stub-exclusive-device"),
    )
    first = StubExclusiveDevice()

    with pytest.raises(BlockingIOError):
        StubExclusiveDevice()
    first.close()
    StubExclusiveDevice().close()


@pytest.mark.requires_gpu
def test_an_input_of_a_python_node_set_through_transact_while_playing_restarts_nothing(
    start_tatolabd_running_stream: StartTatolabdRunningStream, tmp_path: Path
):
    """Mental-revert: a set that rebuilds the probe reports a second setup, a teardown and a
    second open of its device, or reads the new level in another process; one held as not live
    leaves the stream degraded with `edit_not_live`, the probe never reading 0.75."""
    project = write_a_hand_written_graph_project(
        tmp_path / "project", GRAPH_HOLDING_THE_PROBE
    )
    tatolabd = start_tatolabd_running_stream(
        project.tatolab_run,
        extra_environment={
            STUB_EXCLUSIVE_DEVICE_PATH_ENVIRONMENT_VARIABLE: str(
                tmp_path / "stub-exclusive-device"
            )
        },
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    first_read = tatolabd.await_marker("PROBE_READ_LEVEL")
    assert first_read["level"] == pytest.approx(0.25), first_read
    local_api = tatolabd.local_api_client()
    (probe,) = [
        node
        for node in local_api.graph(stream=stream_id)["nodes"]
        if node["name"] == "probe"
    ]

    local_api.call_tool(
        "transact",
        {
            "operations": [
                {
                    "operation": "set_node_input_value",
                    "stream": stream_id,
                    "node": probe["id"],
                    "input": "level",
                    "value": 0.75,
                }
            ]
        },
    )

    read_after_the_set = tatolabd.await_marker("PROBE_READ_LEVEL", occurrence=2)
    assert read_after_the_set == {
        "pid": first_read["pid"],
        "level": pytest.approx(0.75),
    }, f"the new level is read in the process the probe set up in: {read_after_the_set}"
    reported = the_markers_reported_in_order(tatolabd.stderr_text)
    assert_the_stub_device_never_had_two_holders(reported)
    assert [marker_name for marker_name, _ in reported] == [
        "PROBE_SETUP",
        "STUB_DEVICE_OPENED",
    ], f"one setup, one open, no teardown:\n{tatolabd.recent_stderr()}"
    (listed,) = [
        listed for listed in local_api.list_streams() if listed["stream"] == stream_id
    ]
    assert listed["state"] == "running", listed
    assert listed["condition"]["health"] == "healthy", (
        f"a stream holding an edit not live is degraded: {listed}"
    )
    assert_the_graph_document_equals_the_running_graph(
        json.loads(project.graph_document_path.read_text(encoding="utf-8")),
        local_api.graph(stream=stream_id),
    )
