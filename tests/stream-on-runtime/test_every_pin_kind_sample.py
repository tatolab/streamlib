# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The sample that declares each kind of pin, on `tatolabd`: listed among the samples, created
from the copy the runtime unit carries, played, saved and opened again.

`examples/every-pin-kind` holds typed function and shader nodes, a Graph Input, and one node
not yet typed that holds a setting. The pin types `graph` names here are what the Studio's
recordings are made from, so each later type, mode and flow joins the sample and this check.
"""

from __future__ import annotations

import json
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from conftest import TatolabdUnderTest
from local_api_client import LocalApiClient

pytestmark = pytest.mark.requires_gpu

SAMPLE_NAME = "every-pin-kind"

#: The sample's first load makes its venv.
PROJECT_LOAD_TIMEOUT_SECONDS = 900.0

TALLIES_WRITTEN_TIMEOUT_SECONDS = 120.0

SAVED_DOCUMENT_TIMEOUT_SECONDS = 30.0

POLL_INTERVAL_SECONDS = 0.5

#: More than the first tally: the picture keeps reaching the node not yet typed.
TALLIES_SHOWING_THE_STREAM_PLAYS = 2

#: Each port of each node by name, and the pin type `graph` names on it: the `type` of a typed
#: pin, `None` on a pin that is not typed.
PIN_TYPE_OF_EACH_PORT_BY_NODE_NAME = {
    "beat": {
        "level": "float",
        "beats_per_minute": "float",
        "accent_every": "integer",
        "accent": "boolean",
        "glow": "float",
        "count": "integer",
        "accented": "boolean",
    },
    "warmth": {
        "count": "integer",
        "accented": "boolean",
        "beats_per_bar": "integer",
        "base": "float",
        "result": "float",
    },
    "glow": {"image": "texture", "glow": "float", "warmth": "float", "result": "texture"},
    "tally": {"picture": "texture", "tally": None},
}

#: The values saved over the sample's: a typed input of a function node, and the setting of
#: the node not yet typed.
SAVED_VALUE_BY_NODE_AND_INPUT = {("beat", "beats_per_minute"): 100.0, ("tally", "report_every_frames"): 15}


def node_named(graph: "dict[str, Any]", node_name: str) -> "dict[str, Any]":
    (node,) = [node for node in graph["nodes"] if node["name"] == node_name]
    return node


def pin_type_of_each_port(node: "dict[str, Any]") -> "dict[str, str | None]":
    return {
        port["name"]: (port.get("pin_type") or {}).get("type")
        for port in node["ports"]["inputs"] + node["ports"]["outputs"]
    }


def values_the_node_holds(node: "dict[str, Any]") -> "dict[str, Any]":
    """A node's saved values: its config's settings and the values saved on its inputs."""
    return {**(node.get("config") or {}), **node["pins"]}


def tallies_written(graph: "dict[str, Any]") -> int:
    written = (
        node_named(graph, "tally")
        .get("components", {})
        .get("metrics", {})
        .get("written_bags_by_output_port", {})
    )
    return int(written.get("tally", 0))


def await_tallies_written(
    tatolabd: TatolabdUnderTest, local_api: LocalApiClient, stream_id: str
) -> "dict[str, Any]":
    """Poll the live graph until the node not yet typed has written tallies, so pictures pass
    from the test pattern through the shader node driven by the function nodes; answer that
    graph."""
    deadline = time.monotonic() + TALLIES_WRITTEN_TIMEOUT_SECONDS
    while True:
        graph = local_api.graph(stream=stream_id)
        if tallies_written(graph) >= TALLIES_SHOWING_THE_STREAM_PLAYS:
            return graph
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"the tally wrote fewer than {TALLIES_SHOWING_THE_STREAM_PLAYS} records within "
                f"{TALLIES_WRITTEN_TIMEOUT_SECONDS}s: {json.dumps(graph)[:4000]}\n"
                f"{tatolabd.recent_stderr()}"
            )
        time.sleep(POLL_INTERVAL_SECONDS)


def play(local_api: LocalApiClient, project_directory: str, stream_id: str) -> None:
    queued_playback = local_api.call_tool(
        "start_playback", {"project_directory": project_directory}
    )
    played = local_api.stream_action_once_ended(
        queued_playback["action"], timeout=PROJECT_LOAD_TIMEOUT_SECONDS
    )
    assert played["state"] == "done", played
    assert {stream["stream"]: stream["state"] for stream in played["streams"]} == {
        stream_id: "running"
    }, played


def values_saved_in_the_document(main_graph_document: Path) -> "dict[tuple[str, str], Any]":
    nodes_by_name = {
        node["name"]: node
        for node in json.loads(main_graph_document.read_text(encoding="utf-8"))["nodes"]
    }
    return {
        (node_name, input_name): nodes_by_name[node_name].get("input_values", {}).get(input_name)
        for node_name, input_name in SAVED_VALUE_BY_NODE_AND_INPUT
    }


def await_the_values_saved_in_the_document(main_graph_document: Path) -> None:
    deadline = time.monotonic() + SAVED_DOCUMENT_TIMEOUT_SECONDS
    while True:
        saved = values_saved_in_the_document(main_graph_document)
        if saved == SAVED_VALUE_BY_NODE_AND_INPUT:
            return
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"{main_graph_document} holds {saved}, not {SAVED_VALUE_BY_NODE_AND_INPUT}, "
                f"{SAVED_DOCUMENT_TIMEOUT_SECONDS}s after the saves"
            )
        time.sleep(POLL_INTERVAL_SECONDS)


def test_the_sample_declaring_each_kind_of_pin_opens_plays_saves_and_reopens(
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    tatolabd = start_tatolabd()
    local_api = tatolabd.local_api_client()
    listed_samples = local_api.call_tool("list_samples", {})["samples"]
    assert SAMPLE_NAME in {sample["name"] for sample in listed_samples}, listed_samples

    created = local_api.call_tool(
        "create_project",
        {"project_directory": str(tmp_path / "pins"), "sample": SAMPLE_NAME},
    )
    project_directory = created["project_directory"]
    stream_id = created["stream"]
    opened = local_api.stream_action_once_ended(
        created["action"], timeout=PROJECT_LOAD_TIMEOUT_SECONDS
    )
    assert opened["state"] == "done", opened

    play(local_api, project_directory, stream_id)
    graph = await_tallies_written(tatolabd, local_api, stream_id)
    assert {
        node_name: {
            port: pin_type
            for port, pin_type in pin_type_of_each_port(node_named(graph, node_name)).items()
            if port in PIN_TYPE_OF_EACH_PORT_BY_NODE_NAME[node_name]
        }
        for node_name in PIN_TYPE_OF_EACH_PORT_BY_NODE_NAME
    } == PIN_TYPE_OF_EACH_PORT_BY_NODE_NAME
    assert node_named(graph, "brightness")["type"] == "tatolab.stream:GraphInput"
    assert values_the_node_holds(node_named(graph, "tally"))["report_every_frames"] == 30

    stream_asset = local_api.stream_asset_of(stream_id)
    for (node_name, input_name), value in SAVED_VALUE_BY_NODE_AND_INPUT.items():
        local_api.call_tool(
            "set_node_input_value",
            {
                "stream": stream_asset,
                "node": node_named(graph, node_name)["id"],
                "input": input_name,
                "value": value,
                "mode": "commit",
            },
        )
    await_the_values_saved_in_the_document(
        Path(project_directory) / "streams" / "main.graph.json"
    )

    queued_close = local_api.call_tool("close_project", {"project_directory": project_directory})
    assert local_api.stream_action_once_ended(queued_close["action"])["state"] == "done"
    queued_open = local_api.call_tool("open_project", {"project_directory": project_directory})
    reopened = local_api.stream_action_once_ended(
        queued_open["action"], timeout=PROJECT_LOAD_TIMEOUT_SECONDS
    )
    assert reopened["state"] == "done", reopened
    play(local_api, project_directory, stream_id)
    graph_after_the_reopen = await_tallies_written(tatolabd, local_api, stream_id)
    for (node_name, input_name), value in SAVED_VALUE_BY_NODE_AND_INPUT.items():
        assert (
            values_the_node_holds(node_named(graph_after_the_reopen, node_name))[input_name]
            == value
        ), node_named(graph_after_the_reopen, node_name)

    tatolabd.interrupt()
    tatolabd.await_clean_exit()
