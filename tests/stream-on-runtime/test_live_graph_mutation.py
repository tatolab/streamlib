# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Changing a running node's graph over its MCP surface, end to end.

The agent loop this locks: a stream is up on `tatolabd`, a processor class is
written to a module beside `stream.py` *after* launch, and `add_node` /
`connect` / `disconnect` / `remove_node` splice it into and back out of the
live graph. Every step is asserted from the outside — the `graph` the node
reports, the bags a `tap` collects, the marker the added processor logs from
its own processor interpreter — because the failure this guards against is a
call that answers success while nothing flows.

Starting the engine initializes a GPU context, so every test that starts one needs a device.
"""

from __future__ import annotations

import re
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any, TypeVar

import pytest

from conftest import AttachedTatolabRun, TatolabdUnderTest
from local_api_client import LocalApiClient
from runtime_process_under_test import ENGINE_STARTED_LOG_LINE
from stream_runs_on_tatolabd import TatolabRunOfAProject
from test_cli_launch import NODE_READY_TIMEOUT_SECONDS

# A processor interpreter's first frame is a cold spawn plus an import; anything
# past this is a processor that never received traffic.
FIRST_FRAME_TIMEOUT_SECONDS = 30.0
CLEAN_EXIT_TIMEOUT_SECONDS = 60.0
# A disconnect takes the link, not the frame the effect was already holding;
# that one still goes out, well inside this.
SECONDS_FOR_A_HELD_FRAME_TO_LEAVE = 1.0
# How long a link handed to a running helper has to come back `wired`. The
# helper answers between callbacks, so this is bounded by one frame of the
# processor's own work, not by the wire.
LINK_ANSWER_TIMEOUT_SECONDS = 15.0

# The stream the test launches: one native source and nothing else. Everything
# downstream of it is added live.
STREAM_WITH_ONE_PATTERN_SOURCE = '''\
from tatolab.stream import StreamBuilder, TestPatternSource, stream


@stream
def main(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource, name="pattern", config={"width": 320, "height": 180})
'''

# Written beside `stream.py` only after the stream is up, which is the shape an
# agent produces: nothing imported it before, and the class is named to the
# node by its import path alone.
LIVE_ADDED_EFFECT_MODULE = "processors.live_added_effect"
LIVE_ADDED_EFFECT_CLASS = "LiveAddedEffect"
LIVE_ADDED_EFFECT_SOURCE = '''\
"""An effect that announces its frames, written after the node started."""

import dataclasses

from tatolab.stream import (
    NodeOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    log,
    node,
)


@dataclasses.dataclass
class LiveAddedEffectConfig:
    marker: str = "LIVE_FRAME"


@node
class LiveAddedEffect:
    """Republishes each frame on a texture of its own and counts them."""

    def __init__(self, config: LiveAddedEffectConfig) -> None:
        self.marker = config.marker
        self.frames = 0

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    @node.output()
    def video_to_downstream(self) -> None: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.output_ring = NodeOutputTextureRing("rgba8_unorm", ["texture_binding"])

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None:
            return
        frame = VideoFrame.from_bag(bag)
        with ctx.gpu_limited_access.resolve_surface(frame.surface_id) as upstream:
            upstream.lock(read_only=True)
            try:
                pixels = upstream.as_numpy().copy()
            finally:
                upstream.unlock()
        texture = self.output_ring.next_texture_for_this_frame(
            ctx.gpu_limited_access, frame.width, frame.height
        )
        texture.lock(read_only=False)
        try:
            texture.as_numpy()[...] = pixels
        finally:
            texture.unlock()
        republished = dict(bag)
        republished["surface_id"] = texture.surface_id
        republished.pop("texture_layout", None)
        ctx.outputs.write("video_to_downstream", republished)
        self.frames += 1
        if self.frames == 1:
            log.info(f"MARKER:{self.marker}")
'''

# The display window is a native built-in; over the control plane it is named
# by the type `graph` reports for one, its class in `tatolab.stream`.
DISPLAY_WINDOW_TYPE = "tatolab.stream:DisplayWindow"


def test_the_live_added_effect_written_as_a_source_string_still_declares():
    """`LiveAddedEffect` lives as a triple-quoted literal, so no import, no
    linter and no AST sweep reaches it — running it here is the only way a bad
    migration of it fails anywhere but on the rig."""
    namespace: "dict[str, Any]" = {"__name__": LIVE_ADDED_EFFECT_MODULE}
    # `dont_inherit`: compiled as the file it is written to, not under this
    # module's `from __future__ import annotations`.
    exec(
        compile(LIVE_ADDED_EFFECT_SOURCE, "live_added_effect.py", "exec", dont_inherit=True),
        namespace,
    )

    effect = namespace[LIVE_ADDED_EFFECT_CLASS]
    assert effect.__tatolab_node_config_class__ is namespace["LiveAddedEffectConfig"]
    assert effect.__tatolab_node_config_schema__["properties"]["marker"] == {
        "type": "string",
        "default": "LIVE_FRAME",
    }


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


def await_link_state(
    local_api: LocalApiClient, stream_name: str, link_id: str, wanted: str
) -> str:
    """Poll the stream's `graph` until one link reaches `wanted`, and report what it reached.

    A `connect` onto a helper-placed processor returns with the link `pending`:
    the helper opens its own port and answers, and only that answer makes the
    link `wired`. A helper reads commands between callbacks, so how long that
    takes is the processor's cadence, not a fixed number — hence a poll rather
    than a sleep. A link that reaches `error` is returned as it is, so the
    caller's assertion carries the helper's own reason.
    """
    deadline = time.monotonic() + LINK_ANSWER_TIMEOUT_SECONDS
    link = None
    while time.monotonic() < deadline:
        link = link_with_id(local_api.call_tool("graph", {"stream": stream_name}), link_id)
        if link is not None and link["state"] in (wanted, "error"):
            return link["state"] + (
                f" ({link['error_reason']})" if link.get("error_reason") else ""
            )
        time.sleep(0.05)
    return f"still {link['state'] if link else 'absent'} after {LINK_ANSWER_TIMEOUT_SECONDS}s"


def await_node_state(local_api: LocalApiClient, stream_name: str, name: str, wanted: str) -> str:
    """Poll the stream's `graph` until one node reaches `wanted`, and report what it reached.

    A helper-placed node reads `Running` only once its helper has finished
    setting up, which is also when every link its setup command carried is
    confirmed — so waiting for it first is what makes a later `wired` the
    helper's own answer rather than a link read before the helper was up.
    """
    deadline = time.monotonic() + FIRST_FRAME_TIMEOUT_SECONDS
    state = "absent"
    while time.monotonic() < deadline:
        graph = local_api.call_tool("graph", {"stream": stream_name})
        state = node_named(graph, name)["components"]["state"]
        if state == wanted:
            return state
        time.sleep(0.05)
    return f"still {state} after {FIRST_FRAME_TIMEOUT_SECONDS}s"


def tap_channel_of(graph: dict, node_name: str, output_port: str) -> str:
    """The channel `tap` takes: the port's address on this runtime."""
    return f"{graph['runtime_name']}/{node_name}/{output_port}"


