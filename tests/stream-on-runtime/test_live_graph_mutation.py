# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


from __future__ import annotations

import re
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any, TypeVar

import pytest

from cli_launch_projects import NODE_READY_TIMEOUT_SECONDS
from conftest import TatolabdUnderTest, TatolabRunOrDevOnTheRuntime
from local_api_client import LocalApiClient
from runtime_process_under_test import ENGINE_STARTED_LOG_LINE
from stream_runs_on_tatolabd import TatolabRunOfAProject

FIRST_FRAME_TIMEOUT_SECONDS = 30.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0
# A disconnect takes the link, not the frame the effect was already holding;
# that one still goes out, well inside this.
SECONDS_FOR_A_HELD_FRAME_TO_LEAVE = 1.0
# How long a disconnected effect's write count is watched for standing still.
SECONDS_A_STARVED_OUTPUT_IS_WATCHED = 0.5
LINK_ANSWER_TIMEOUT_SECONDS = 15.0

# The stream the test launches: one test pattern source and nothing else. Everything
# downstream of it is added live.
STREAM_WITH_ONE_PATTERN_SOURCE = '''\
from tatolab.stream import StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource


@graph_generator
def main(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource, name="pattern", input_values={"width": 320, "height": 180})
'''

# Written beside `stream.py` only after the stream is up, which is the shape an
# agent produces: nothing imported it before, and the class is named to the
# node by its import path alone.
LIVE_ADDED_EFFECT_MODULE = "nodes.live_added_effect"
LIVE_ADDED_EFFECT_CLASS = "LiveAddedEffect"
LIVE_ADDED_EFFECT_SOURCE = '''\
"""An effect that announces its frames, written after the node started."""

from typing import Annotated

from tatolab.stream import (
    ImageFrameToWrite,
    InputPin,
    NodeOutputTextureRing,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    TexturePixelFormats,
    log,
    node,
)


@node
class LiveAddedEffect:
    """Republishes each frame on a texture of its own and counts them."""

    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]]
    video_to_downstream: OutputPin[Annotated[Texture, TexturePixelFormats("rgba8_unorm")]]
    marker: str = "LIVE_FRAME"

    def __init__(self) -> None:
        self._frames = 0

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self._output_ring = NodeOutputTextureRing("rgba8_unorm", ["texture_binding"])

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = self.video_from_upstream.read()
        if frame is None:
            return
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as upstream:
            upstream.lock(read_only=True)
            try:
                pixels = upstream.as_numpy().copy()
            finally:
                upstream.unlock()
        texture = self._output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        texture.lock(read_only=False)
        try:
            texture.as_numpy()[...] = pixels
        finally:
            texture.unlock()
        self.video_to_downstream.write(
            ImageFrameToWrite.of_texture(texture, frame.colour),
            timestamp_ns=frame.timestamp_ns,
        )
        self._frames += 1
        if self._frames == 1:
            log.info(f"MARKER:{self.marker}")
'''

# A native built-in is named over the control plane by the type `graph`
# reports for one, its class in `tatolab.stream`.
LIVE_ADDED_NATIVE_BUILT_IN_TYPE = "tatolab.stream:FloatToInteger"


def test_the_live_added_effect_written_as_a_source_string_still_declares():
    """`LiveAddedEffect` lives as a triple-quoted literal, so no import, no
    linter and no AST sweep reaches it — running it here is how a bad migration
    of it fails without a device."""
    namespace: "dict[str, Any]" = {"__name__": LIVE_ADDED_EFFECT_MODULE}
    # `dont_inherit`: compiled as the file it is written to, not under this
    # module's `from __future__ import annotations`.
    exec(
        compile(LIVE_ADDED_EFFECT_SOURCE, "live_added_effect.py", "exec", dont_inherit=True),
        namespace,
    )

    effect = namespace[LIVE_ADDED_EFFECT_CLASS]
    (marker,) = [
        port for port in effect.__tatolab_node_input_ports__ if port["name"] == "marker"
    ]
    assert marker["pin_declaration"]["pin_type"] == {"kind": "is", "pin_type": {"type": "string"}}
    assert marker["pin_default"] == "LIVE_FRAME"


