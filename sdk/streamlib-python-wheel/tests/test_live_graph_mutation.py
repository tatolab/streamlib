# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Changing a running node's graph over its MCP surface, end to end.

The agent loop this locks: a node is up, a processor class is written to a
module beside `app.py` *after* launch, and `add_processor` / `connect` /
`disconnect` / `remove_processor` splice it into and back out of the live
graph. Every step is asserted from the outside — the `graph` the node reports,
the bags a `tap` collects, the marker the added processor logs from its own
helper process — because the failure this guards against is a call that
answers success while nothing flows.

Booting initializes a GPU context, so the whole module needs a device.
"""

import json
import re
import time
from pathlib import Path
from typing import Callable, TypeVar

import pytest

from streamlib._control_plane_client import ControlPlaneError, call_tool
from test_cli_launch import (  # noqa: F401 — the two fixtures are used by name
    NODE_READY_TIMEOUT_SECONDS,
    LaunchedNode,
    await_sole_registry_entry,
    free_port,
    isolated_runtime_directory,
    launch_node,
)

pytestmark = pytest.mark.requires_gpu

# A helper interpreter's first frame is a cold spawn plus an import; anything
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

# The node the test launches: one native source and nothing else. Everything
# downstream of it is added live.
APP_WITH_ONE_PATTERN_SOURCE = '''\
from streamlib import Runtime, TestPatternSource


def setup(rt: Runtime) -> None:
    rt.add(TestPatternSource, config={"width": 320, "height": 180}, display_name="pattern")
'''

# Written beside `app.py` only after the node is up, which is the shape an
# agent produces: the app process never imported it, and the class is named to
# the node by its import path alone.
LIVE_ADDED_EFFECT_MODULE = "processors.live_added_effect"
LIVE_ADDED_EFFECT_CLASS = "LiveAddedEffect"
LIVE_ADDED_EFFECT_SOURCE = '''\
"""An effect that announces its frames, written after the node started."""

import dataclasses

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    ProcessorOutputTextureRing,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    output,
    processor,
)


@dataclasses.dataclass
class LiveAddedEffectConfig:
    marker: str = "LIVE_FRAME"


@processor
class LiveAddedEffect:
    """Republishes each frame on a texture of its own and counts them."""

    def __init__(self, config: LiveAddedEffectConfig) -> None:
        self.marker = config.marker
        self.frames = 0

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    @output()
    def video_to_downstream(self) -> None: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.output_ring = ProcessorOutputTextureRing("rgba8_unorm", ["texture_binding"])

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
# by the class import path `graph` reports for one, which for a Rust built-in
# is its declaration's module path.
DISPLAY_WINDOW_TYPE = "streamlib_media_builtins::display_window::DisplayWindow"


def mcp_json(control_url: str, tool_name: str, arguments: dict) -> dict:
    """One tool call, its text result decoded."""
    return json.loads(call_tool(control_url, tool_name, arguments))


def node_named(graph: dict, display_name: str) -> dict:
    """The one node carrying `display_name`, or a failure naming what is there."""
    matches = [node for node in graph["nodes"] if node["display_name"] == display_name]
    assert len(matches) == 1, (
        f"expected exactly one node named {display_name!r}; graph names "
        f"{[node['display_name'] for node in graph['nodes']]}"
    )
    return matches[0]


def link_with_id(graph: dict, link_id: str) -> "dict | None":
    return next((link for link in graph["links"] if link["id"] == link_id), None)


def await_link_state(control_url: str, link_id: str, wanted: str) -> str:
    """Poll `graph` until one link reaches `wanted`, and report what it reached.

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
        link = link_with_id(mcp_json(control_url, "graph", {}), link_id)
        if link is not None and link["state"] in (wanted, "error"):
            return link["state"] + (
                f" ({link['error_reason']})" if link.get("error_reason") else ""
            )
        time.sleep(0.05)
    return f"still {link['state'] if link else 'absent'} after {LINK_ANSWER_TIMEOUT_SECONDS}s"


def tap_channel_of(processor_id: str, output_port: str) -> str:
    """The channel name `tap` takes: the source id lowercased, then the port."""
    return f"{processor_id.lower()}/{output_port}"


def await_marker(node: LaunchedNode, marker: str) -> None:
    node.await_captured_output_containing(f"MARKER:{marker}", FIRST_FRAME_TIMEOUT_SECONDS)


