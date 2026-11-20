# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The sample that declares each kind of pin, on `tatolabd`: listed among the samples, created
from the copy the runtime unit carries, played, saved and opened again.

`examples/every-pin-kind` holds typed function and shader nodes, a Graph Input, one node not
yet typed that holds a setting, a node type whose inputs each name the control the Studio
draws, and two nodes whose Texture pins state different formats, which `connect` links through
a `TextureFormatConvert`. The pin types `graph` names here are what the Studio's
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

PICTURES_WRITTEN_TIMEOUT_SECONDS = 120.0

SAVED_DOCUMENT_TIMEOUT_SECONDS = 30.0

POLL_INTERVAL_SECONDS = 0.5

#: More than the first picture: pictures keep reaching the node not yet typed.
PICTURES_SHOWING_THE_STREAM_PLAYS = 2

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
    "bars": {"bars": "texture"},
    "bars_check": {"bars": "texture"},
}

#: The control each input of `Caption` names, which the Studio draws in place of its type's
#: default. The stream places one `Caption` that nothing reads, so its interpreter describes the
#: node type with its annotations: a node type read from source alone states no pin types.
CONTROL_NAMED_BY_EACH_CAPTION_INPUT = {
    "shown": "checkbox",
    "lines": "dial",
    "size": "dial",
    "words": "multiline",
    "style": "code",
    "align": "segmented",
    "anchor": "xy_pad",
    "font": "file_picker",
}

#: The nodes whose `process()`, or evaluation, must never raise while the stream plays.
NODES_THAT_MUST_RUN_CLEANLY = ("beat", "warmth", "glow", "tally", "caption", "bars", "bars_check")

#: More than the first converted picture: `bars_check` raises on the first one whose bars are
#: not the colours `bars` wrote.
PICTURES_SHOWING_THE_CONVERSION_RUNS = 10

#: The function nodes, which run inside the read of the stream's function node pool, since
#: `glow` is a shader node and `beat` runs as Python.
FUNCTION_NODES = ("beat", "warmth")

#: The inputs of `glow` the function nodes drive, which read from their links, not a default.
GLOW_INPUTS_FED_BY_A_LINK = ("glow", "warmth")

#: What the sample's document holds where it differs from each input's declared default, so a
#: value read at the first open comes from the document.
VALUE_THE_DOCUMENT_HOLDS_BY_NODE_AND_INPUT = {
    ("beat", "beats_per_minute"): 120.0,
    ("tally", "report_every_frames"): 20,
}

#: The values saved over the sample's: a typed input of a function node, and the setting of
#: the node not yet typed.
SAVED_VALUE_BY_NODE_AND_INPUT = {("beat", "beats_per_minute"): 100.0, ("tally", "report_every_frames"): 15}


def node_named(graph: dict[str, Any], node_name: str) -> dict[str, Any]:
    (node,) = [node for node in graph["nodes"] if node["name"] == node_name]
    return node


def pin_type_of_each_port(node: dict[str, Any]) -> dict[str, str | None]:
    return {
        port["name"]: (port.get("pin_type") or {}).get("type")
        for port in node["ports"]["inputs"] + node["ports"]["outputs"]
    }


def input_port_named(node: dict[str, Any], input_name: str) -> dict[str, Any]:
    (port,) = [port for port in node["ports"]["inputs"] if port["name"] == input_name]
    return port


def assert_the_stream_runs_cleanly_through_its_links(graph: dict[str, Any]) -> None:
    """No node raises, and `glow` reads its numbers from the function nodes' links: a broken
    function node would leave it drawing from its defaults, still writing pictures."""
    raising_because_by_node_name = {
        node_name: node_named(graph, node_name).get("components", {}).get(
            "process_raising_because"
        )
        for node_name in NODES_THAT_MUST_RUN_CLEANLY
    }
    assert raising_because_by_node_name == dict.fromkeys(NODES_THAT_MUST_RUN_CLEANLY), (
        raising_because_by_node_name
    )
    glow = node_named(graph, "glow")
    for input_name in GLOW_INPUTS_FED_BY_A_LINK:
        port = input_port_named(glow, input_name)
        assert port.get("value_comes_from") == "wire" and "value" not in port, port