def node_named(graph: dict, name: str) -> dict:
    """The one node carrying `name`, or a failure naming what is there."""
    matches = [node for node in graph["nodes"] if node["name"] == name]
    assert len(matches) == 1, (
        f"expected exactly one node named {name!r}; graph names "
        f"{[node['name'] for node in graph['nodes']]}"
    )
    return matches[0]


def link_with_id(graph: dict, link_id: str) -> "dict | None":
    return next((link for link in graph["links"] if link["id"] == link_id), None)


def link_end(node_uuid: str, port: str) -> "dict[str, str]":
    """One end of a link as the graph tools name it: a node's UUID and a port name."""
    return {"node": node_uuid, "port": port}


def add_node_live(local_api: LocalApiClient, stream_id: str, arguments: dict) -> "dict[str, str]":
    """Add a node to the running stream alone, and answer its `node` UUID and `name`."""
    answered = local_api.call_tool(
        "add_node", {**arguments, "stream": stream_id, "mode": "live"}
    )
    (added,) = answered["added_nodes"]
    return added


def connect_live(
    local_api: LocalApiClient,
    stream_id: str,
    source: "dict[str, str]",
    target: "dict[str, str]",
) -> str:
    """Link `source` to `target` in the running stream alone, and answer the new link's id."""
    local_api.call_tool(
        "connect",
        {
            "stream": stream_id,
            "source": source,
            "target": target,
            "mode": "live",
        },
    )
    graph = local_api.call_tool("graph", {"stream": stream_id})
    name_of = {node["id"]: node["name"] for node in graph["nodes"]}
    matches = [
        link["id"]
        for link in graph["links"]
        if link["source"] == {"node": name_of[source["node"]], "port": source["port"]}
        and link["target"] == {"node": name_of[target["node"]], "port": target["port"]}
    ]
    assert len(matches) == 1, f"expected the one new link in {graph['links']}"
    return matches[0]


def await_link_state(
    local_api: LocalApiClient, stream_id: str, link_id: str, wanted: str
) -> str:
    deadline = time.monotonic() + LINK_ANSWER_TIMEOUT_SECONDS
    link = None
    while time.monotonic() < deadline:
        link = link_with_id(local_api.call_tool("graph", {"stream": stream_id}), link_id)
        if link is not None and link["state"] in (wanted, "error"):
            return link["state"] + (
                f" ({link['error_reason']})" if link.get("error_reason") else ""
            )
        time.sleep(0.05)
    return f"still {link['state'] if link else 'absent'} after {LINK_ANSWER_TIMEOUT_SECONDS}s"


def await_node_state(local_api: LocalApiClient, stream_id: str, name: str, wanted: str) -> str:
    """Poll the stream's `graph` until one node reaches `wanted`, and report what it reached.

    A helper-placed node reads `Running` only once its helper has finished
    setting up, which is also when every link its setup command carried is
    confirmed — so waiting for it first is what makes a later `wired` the
    helper's own answer rather than a link read before the helper was up.
    """
    deadline = time.monotonic() + FIRST_FRAME_TIMEOUT_SECONDS
    state = "absent"
    while time.monotonic() < deadline:
        graph = local_api.call_tool("graph", {"stream": stream_id})
        state = node_named(graph, name)["components"]["state"]
        if state == wanted:
            return state
        time.sleep(0.05)
    return f"still {state} after {FIRST_FRAME_TIMEOUT_SECONDS}s"


def bags_written_on(
    local_api: LocalApiClient, stream_id: str, node_name: str, output_port: str
) -> int:
    """The bags `node_name` has written on `output_port` so far, read off `graph`."""
    graph = local_api.call_tool("graph", {"stream": stream_id})
    return node_named(graph, node_name)["components"]["metrics"]["written_bags_by_output_port"][
        output_port
    ]


@dataclass(frozen=True)
class PatternStreamRunning:
    """The one-source stream, run on this test's `tatolabd`."""

    app_directory: Path
    tatolabd: TatolabdUnderTest
    tatolab_run: TatolabRunOrDevOnTheRuntime
    local_api: LocalApiClient
    stream_id: str