def test_a_processor_written_after_launch_is_added_wired_and_removed_live(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """The whole agent loop against one node.

    Order matters and each step is checked before the next: the add spawns a
    helper that imports a module the app never saw; the first connect wires a
    link into a source that was already publishing; the second consumer of
    that same source port proves the channel was sized for a late subscriber;
    the window proves a native built-in can be added live and a helper's
    output can be wired after its setup; the disconnect stops the flow the tap
    was seeing; the remove takes the processor and its links away.
    """
    app_directory = tmp_path / "app"
    (app_directory / "processors").mkdir(parents=True)
    (app_directory / "app.py").write_text(APP_WITH_ONE_PATTERN_SOURCE)
    (app_directory / "processors" / "__init__.py").write_text("")

    node = launch_node("run", app_directory, free_port(), capture_output=True)
    entry = await_sole_registry_entry(isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS)
    control_url = entry["control_url"]
    node.await_captured_output_containing("[start] Runtime started", NODE_READY_TIMEOUT_SECONDS)

    graph_before = mcp_json(control_url, "graph", {})
    pattern = node_named(graph_before, "pattern")
    assert pattern["components"]["state"] == "Running"
    assert graph_before["links"] == []

    # The module lands beside the entry file only now, after the app process
    # has been running for a while.
    (app_directory / "processors" / "live_added_effect.py").write_text(
        LIVE_ADDED_EFFECT_SOURCE
    )

    added = mcp_json(
        control_url,
        "add_processor",
        {
            "type": f"{LIVE_ADDED_EFFECT_MODULE}:{LIVE_ADDED_EFFECT_CLASS}",
            "config": {"marker": "FIRST_EFFECT_SAW_A_FRAME"},
            "display_name": "effect",
        },
    )
    effect_id = added["processor_id"]

    graph_after_add = mcp_json(control_url, "graph", {})
    effect = node_named(graph_after_add, "effect")
    assert effect["id"] == effect_id
    assert effect["type"] == f"{LIVE_ADDED_EFFECT_MODULE}:{LIVE_ADDED_EFFECT_CLASS}"
    assert effect["config"] == {"marker": "FIRST_EFFECT_SAW_A_FRAME"}
    assert [port["name"] for port in effect["ports"]["inputs"]] == ["video_from_upstream"]
    assert [port["name"] for port in effect["ports"]["outputs"]] == ["video_to_downstream"]

    connected = mcp_json(
        control_url,
        "connect",
        {
            "from_processor_id": pattern["id"],
            "from_port": "video",
            "to_processor_id": effect_id,
            "to_port": "video_from_upstream",
        },
    )
    upstream_link_id = connected["link_id"]

    graph_after_connect = mcp_json(control_url, "graph", {})
    upstream_link = link_with_id(graph_after_connect, upstream_link_id)
    assert upstream_link is not None, f"the link is missing from {graph_after_connect['links']}"
    assert upstream_link["state"] in ("pending", "wired"), (
        "`connect` onto a helper returns before that helper has opened its "
        f"port, so the link reads pending or wired and nothing else: {upstream_link}"
    )
    assert await_link_state(control_url, upstream_link_id, "wired") == "wired", (
        "the helper's own answer is what makes the link wired; a link stuck "
        "pending is a helper that never opened its port, and one in error "
        "carries the helper's reason"
    )
    # Read again: `connect` does not wait for the helper's setup, so the graph
    # taken right after it can still show the effect setting up.
    graph_once_wired = mcp_json(control_url, "graph", {})
    assert node_named(graph_once_wired, "effect")["components"]["state"] == "Running"

    # The processor reports from its own helper process, so a marker in the
    # node's output is a frame that crossed the late-wired link.
    await_marker(node, "FIRST_EFFECT_SAW_A_FRAME")

    # A second consumer on the SAME source output port: the channel was
    # created for the first link, and iceoryx2 pins its subscriber count then,
    # so this is the late subscriber the fixed sizing exists for.
    second = mcp_json(
        control_url,
        "add_processor",
        {
            "type": f"{LIVE_ADDED_EFFECT_MODULE}:{LIVE_ADDED_EFFECT_CLASS}",
            "config": {"marker": "SECOND_EFFECT_SAW_A_FRAME"},
            "display_name": "second effect",
        },
    )
    mcp_json(
        control_url,
        "connect",
        {
            "from_processor_id": pattern["id"],
            "from_port": "video",
            "to_processor_id": second["processor_id"],
            "to_port": "video_from_upstream",
        },
    )
    await_marker(node, "SECOND_EFFECT_SAW_A_FRAME")

    # A native built-in added live, consuming the helper's output: the output
    # side of a helper that already ran its setup, and a destination whose
    # notify service is created after the source was already publishing.
    window = mcp_json(
        control_url,
        "add_processor",
        {
            "type": DISPLAY_WINDOW_TYPE,
            "config": {"title": "live-added", "scaling": "fit"},
            "display_name": "window",
        },
    )
    mcp_json(
        control_url,
        "connect",
        {
            "from_processor_id": effect_id,
            "from_port": "video_to_downstream",
            "to_processor_id": window["processor_id"],
            "to_port": "video",
        },
    )
    effect_output_channel = tap_channel_of(effect_id, "video_to_downstream")
    flowing = mcp_json(control_url, "tap", {"channel": effect_output_channel, "count": 3})
    assert flowing["received"] > 0, f"no bags left the live-added effect: {flowing}"

    mcp_json(control_url, "disconnect", {"link_id": upstream_link_id})
    graph_after_disconnect = mcp_json(control_url, "graph", {})
    assert link_with_id(graph_after_disconnect, upstream_link_id) is None
    # Nothing feeds the effect any more, so nothing leaves it — once the frame
    # it already held when the link went has gone out. A tap that then waits
    # out its window and comes back empty is the disconnect taking.
    time.sleep(SECONDS_FOR_A_HELD_FRAME_TO_LEAVE)
    starved = mcp_json(control_url, "tap", {"channel": effect_output_channel, "count": 3})
    assert starved["received"] == 0, f"bags still leave a disconnected effect: {starved}"

    mcp_json(control_url, "remove_processor", {"processor_id": effect_id})
    graph_after_remove = mcp_json(control_url, "graph", {})
    assert all(node["id"] != effect_id for node in graph_after_remove["nodes"])
    assert all(
        effect_id not in (link["source"]["processor_id"], link["target"]["processor_id"])
        for link in graph_after_remove["links"]
    ), f"a removed processor's links must go with it: {graph_after_remove['links']}"
    # The rest of the graph is untouched by the removal.
    assert node_named(graph_after_remove, "second effect")["components"]["state"] == "Running"
    assert node_named(graph_after_remove, "window")["components"]["state"] == "Running"

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, node.recent_output()


# How long the slowly importing helper below sleeps at import. The calls made
# meanwhile must each return inside half of it, which a call waiting on the
# import cannot.
HELPER_IMPORT_SECONDS = 8.0
MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS = HELPER_IMPORT_SECONDS / 2

SLOWLY_IMPORTING_SINK_MODULE = "processors.slowly_importing_sink"
SLOWLY_IMPORTING_SINK_CLASS = "SlowlyImportingSink"
# Only its helper sleeps: `STREAMLIB_ENTRYPOINT` is set in a helper process and
# nowhere else, so the app process's own import for the catalog stays quick and
# the wait lands on the helper's setup, where a torch import would.
SLOWLY_IMPORTING_SINK_SOURCE = f'''\
"""A sink whose helper takes {HELPER_IMPORT_SECONDS} s to import it."""

import os
import time

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    RuntimeContextLimitedAccess,
    input,
    processor,
)

if "STREAMLIB_ENTRYPOINT" in os.environ:
    time.sleep({HELPER_IMPORT_SECONDS})


@processor
class SlowlyImportingSink:
    """Reads and drops every frame."""

    @input(delivery_profile="newest")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("video_from_upstream")
'''


Returned = TypeVar("Returned")


def seconds_taken_by(call: Callable[[], Returned]) -> "tuple[float, Returned]":
    started = time.monotonic()
    returned = call()
    return time.monotonic() - started, returned


def test_graph_calls_made_while_a_helper_imports_never_wait_for_its_import(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """The documented live recipe — add a Python processor, connect it at once.

    The connect lands while the helper is still importing. It returns with the
    link pending rather than holding the graph until the import ends, and
    meanwhile `graph` answers and another processor is added; the link reads
    wired once the helper is up.
    """
    app_directory = tmp_path / "app"
    (app_directory / "processors").mkdir(parents=True)
    (app_directory / "app.py").write_text(APP_WITH_ONE_PATTERN_SOURCE)
    (app_directory / "processors" / "__init__.py").write_text("")
    (app_directory / "processors" / "slowly_importing_sink.py").write_text(
        SLOWLY_IMPORTING_SINK_SOURCE
    )

    node = launch_node("run", app_directory, free_port(), capture_output=True)
    entry = await_sole_registry_entry(isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS)
    control_url = entry["control_url"]
    node.await_captured_output_containing("[start] Runtime started", NODE_READY_TIMEOUT_SECONDS)
    pattern = node_named(mcp_json(control_url, "graph", {}), "pattern")

    sink = mcp_json(
        control_url,
        "add_processor",
        {
            "type": f"{SLOWLY_IMPORTING_SINK_MODULE}:{SLOWLY_IMPORTING_SINK_CLASS}",
            "display_name": "sink",
        },
    )

    connect_seconds, connected = seconds_taken_by(
        lambda: mcp_json(
            control_url,
            "connect",
            {
                "from_processor_id": pattern["id"],
                "from_port": "video",
                "to_processor_id": sink["processor_id"],
                "to_port": "video_from_upstream",
            },
        )
    )
    assert connect_seconds < MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS, (
        f"connect took {connect_seconds:.1f}s, waiting on a helper still importing"
    )

    graph_seconds, graph_while_importing = seconds_taken_by(
        lambda: mcp_json(control_url, "graph", {})
    )
    assert graph_seconds < MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS, (
        f"graph took {graph_seconds:.1f}s, waiting on a helper still importing"
    )
    link_while_importing = link_with_id(graph_while_importing, connected["link_id"])
    assert link_while_importing is not None
    assert link_while_importing["state"] in ("pending", "wired"), link_while_importing

    add_seconds, _ = seconds_taken_by(
        lambda: mcp_json(
            control_url,
            "add_processor",
            {"type": pattern["type"], "display_name": "second pattern"},
        )
    )
    assert add_seconds < MOST_A_CALL_MAY_TAKE_WHILE_A_HELPER_IMPORTS, (
        f"add_processor took {add_seconds:.1f}s, waiting on a helper still importing"
    )

    assert await_link_state(control_url, connected["link_id"], "wired") == "wired", (
        "the link connected during the import is wired once the helper is up"
    )

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, node.recent_output()


def test_a_mutation_that_cannot_take_is_refused_by_the_call_itself(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node
):
    """A change the engine cannot make is the caller's error, not a log line.

    Before the commit ran inside the call, every add and connect answered
    success and the compile failed later, out of sight; an agent then read a
    `Running` node with nothing flowing. An import path naming no class the
    app can reach is the ordinary way an agent gets this wrong.
    """
    app_directory = tmp_path / "app"
    app_directory.mkdir()
    (app_directory / "app.py").write_text(APP_WITH_ONE_PATTERN_SOURCE)

    node = launch_node("run", app_directory, free_port(), capture_output=True)
    entry = await_sole_registry_entry(isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS)
    control_url = entry["control_url"]
    node.await_captured_output_containing("[start] Runtime started", NODE_READY_TIMEOUT_SECONDS)

    with pytest.raises(ControlPlaneError) as refusal:
        call_tool(control_url, "add_processor", {"type": "processors.no_such_module:Missing"})
    assert re.search(r"no_such_module|No module named", str(refusal.value)), str(refusal.value)

    graph = mcp_json(control_url, "graph", {})
    assert [n["display_name"] for n in graph["nodes"] if n["display_name"] == "pattern"], (
        "a refused add must leave the running graph as it was"
    )

    pattern = node_named(graph, "pattern")
    with pytest.raises(ControlPlaneError) as port_refusal:
        call_tool(
            control_url,
            "connect",
            {
                "from_processor_id": pattern["id"],
                "from_port": "no_such_port",
                "to_processor_id": pattern["id"],
                "to_port": "video",
            },
        )
    assert "no_such_port" in str(port_refusal.value)

    node.interrupt()
    assert node.await_exit(CLEAN_EXIT_TIMEOUT_SECONDS) == 0, node.recent_output()
