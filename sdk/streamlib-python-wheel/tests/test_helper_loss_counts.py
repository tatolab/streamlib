# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Python processor's losses, read from `graph` on a running node.

Every Python processor runs in its own helper process and counts its losses
there. The helper mirrors each count onto a blackboard its parent created for
that spawn, and the parent renders the board under the node's `metrics` exactly
as it renders an app-process processor's own counts. Asserted from the outside —
the `graph` a control-plane client reads — because the failure this guards
against is a helper that lost most of its bags while its node reads healthy.

Booting initializes a GPU context, so the whole module needs a device.
"""

import json
import os
import re
import signal
import time
from pathlib import Path
from typing import Callable

import pytest

from streamlib._control_plane_client import call_tool
from test_cli_launch import (  # noqa: F401 — the two fixtures are used by name
    NODE_READY_TIMEOUT_SECONDS,
    LaunchedNode,
    await_sole_registry_entry,
    free_port,
    isolated_runtime_directory,
    launch_node,
)

pytestmark = pytest.mark.requires_gpu

# How long a count has to reach `graph` once bags are flowing. A helper's first
# bag is a cold spawn plus an import away.
COUNT_TIMEOUT_SECONDS = 30.0

# The helper-link ceiling the node runs under, lowered so an over-ceiling bag is
# cheap to build, and a bag comfortably past it.
HELPER_LINK_CEILING_BYTES = 64 * 1024
OVERSIZED_BAG_PADDING_BYTES = 256 * 1024

LOSS_COUNTING_PROCESSORS_MODULE = "processors.loss_counting"
LOSS_COUNTING_PROCESSORS_SOURCE = f'''\
"""A source faster than its ordered consumer, and a port that writes past its ceiling."""

import os
import time

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    input,
    log,
    output,
    processor,
)

BAGS_PER_TICK = 20
TICKS_PER_OVERSIZED_BAG = 50


@processor(execution="continuous", interval_ms=1)
class FastBagSource:
    """Writes far faster than `SlowOrderedSink` reads, and now and then a bag no
    helper link can carry."""

    def __init__(self) -> None:
        self.ticks = 0

    @output()
    def bags(self) -> None: ...

    @output()
    def oversized_bags(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.ticks += 1
        for bag_index in range(BAGS_PER_TICK):
            ctx.outputs.write("bags", {{"tick": self.ticks, "bag_index": bag_index}})
        if self.ticks % TICKS_PER_OVERSIZED_BAG == 0:
            ctx.outputs.write(
                "oversized_bags", {{"padding": bytes({OVERSIZED_BAG_PADDING_BYTES})}}
            )


@processor
class SlowOrderedSink:
    """Takes every bag in order, far slower than they arrive."""

    @input(delivery_profile="ordered")
    def bags(self) -> None: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        log.info(f"MARKER:SLOW_SINK_PID {{os.getpid()}}")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if ctx.inputs.read("bags") is not None:
            time.sleep(0.05)


@processor
class NewestSink:
    """Drains whatever reaches it."""

    @input(delivery_profile="newest")
    def bags(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("bags")
'''

APP_SOURCE = f'''\
from streamlib import Runtime

from {LOSS_COUNTING_PROCESSORS_MODULE} import FastBagSource, NewestSink, SlowOrderedSink


def setup(rt: Runtime) -> None:
    source = rt.add(FastBagSource, display_name="source")
    slow_sink = rt.add(SlowOrderedSink, display_name="slow sink")
    oversized_sink = rt.add(NewestSink, display_name="oversized sink")
    rt.connect(source.output("bags"), slow_sink.input("bags"))
    rt.connect(source.output("oversized_bags"), oversized_sink.input("bags"))
'''

SLOW_SINK_PID = re.compile(r"MARKER:SLOW_SINK_PID (\d+)")

# The keys an app-process processor's `metrics` carries when none of its links
# feeds a windowed port.
APP_PROCESS_METRICS_KEYS = {
    "frames_dropped",
    "dropped_bags_by_link",
    "refused_bags_by_output_port",
}


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


def metrics_of(control_url: str, display_name: str) -> dict:
    """What `graph` renders under `display_name`'s `metrics`, or `{}` for no key."""
    node = node_named(mcp_json(control_url, "graph", {}), display_name)
    return node["components"].get("metrics", {})


def await_metrics_satisfying(
    control_url: str,
    display_name: str,
    satisfied: "Callable[[dict], bool]",
    awaited: str,
    node: LaunchedNode,
) -> dict:
    """Poll `graph` until `display_name`'s metrics satisfy `satisfied`."""
    deadline = time.monotonic() + COUNT_TIMEOUT_SECONDS
    metrics: dict = {}
    while time.monotonic() < deadline:
        metrics = metrics_of(control_url, display_name)
        if satisfied(metrics):
            return metrics
        time.sleep(0.2)
    raise AssertionError(
        f"{display_name!r} never rendered {awaited} within {COUNT_TIMEOUT_SECONDS}s; "
        f"its metrics were {metrics}\n{node.recent_output()}"
    )


def launch_the_loss_counting_node(
    tmp_path: Path,
    isolated_runtime_directory: Path,
    launch_node,
    monkeypatch: pytest.MonkeyPatch,
) -> "tuple[LaunchedNode, str]":
    """Write the app beside its processors, launch it, and hand back the node and
    its control URL once it runs."""
    app_directory = tmp_path / "app"
    (app_directory / "processors").mkdir(parents=True)
    (app_directory / "processors" / "__init__.py").write_text("")
    (app_directory / "processors" / "loss_counting.py").write_text(
        LOSS_COUNTING_PROCESSORS_SOURCE
    )
    (app_directory / "app.py").write_text(APP_SOURCE)
    monkeypatch.setenv(
        "STREAMLIB_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION",
        str(HELPER_LINK_CEILING_BYTES),
    )

    node = launch_node("run", app_directory, free_port(), capture_output=True)
    entry = await_sole_registry_entry(isolated_runtime_directory, NODE_READY_TIMEOUT_SECONDS)
    node.await_captured_output_containing("[start] Runtime started", NODE_READY_TIMEOUT_SECONDS)
    return node, entry["control_url"]