def values_the_node_holds(node: dict[str, Any]) -> dict[str, Any]:
    """A node's saved values: its config's settings and the values saved on its inputs."""
    return {**(node.get("config") or {}), **node["pins"]}


def pictures_glow_wrote(graph: dict[str, Any]) -> int:
    """The pictures `glow` wrote on `result`, which links into the tally: the runtime counts
    the writes of a linked output only."""
    written = (
        node_named(graph, "glow")
        .get("components", {})
        .get("metrics", {})
        .get("written_bags_by_output_port", {})
    )
    return int(written.get("result", 0))


def every_island_glow_reads_answered_or_a_function_node_raised(graph: dict[str, Any]) -> bool:
    """Whether the function node pool has handed `glow` a value from every island it carries,
    or a function node shows a raise: until one of them, an island may not have been called."""
    if any(
        node_named(graph, node_name).get("components", {}).get("process_raising_because")
        for node_name in FUNCTION_NODES
    ):
        return True
    written_by_island_output = (
        graph.get("function_node_pool", {})
        .get("components", {})
        .get("metrics", {})
        .get("written_bags_by_output_port", {})
    )
    return bool(written_by_island_output) and all(
        int(written) > 0 for written in written_by_island_output.values()
    )


def await_pictures_through_glow(
    tatolabd: TatolabdUnderTest, local_api: LocalApiClient, stream_id: str
) -> dict[str, Any]:
    """Poll the live graph until the shader node has written pictures into the node not yet
    typed and every island it reads has answered, or a function node raised; answer that
    graph. Glow writes on its defaults too, so whether the function nodes drive it is asserted
    from the graph this answers."""
    deadline = time.monotonic() + PICTURES_WRITTEN_TIMEOUT_SECONDS
    while True:
        graph = local_api.graph(stream=stream_id)
        if (
            pictures_glow_wrote(graph) >= PICTURES_SHOWING_THE_STREAM_PLAYS
            and every_island_glow_reads_answered_or_a_function_node_raised(graph)
        ):
            return graph
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"glow wrote fewer than {PICTURES_SHOWING_THE_STREAM_PLAYS} pictures, or an "
                f"island it reads neither answered nor raised, within "
                f"{PICTURES_WRITTEN_TIMEOUT_SECONDS}s: {json.dumps(graph)[:4000]}\n"
                f"{tatolabd.recent_stderr()}"
            )
        time.sleep(POLL_INTERVAL_SECONDS)


def pictures_a_node_wrote_on(graph: dict[str, Any], node_id: str, output_name: str) -> int:
    """The pictures the node of `node_id` wrote on its linked output `output_name`."""
    (node,) = [node for node in graph["nodes"] if node["id"] == node_id]
    written = node.get("components", {}).get("metrics", {}).get("written_bags_by_output_port", {})
    return int(written.get(output_name, 0))


def assert_the_colour_bars_connect_through_a_texture_format_convert(
    tatolabd: TatolabdUnderTest,
    local_api: LocalApiClient,
    project_directory: str,
    stream_id: str,
    graph: dict[str, Any],
) -> None:
    """`connect` links the `bgra8_unorm` bars into the check that takes only `rgba8_unorm`
    through a `TextureFormatConvert` it places in the same edit; the check reads each bar back
    the colour written, and one `undo` removes the node and both links."""
    connected = local_api.call_tool(
        "connect",
        {
            "stream": stream_id,
            "source": {"node": node_named(graph, "bars")["id"], "port": "bars"},
            "target": {"node": node_named(graph, "bars_check")["id"], "port": "bars"},
        },
    )
    (inserted,) = connected["added_nodes"]
    assert inserted["node_type"] == "tatolab.stream:TextureFormatConvert", connected
    assert inserted["inserted_because"] == "converts_pin_types", connected
    assert len(connected["connected_links"]) == 2, connected

    deadline = time.monotonic() + PICTURES_WRITTEN_TIMEOUT_SECONDS
    while True:
        graph_converting = local_api.graph(stream=stream_id)
        converted = pictures_a_node_wrote_on(graph_converting, inserted["node"], "rgba_image")
        if converted >= PICTURES_SHOWING_THE_CONVERSION_RUNS:
            break
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"the TextureFormatConvert wrote {converted} pictures within "
                f"{PICTURES_WRITTEN_TIMEOUT_SECONDS}s: {json.dumps(graph_converting)[:4000]}\n"
                f"{tatolabd.recent_stderr()}"
            )
        time.sleep(POLL_INTERVAL_SECONDS)
    for node_name in ("bars", "bars_check"):
        raising_because = (
            node_named(graph_converting, node_name).get("components", {}).get(
                "process_raising_because"
            )
        )
        assert raising_because is None, (node_name, raising_because)

    local_api.call_tool("undo", {"project_directory": project_directory})
    graph_undone = local_api.graph(stream=stream_id)
    assert inserted["node"] not in {node["id"] for node in graph_undone["nodes"]}, graph_undone
    assert not [
        link
        for link in graph_undone["links"]
        if inserted["node"] in (link["source"]["node"], link["target"]["node"])
    ], graph_undone