def start_the_pattern_stream(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    project_files: "dict[str, str]",
) -> PatternStreamRunning:
    """`tatolab open --play` on a project holding the one-source stream and `project_files`,
    once its engine has started and its source is running."""
    app_directory = make_tatolab_project(
        {"stream.py": STREAM_WITH_ONE_PATTERN_SOURCE, **project_files}
    )
    tatolabd = start_tatolabd()
    tatolab_run = tatolabd.run_stream(TatolabRunOfAProject(working_directory=app_directory))
    stream_id = tatolab_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)["stream_id"]
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    local_api = tatolabd.local_api_client()
    local_api.await_every_node_running(stream=stream_id)
    return PatternStreamRunning(
        app_directory=app_directory,
        tatolabd=tatolabd,
        tatolab_run=tatolab_run,
        local_api=local_api,
        stream_id=stream_id,
    )


@pytest.mark.requires_gpu
def test_a_node_written_after_launch_is_added_wired_and_removed_live(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    pattern_stream = start_the_pattern_stream(
        make_tatolab_project, start_tatolabd, {"nodes/__init__.py": ""}
    )
    app_directory, tatolabd, local_api, stream_id = (
        pattern_stream.app_directory,
        pattern_stream.tatolabd,
        pattern_stream.local_api,
        pattern_stream.stream_id,
    )

    graph_before = local_api.call_tool("graph", {"stream": stream_id})
    pattern = node_named(graph_before, "pattern")
    assert pattern["components"]["state"] == "Running"
    assert graph_before["links"] == []

    # The module lands beside the entry file only now, after the stream has
    # been running for a while.
    (app_directory / "nodes" / "live_added_effect.py").write_text(LIVE_ADDED_EFFECT_SOURCE)

    added = add_node_live(
        local_api,
        stream_id,
        {
            "type": f"{LIVE_ADDED_EFFECT_MODULE}:{LIVE_ADDED_EFFECT_CLASS}",
            "pins": {"marker": "FIRST_EFFECT_SAW_A_FRAME"},
            "name": "effect",
        },
    )
    assert added["name"] == "effect"

    graph_after_add = local_api.call_tool("graph", {"stream": stream_id})
    effect = node_named(graph_after_add, "effect")
    assert effect["id"] == added["node"], "the node keeps the UUID `add_node` answered"
    assert effect["type"] == f"{LIVE_ADDED_EFFECT_MODULE}:{LIVE_ADDED_EFFECT_CLASS}"
    assert effect["pins"] == {"marker": "FIRST_EFFECT_SAW_A_FRAME"}
    assert [port["name"] for port in effect["ports"]["inputs"]] == ["video_from_upstream", "marker"]
    assert [port["name"] for port in effect["ports"]["outputs"]] == ["video_to_downstream"]

    upstream_link_id = connect_live(
        local_api,
        stream_id,
        link_end(pattern["id"], "video"),
        link_end(added["node"], "video_from_upstream"),
    )

    graph_after_connect = local_api.call_tool("graph", {"stream": stream_id})
    upstream_link = link_with_id(graph_after_connect, upstream_link_id)
    assert upstream_link is not None, f"the link is missing from {graph_after_connect['links']}"
    assert upstream_link["state"] in ("pending", "wired"), (
        "`connect` onto a helper returns before that helper has opened its "
        f"port, so the link reads pending or wired and nothing else: {upstream_link}"
    )
    assert await_node_state(local_api, stream_id, "effect", "Running") == "Running"
    assert await_link_state(local_api, stream_id, upstream_link_id, "wired") == "wired", (
        "the helper's own answer is what makes the link wired; a link stuck "
        "pending is a helper that never opened its port, and one in error "
        "carries the helper's reason"
    )

    tatolabd.await_marker("FIRST_EFFECT_SAW_A_FRAME", timeout=FIRST_FRAME_TIMEOUT_SECONDS)

    # A second consumer on the SAME source output port: the channel was
    # created for the first link, and iceoryx2 pins its subscriber count then,
    # so this is the late subscriber the fixed sizing exists for.
    second = add_node_live(
        local_api,
        stream_id,
        {
            "type": f"{LIVE_ADDED_EFFECT_MODULE}:{LIVE_ADDED_EFFECT_CLASS}",
            "pins": {"marker": "SECOND_EFFECT_SAW_A_FRAME"},
            "name": "second-effect",
        },
    )
    connect_live(
        local_api,
        stream_id,
        link_end(pattern["id"], "video"),
        link_end(second["node"], "video_from_upstream"),
    )
    tatolabd.await_marker("SECOND_EFFECT_SAW_A_FRAME", timeout=FIRST_FRAME_TIMEOUT_SECONDS)

    # A node added live, consuming the helper's output: the output side of a
    # helper that already ran its setup, and a destination whose notify
    # service is created after the source was already publishing.
    downstream = add_node_live(
        local_api,
        stream_id,
        {
            "type": f"{LIVE_ADDED_EFFECT_MODULE}:{LIVE_ADDED_EFFECT_CLASS}",
            "pins": {"marker": "DOWNSTREAM_EFFECT_SAW_A_FRAME"},
            "name": "downstream-effect",
        },
    )
    connect_live(
        local_api,
        stream_id,
        link_end(added["node"], "video_to_downstream"),
        link_end(downstream["node"], "video_from_upstream"),
    )
    tatolabd.await_marker("DOWNSTREAM_EFFECT_SAW_A_FRAME", timeout=FIRST_FRAME_TIMEOUT_SECONDS)
    assert bags_written_on(local_api, stream_id, "effect", "video_to_downstream") > 0

    live_added_float_to_integer = add_node_live(
        local_api,
        stream_id,
        {
            "type": LIVE_ADDED_NATIVE_BUILT_IN_TYPE,
            "name": "live-added-float-to-integer",
        },
    )
    assert (
        await_node_state(local_api, stream_id, live_added_float_to_integer["name"], "Running")
        == "Running"
    )

    local_api.call_tool(
        "disconnect",
        {
            "stream": stream_id,
            "source": link_end(pattern["id"], "video"),
            "target": link_end(added["node"], "video_from_upstream"),
            "mode": "live",
        },
    )
    graph_after_disconnect = local_api.call_tool("graph", {"stream": stream_id})
    assert link_with_id(graph_after_disconnect, upstream_link_id) is None
    # Nothing feeds the effect any more, so nothing leaves it — once the frame
    # it already held when the link went has gone out. A write count that then
    # stands still across a window is the disconnect taking.
    time.sleep(SECONDS_FOR_A_HELD_FRAME_TO_LEAVE)
    written_once_starved = bags_written_on(local_api, stream_id, "effect", "video_to_downstream")
    time.sleep(SECONDS_A_STARVED_OUTPUT_IS_WATCHED)
    assert (
        bags_written_on(local_api, stream_id, "effect", "video_to_downstream")
        == written_once_starved
    ), "bags still leave a disconnected effect"

    removed = local_api.call_tool(
        "remove_node",
        {"stream": stream_id, "node": added["node"], "mode": "live"},
    )
    assert [operation["operation"] for operation in removed["operations"]] == ["remove_node"]
    graph_after_remove = local_api.call_tool("graph", {"stream": stream_id})
    assert all(node["name"] != "effect" for node in graph_after_remove["nodes"])
    assert all(
        "effect" not in (link["source"]["node"], link["target"]["node"])
        for link in graph_after_remove["links"]
    ), f"a removed node's links must go with it: {graph_after_remove['links']}"
    # The rest of the graph is untouched by the removal.
    assert node_named(graph_after_remove, "second-effect")["components"]["state"] == "Running"
    assert node_named(graph_after_remove, "downstream-effect")["components"]["state"] == "Running"

    assert pattern_stream.tatolab_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


# How long the slowly importing helper below sleeps at import. The calls made
# meanwhile must each return inside half of it, which a call waiting on the
# import cannot.
HELPER_IMPORT_SECONDS = 8.0
MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS = HELPER_IMPORT_SECONDS / 2

SLOWLY_IMPORTING_SINK_MODULE = "nodes.slowly_importing_sink"
SLOWLY_IMPORTING_SINK_CLASS = "SlowlyImportingSink"
SLOWLY_IMPORTING_SINK_SOURCE = f'''\
"""A sink whose node interpreter takes {HELPER_IMPORT_SECONDS} s to import it."""

import os
import time
from typing import Annotated

from tatolab.stream import InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, node

if "STREAMLIB_ENTRYPOINT" in os.environ:
    time.sleep({HELPER_IMPORT_SECONDS})


@node
class SlowlyImportingSink:
    """Reads and drops every frame."""

    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.video_from_upstream.read()
'''


Returned = TypeVar("Returned")


def seconds_taken_by(call: Callable[[], Returned]) -> "tuple[float, Returned]":
    started = time.monotonic()
    returned = call()
    return time.monotonic() - started, returned


@pytest.mark.requires_gpu
def test_graph_calls_made_while_a_helper_imports_never_wait_for_its_import(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    pattern_stream = start_the_pattern_stream(
        make_tatolab_project,
        start_tatolabd,
        {
            "nodes/__init__.py": "",
            "nodes/slowly_importing_sink.py": SLOWLY_IMPORTING_SINK_SOURCE,
        },
    )
    local_api, stream_id = pattern_stream.local_api, pattern_stream.stream_id
    pattern = node_named(local_api.call_tool("graph", {"stream": stream_id}), "pattern")

    sink = add_node_live(
        local_api,
        stream_id,
        {"type": f"{SLOWLY_IMPORTING_SINK_MODULE}:{SLOWLY_IMPORTING_SINK_CLASS}", "name": "sink"},
    )

    connect_seconds, connected_link_id = seconds_taken_by(
        lambda: connect_live(
            local_api,
            stream_id,
            link_end(pattern["id"], "video"),
            link_end(sink["node"], "video_from_upstream"),
        )
    )
    assert connect_seconds < MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS, (
        f"connect took {connect_seconds:.1f}s, waiting on a helper still importing"
    )

    graph_seconds, graph_while_importing = seconds_taken_by(
        lambda: local_api.call_tool("graph", {"stream": stream_id})
    )
    assert graph_seconds < MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS, (
        f"graph took {graph_seconds:.1f}s, waiting on a helper still importing"
    )
    link_while_importing = link_with_id(graph_while_importing, connected_link_id)
    assert link_while_importing is not None
    assert link_while_importing["state"] in ("pending", "wired"), link_while_importing

    add_seconds, _ = seconds_taken_by(
        lambda: add_node_live(
            local_api, stream_id, {"type": pattern["type"], "name": "second-pattern"}
        )
    )
    assert add_seconds < MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS, (
        f"add_node took {add_seconds:.1f}s, waiting on a helper still importing"
    )

    assert await_node_state(local_api, stream_id, "sink", "Running") == "Running"
    assert await_link_state(local_api, stream_id, connected_link_id, "wired") == "wired", (
        "the link connected during the import is wired once the helper is up"
    )

    assert pattern_stream.tatolab_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0


@pytest.mark.requires_gpu
def test_a_mutation_that_cannot_take_is_refused_by_the_call_itself(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """A change the engine cannot make is the caller's error, not a log line.

    Before the commit ran inside the call, every add and connect answered
    success and the compile failed later, out of sight; an agent then read a
    `Running` node with nothing flowing. An import path naming no class the
    project can reach is the ordinary way an agent gets this wrong.
    """
    pattern_stream = start_the_pattern_stream(make_tatolab_project, start_tatolabd, {})
    local_api, stream_id = pattern_stream.local_api, pattern_stream.stream_id

    refusal = local_api.call_tool_refusal(
        "add_node",
        {
            "stream": stream_id,
            "type": "nodes.no_such_module:Missing",
            "mode": "live",
        },
    )
    assert re.search(r"no_such_module|No module named", refusal), refusal

    graph: "dict[str, Any]" = local_api.call_tool("graph", {"stream": stream_id})
    assert [n["name"] for n in graph["nodes"] if n["name"] == "pattern"], (
        "a refused add must leave the running graph as it was"
    )

    pattern = node_named(graph, "pattern")
    port_refusal = local_api.call_tool_refusal(
        "connect",
        {
            "stream": stream_id,
            "source": link_end(pattern["id"], "no_such_port"),
            "target": link_end(pattern["id"], "video"),
            "mode": "live",
        },
    )
    assert "no_such_port" in port_refusal

    assert pattern_stream.tatolab_run.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0