def the_link_into(graph: dict, display_name: str) -> str:
    """The id of the one link into `display_name`."""
    node_id = node_named(graph, display_name)["id"]
    link_ids = [link["id"] for link in graph["links"] if link["target"]["processor_id"] == node_id]
    assert len(link_ids) == 1, f"expected one link into {display_name!r}: {graph['links']}"
    return link_ids[0]


def any_dropped_bags_on(link_id: str) -> "Callable[[dict], bool]":
    return lambda metrics: metrics.get("dropped_bags_by_link", {}).get(link_id, 0) > 0


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_an_overrun_helper_placed_ordered_destination_renders_its_dropped_bags_per_link(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node, monkeypatch
):
    """A Python `ordered` consumer far slower than its producer loses bags in
    its own process, and its node renders them on the link they were lost on,
    under exactly the keys an app-process node's `metrics` carries.

    Fail-without-fix: attach no metrics for a helper-placed destination and the
    node renders no `metrics` key however many bags its helper lost.
    """
    node, control_url = launch_the_loss_counting_node(
        tmp_path, isolated_runtime_directory, launch_node, monkeypatch
    )
    link_id = the_link_into(mcp_json(control_url, "graph", {}), "slow sink")

    metrics = await_metrics_satisfying(
        control_url,
        "slow sink",
        any_dropped_bags_on(link_id),
        f"dropped bags on {link_id}",
        node,
    )

    assert set(metrics) == APP_PROCESS_METRICS_KEYS, metrics
    assert list(metrics["dropped_bags_by_link"]) == [link_id]
    assert metrics["frames_dropped"] == metrics["dropped_bags_by_link"][link_id]
    assert metrics["refused_bags_by_output_port"] == {}


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_helper_placed_producers_write_refused_at_the_ceiling_renders_on_its_output_port(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node, monkeypatch
):
    """A Python producer's bag past its helper link's ceiling is refused in its
    own process, never raised, and its node renders the refusal on the port
    that refused it — not on the destination's link, which never saw the bag.

    Fail-without-fix: count the refusal in the helper and mirror nothing, and
    the producer's `refused_bags_by_output_port` stays at zero.
    """
    node, control_url = launch_the_loss_counting_node(
        tmp_path, isolated_runtime_directory, launch_node, monkeypatch
    )

    metrics = await_metrics_satisfying(
        control_url,
        "source",
        lambda metrics: metrics.get("refused_bags_by_output_port", {}).get("oversized_bags", 0)
        > 0,
        "a refused bag on `oversized_bags`",
        node,
    )

    assert set(metrics) == APP_PROCESS_METRICS_KEYS, metrics
    assert metrics["refused_bags_by_output_port"]["bags"] == 0
    assert metrics["dropped_bags_by_link"] == {}, "the source has no inbound link"
    graph = mcp_json(control_url, "graph", {})
    assert metrics_of(control_url, "oversized sink")["dropped_bags_by_link"] == {
        the_link_into(graph, "oversized sink"): 0
    }, "a bag refused before it reached any link is no loss on the destination's link"
    node.await_captured_output_containing(
        "refused a", COUNT_TIMEOUT_SECONDS
    )


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_killed_helpers_last_counts_render_until_its_processor_is_removed(
    tmp_path: Path, isolated_runtime_directory: Path, launch_node, monkeypatch
):
    """The board belongs to the parent: a helper killed without warning leaves
    the counts it last wrote rendering in `graph`, where they stay until the
    processor is removed.

    Fail-without-fix: drop the board with the helper, or read the counts over a
    channel to the child, and the killed sink's node renders nothing — or
    zeros — for losses that happened.
    """
    node, control_url = launch_the_loss_counting_node(
        tmp_path, isolated_runtime_directory, launch_node, monkeypatch
    )
    graph = mcp_json(control_url, "graph", {})
    slow_sink_id = node_named(graph, "slow sink")["id"]
    link_id = the_link_into(graph, "slow sink")
    await_metrics_satisfying(
        control_url,
        "slow sink",
        any_dropped_bags_on(link_id),
        f"dropped bags on {link_id}",
        node,
    )
    pid_marker = SLOW_SINK_PID.search(node.captured_output())
    assert pid_marker is not None, node.recent_output()
    counted_before_the_kill = metrics_of(control_url, "slow sink")["dropped_bags_by_link"][link_id]

    os.kill(int(pid_marker.group(1)), signal.SIGKILL)
    node.await_captured_output_containing("its helper process (pid=", COUNT_TIMEOUT_SECONDS)

    after_the_kill = metrics_of(control_url, "slow sink")
    time.sleep(1.0)
    a_second_later = metrics_of(control_url, "slow sink")
    assert after_the_kill == a_second_later, "a dead helper's counts no longer move"
    assert set(after_the_kill) == APP_PROCESS_METRICS_KEYS, after_the_kill
    assert after_the_kill["dropped_bags_by_link"][link_id] >= counted_before_the_kill > 0, (
        f"the counts the helper last wrote must still render: before the kill "
        f"{counted_before_the_kill}, after {after_the_kill}"
    )

    mcp_json(control_url, "remove_processor", {"processor_id": slow_sink_id})
    assert all(
        rendered["id"] != slow_sink_id
        for rendered in mcp_json(control_url, "graph", {})["nodes"]
    ), "a removed processor's node, and the counts on it, go with it"