def play(local_api: LocalApiClient, project_directory: str, stream_id: str) -> None:
    queued_playback = local_api.call_tool(
        "start_playback", {"project_directory": project_directory}
    )
    played = local_api.stream_action_once_ended(
        queued_playback["action"], timeout=PROJECT_LOAD_TIMEOUT_SECONDS
    )
    assert played["state"] == "done", json.dumps(played)
    assert {stream["stream"]: stream["state"] for stream in played["streams"]} == {
        stream_id: "running"
    }, played


def values_saved_in_the_document(main_graph_document: Path) -> dict[tuple[str, str], Any]:
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
    start_tatolabd: Callable[..., TatolabdUnderTest],
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
    graph = await_pictures_through_glow(tatolabd, local_api, stream_id)
    assert {
        node_name: {
            port: pin_type
            for port, pin_type in pin_type_of_each_port(node_named(graph, node_name)).items()
            if port in PIN_TYPE_OF_EACH_PORT_BY_NODE_NAME[node_name]
        }
        for node_name in PIN_TYPE_OF_EACH_PORT_BY_NODE_NAME
    } == PIN_TYPE_OF_EACH_PORT_BY_NODE_NAME
    assert node_named(graph, "brightness")["type"] == "tatolab.stream:GraphInput"
    (caption,) = local_api.call_tool(
        "describe_node_types",
        {"project_directory": project_directory, "node_types": ["nodes.caption:Caption"]},
    )["node_types"]
    assert {
        pin["name"]: pin.get("annotations", {}).get("control") for pin in caption["inputs"]
    } == CONTROL_NAMED_BY_EACH_CAPTION_INPUT, caption
    assert_the_stream_runs_cleanly_through_its_links(graph)
    assert_the_colour_bars_connect_through_a_texture_format_convert(
        tatolabd, local_api, project_directory, stream_id, graph
    )
    beats_per_minute = input_port_named(node_named(graph, "beat"), "beats_per_minute")
    assert beats_per_minute.get("value_comes_from") == "saved", beats_per_minute
    for (node_name, input_name), value in VALUE_THE_DOCUMENT_HOLDS_BY_NODE_AND_INPUT.items():
        assert values_the_node_holds(node_named(graph, node_name))[input_name] == value, (
            node_named(graph, node_name)
        )

    for (node_name, input_name), value in SAVED_VALUE_BY_NODE_AND_INPUT.items():
        local_api.call_tool(
            "set_node_input_value",
            {
                "stream": stream_id,
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
    graph_after_the_reopen = await_pictures_through_glow(tatolabd, local_api, stream_id)
    assert_the_stream_runs_cleanly_through_its_links(graph_after_the_reopen)
    for (node_name, input_name), value in SAVED_VALUE_BY_NODE_AND_INPUT.items():
        assert (
            values_the_node_holds(node_named(graph_after_the_reopen, node_name))[input_name]
            == value
        ), node_named(graph_after_the_reopen, node_name)

    tatolabd.interrupt()
    tatolabd.await_clean_exit()