@dataclass(frozen=True)
class PatternStreamRunning:
    """The one-source stream, run attached on this test's `tatolabd`."""

    app_directory: Path
    tatolabd: TatolabdUnderTest
    tatolab_run: AttachedTatolabRun
    local_api: LocalApiClient
    stream_name: str


def start_the_pattern_stream(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    project_files: "dict[str, str]",
) -> PatternStreamRunning:
    """`tatolab run` on a project holding the one-source stream and `project_files`,
    once its engine has started."""
    app_directory = make_tatolab_project(
        {"stream.py": STREAM_WITH_ONE_PATTERN_SOURCE, **project_files}
    )
    tatolabd = start_tatolabd()
    tatolab_run = tatolabd.run_stream_attached(TatolabRunOfAProject(working_directory=app_directory))
    stream_name = tatolab_run.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)["stream_name"]
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    return PatternStreamRunning(
        app_directory=app_directory,
        tatolabd=tatolabd,
        tatolab_run=tatolab_run,
        local_api=tatolabd.local_api_client(),
        stream_name=stream_name,
    )


@pytest.mark.requires_gpu
def test_a_processor_written_after_launch_is_added_wired_and_removed_live(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The whole agent loop against one node.

    Order matters and each step is checked before the next: the add starts a
    processor interpreter that imports a module nothing had seen; the first
    connect wires a link into a source that was already publishing; the second
    consumer of that same source port proves the channel was sized for a late
    subscriber; the window proves a native built-in can be added live and a
    helper's output can be wired after its setup; the disconnect stops the flow
    the tap was seeing; the remove takes the processor and its links away.
    """
    pattern_stream = start_the_pattern_stream(
        make_tatolab_project, start_tatolabd, {"processors/__init__.py": ""}
    )
    app_directory, tatolabd, local_api, stream_name = (
        pattern_stream.app_directory,
        pattern_stream.tatolabd,
        pattern_stream.local_api,
        pattern_stream.stream_name,
    )

    graph_before = local_api.call_tool("graph", {"stream": stream_name})
    pattern = node_named(graph_before, "pattern")
    assert pattern["components"]["state"] == "Running"
    assert graph_before["links"] == []

    # The module lands beside the entry file only now, after the stream has
    # been running for a while.
    (app_directory / "processors" / "live_added_effect.py").write_text(LIVE_ADDED_EFFECT_SOURCE)

    added = local_api.call_tool(
        "add_node",
        {
            "stream": stream_name,
            "type": f"{LIVE_ADDED_EFFECT_MODULE}:{LIVE_ADDED_EFFECT_CLASS}",
            "config": {"marker": "FIRST_EFFECT_SAW_A_FRAME"},
            "name": "effect",
        },
    )
    assert added == {"name": "effect"}

    graph_after_add = local_api.call_tool("graph", {"stream": stream_name})
    effect = node_named(graph_after_add, "effect")
    assert effect["type"] == f"{LIVE_ADDED_EFFECT_MODULE}:{LIVE_ADDED_EFFECT_CLASS}"
    assert effect["config"] == {"marker": "FIRST_EFFECT_SAW_A_FRAME"}
    assert [port["name"] for port in effect["ports"]["inputs"]] == ["video_from_upstream"]
    assert [port["name"] for port in effect["ports"]["outputs"]] == ["video_to_downstream"]

    connected = local_api.call_tool(
        "connect",
        {
            "stream": stream_name,
            "from_node": "pattern",
            "from_port": "video",
            "to_node": "effect",
            "to_port": "video_from_upstream",
        },
    )
    upstream_link_id = connected["link_id"]

    graph_after_connect = local_api.call_tool("graph", {"stream": stream_name})
    upstream_link = link_with_id(graph_after_connect, upstream_link_id)
    assert upstream_link is not None, f"the link is missing from {graph_after_connect['links']}"
    assert upstream_link["state"] in ("pending", "wired"), (
        "`connect` onto a helper returns before that helper has opened its "
        f"port, so the link reads pending or wired and nothing else: {upstream_link}"
    )
    assert await_node_state(local_api, stream_name, "effect", "Running") == "Running"
    assert await_link_state(local_api, stream_name, upstream_link_id, "wired") == "wired", (
        "the helper's own answer is what makes the link wired; a link stuck "
        "pending is a helper that never opened its port, and one in error "
        "carries the helper's reason"
    )

    # The processor reports from its own processor interpreter, so a marker on
    # tatolabd's standard error is a frame that crossed the late-wired link.
    tatolabd.await_marker("FIRST_EFFECT_SAW_A_FRAME", timeout=FIRST_FRAME_TIMEOUT_SECONDS)

    # A second consumer on the SAME source output port: the channel was
    # created for the first link, and iceoryx2 pins its subscriber count then,
    # so this is the late subscriber the fixed sizing exists for.
    second = local_api.call_tool(
        "add_node",
        {
            "stream": stream_name,
            "type": f"{LIVE_ADDED_EFFECT_MODULE}:{LIVE_ADDED_EFFECT_CLASS}",
            "config": {"marker": "SECOND_EFFECT_SAW_A_FRAME"},
            "name": "second-effect",
        },
    )
    local_api.call_tool(
        "connect",
        {
            "stream": stream_name,
            "from_node": "pattern",
            "from_port": "video",
            "to_node": second["name"],
            "to_port": "video_from_upstream",
        },
    )
    tatolabd.await_marker("SECOND_EFFECT_SAW_A_FRAME", timeout=FIRST_FRAME_TIMEOUT_SECONDS)

    # A native built-in added live, consuming the helper's output: the output
    # side of a helper that already ran its setup, and a destination whose
    # notify service is created after the source was already publishing.
    window = local_api.call_tool(
        "add_node",
        {
            "stream": stream_name,
            "type": DISPLAY_WINDOW_TYPE,
            "config": {"title": "live-added", "scaling": "fit"},
            "name": "window",
        },
    )
    local_api.call_tool(
        "connect",
        {
            "stream": stream_name,
            "from_node": "effect",
            "from_port": "video_to_downstream",
            "to_node": window["name"],
            "to_port": "video",
        },
    )
    effect_output_channel = tap_channel_of(graph_after_add, "effect", "video_to_downstream")
    flowing = local_api.call_tool(
        "tap", {"stream": stream_name, "channel": effect_output_channel, "count": 3}
    )
    assert flowing["received"] > 0, f"no bags left the live-added effect: {flowing}"

    local_api.call_tool("disconnect", {"stream": stream_name, "link_id": upstream_link_id})
    graph_after_disconnect = local_api.call_tool("graph", {"stream": stream_name})
    assert link_with_id(graph_after_disconnect, upstream_link_id) is None
    # Nothing feeds the effect any more, so nothing leaves it — once the frame
    # it already held when the link went has gone out. A tap that then waits
    # out its window and comes back empty is the disconnect taking.
    time.sleep(SECONDS_FOR_A_HELD_FRAME_TO_LEAVE)
    starved = local_api.call_tool(
        "tap", {"stream": stream_name, "channel": effect_output_channel, "count": 3}
    )
    assert starved["received"] == 0, f"bags still leave a disconnected effect: {starved}"

    assert local_api.call_tool("remove_node", {"stream": stream_name, "name": "effect"}) == {
        "removed_name": "effect"
    }
    graph_after_remove = local_api.call_tool("graph", {"stream": stream_name})
    assert all(node["name"] != "effect" for node in graph_after_remove["nodes"])
    assert all(
        "effect" not in (link["source"]["node"], link["target"]["node"])
        for link in graph_after_remove["links"]
    ), f"a removed node's links must go with it: {graph_after_remove['links']}"
    # The rest of the graph is untouched by the removal.
    assert node_named(graph_after_remove, "second-effect")["components"]["state"] == "Running"
    assert node_named(graph_after_remove, "window")["components"]["state"] == "Running"

    pattern_stream.tatolab_run.interrupt()
    assert pattern_stream.tatolab_run.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        pattern_stream.tatolab_run.recent_stderr()
    )


# How long the slowly importing helper below sleeps at import. The calls made
# meanwhile must each return inside half of it, which a call waiting on the
# import cannot.
HELPER_IMPORT_SECONDS = 8.0
MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS = HELPER_IMPORT_SECONDS / 2

SLOWLY_IMPORTING_SINK_MODULE = "processors.slowly_importing_sink"
SLOWLY_IMPORTING_SINK_CLASS = "SlowlyImportingSink"
# Only the running processor interpreter sleeps: `STREAMLIB_ENTRYPOINT` is set
# there and nowhere else, so the describe `add_node` makes stays quick and the
# wait lands on the processor's setup, where a torch import would.
SLOWLY_IMPORTING_SINK_SOURCE = f'''\
"""A sink whose processor interpreter takes {HELPER_IMPORT_SECONDS} s to import it."""

import os
import time

from tatolab.stream import (
    RuntimeContextLimitedAccess,
    node,
)

if "STREAMLIB_ENTRYPOINT" in os.environ:
    time.sleep({HELPER_IMPORT_SECONDS})


@node
class SlowlyImportingSink:
    """Reads and drops every frame."""

    @node.input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("video_from_upstream")
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
    """The documented live recipe — add a Python processor, connect it at once.

    The connect lands while the processor interpreter is still importing. It
    returns with the link pending rather than holding the graph until the
    import ends, and meanwhile `graph` answers and another processor is added;
    the link reads wired once the helper is up.
    """
    pattern_stream = start_the_pattern_stream(
        make_tatolab_project,
        start_tatolabd,
        {
            "processors/__init__.py": "",
            "processors/slowly_importing_sink.py": SLOWLY_IMPORTING_SINK_SOURCE,
        },
    )
    local_api, stream_name = pattern_stream.local_api, pattern_stream.stream_name
    pattern = node_named(local_api.call_tool("graph", {"stream": stream_name}), "pattern")

    sink = local_api.call_tool(
        "add_node",
        {
            "stream": stream_name,
            "type": f"{SLOWLY_IMPORTING_SINK_MODULE}:{SLOWLY_IMPORTING_SINK_CLASS}",
            "name": "sink",
        },
    )

    connect_seconds, connected = seconds_taken_by(
        lambda: local_api.call_tool(
            "connect",
            {
                "stream": stream_name,
                "from_node": pattern["name"],
                "from_port": "video",
                "to_node": sink["name"],
                "to_port": "video_from_upstream",
            },
        )
    )
    assert connect_seconds < MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS, (
        f"connect took {connect_seconds:.1f}s, waiting on a helper still importing"
    )

    graph_seconds, graph_while_importing = seconds_taken_by(
        lambda: local_api.call_tool("graph", {"stream": stream_name})
    )
    assert graph_seconds < MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS, (
        f"graph took {graph_seconds:.1f}s, waiting on a helper still importing"
    )
    link_while_importing = link_with_id(graph_while_importing, connected["link_id"])
    assert link_while_importing is not None
    assert link_while_importing["state"] in ("pending", "wired"), link_while_importing

    add_seconds, _ = seconds_taken_by(
        lambda: local_api.call_tool(
            "add_node", {"stream": stream_name, "type": pattern["type"], "name": "second-pattern"}
        )
    )
    assert add_seconds < MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS, (
        f"add_node took {add_seconds:.1f}s, waiting on a helper still importing"
    )

    assert await_node_state(local_api, stream_name, "sink", "Running") == "Running"
    assert await_link_state(local_api, stream_name, connected["link_id"], "wired") == "wired", (
        "the link connected during the import is wired once the helper is up"
    )

    pattern_stream.tatolab_run.interrupt()
    assert pattern_stream.tatolab_run.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        pattern_stream.tatolab_run.recent_stderr()
    )


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
    local_api, stream_name = pattern_stream.local_api, pattern_stream.stream_name

    refusal = local_api.call_tool_refusal(
        "add_node", {"stream": stream_name, "type": "processors.no_such_module:Missing"}
    )
    assert re.search(r"no_such_module|No module named", refusal), refusal

    graph: "dict[str, Any]" = local_api.call_tool("graph", {"stream": stream_name})
    assert [n["name"] for n in graph["nodes"] if n["name"] == "pattern"], (
        "a refused add must leave the running graph as it was"
    )

    port_refusal = local_api.call_tool_refusal(
        "connect",
        {
            "stream": stream_name,
            "from_node": "pattern",
            "from_port": "no_such_port",
            "to_node": "pattern",
            "to_port": "video",
        },
    )
    assert "no_such_port" in port_refusal

    pattern_stream.tatolab_run.interrupt()
    assert pattern_stream.tatolab_run.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        pattern_stream.tatolab_run.recent_stderr()
    )
