# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Python processor's losses, read from `graph` on a running node.

Every Python processor runs in its own helper process and counts its losses
there. The helper mirrors each count onto a blackboard `tatolabd` created for
that spawn, and `tatolabd` renders the board under the node's `metrics` exactly
as it renders a native processor's own counts. Asserted from the outside — the
`graph` a control-plane client reads — because the failure this guards against
is a helper that lost most of its bags while its node reads healthy.

Each stream is launched with `tatolab run` from a project directory holding its
`stream.py` and processors. Running initializes a GPU context, so the whole
module needs a device.
"""

from __future__ import annotations

import os
import signal
import time
from collections.abc import Callable
from typing import Any

import pytest

from local_api_client import LocalApiClient
from runtime_process_under_test import ENGINE_STARTED_LOG_LINE, RuntimeProcessUnderTest

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

import json
import os
import time

from tatolab.stream import (
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)

BAGS_PER_TICK = 20
TICKS_PER_OVERSIZED_BAG = 50


@node(execution="continuous", interval_ms=1)
class FastBagSource:
    """Writes far faster than `SlowOrderedSink` reads, and now and then a bag no
    helper link can carry."""

    def __init__(self) -> None:
        self.ticks = 0

    @node.output()
    def bags(self) -> None: ...

    @node.output()
    def oversized_bags(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        self.ticks += 1
        for bag_index in range(BAGS_PER_TICK):
            ctx.outputs.write("bags", {{"tick": self.ticks, "bag_index": bag_index}})
        if self.ticks % TICKS_PER_OVERSIZED_BAG == 0:
            ctx.outputs.write(
                "oversized_bags", {{"padding": bytes({OVERSIZED_BAG_PADDING_BYTES})}}
            )


@node
class SlowOrderedSink:
    """Takes every bag in order, far slower than they arrive."""

    @node.input(delivery_profile="ordered")
    def bags(self) -> None: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        log.info("MARKER:SLOW_SINK_PID " + json.dumps({{"pid": os.getpid()}}))

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if ctx.inputs.read("bags") is not None:
            time.sleep(0.05)


@node
class NewestSink:
    """Drains whatever reaches it."""

    @node.input(delivery_profile="newest")
    def bags(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("bags")
'''

LOSS_COUNTING_STREAM_SOURCE = f'''\
from tatolab.stream import StreamBuilder, stream

from {LOSS_COUNTING_PROCESSORS_MODULE} import FastBagSource, NewestSink, SlowOrderedSink


@stream
def main(stream_builder: StreamBuilder) -> None:
    source = stream_builder.add(FastBagSource, name="source")
    slow_sink = stream_builder.add(SlowOrderedSink, name="slow-sink")
    oversized_sink = stream_builder.add(NewestSink, name="oversized-sink")
    stream_builder.connect(source.output("bags"), slow_sink.input("bags"))
    stream_builder.connect(source.output("oversized_bags"), oversized_sink.input("bags"))
'''

# The keys a native processor's `metrics` carries when none of its links feeds
# a windowed port.
APP_PROCESS_METRICS_KEYS = {
    "frames_dropped",
    "dropped_bags_by_link",
    "refused_bags_by_output_port",
}


def node_named(graph: "dict[str, Any]", name: str) -> "dict[str, Any]":
    """The one node carrying `name`, or a failure naming what is there."""
    matches = [node for node in graph["nodes"] if node["name"] == name]
    assert len(matches) == 1, (
        f"expected exactly one node named {name!r}; graph names "
        f"{[node['name'] for node in graph['nodes']]}"
    )
    return matches[0]


def metrics_of(local_api: LocalApiClient, name: str) -> "dict[str, Any]":
    """What `graph` renders under `name`'s `metrics`, or `{}` for no key."""
    node = node_named(local_api.call_tool("graph", {}), name)
    return node["components"].get("metrics", {})


def await_metrics_satisfying(
    local_api: LocalApiClient,
    name: str,
    satisfied: "Callable[[dict[str, Any]], bool]",
    awaited: str,
    tatolab: RuntimeProcessUnderTest,
) -> "dict[str, Any]":
    """Poll `graph` until `name`'s metrics satisfy `satisfied`."""
    deadline = time.monotonic() + COUNT_TIMEOUT_SECONDS
    metrics: "dict[str, Any]" = {}
    while time.monotonic() < deadline:
        metrics = metrics_of(local_api, name)
        if satisfied(metrics):
            return metrics
        time.sleep(0.2)
    raise AssertionError(
        f"{name!r} never rendered {awaited} within {COUNT_TIMEOUT_SECONDS}s; "
        f"its metrics were {metrics}\n{tatolab.recent_stderr()}"
    )


def launch_the_loss_counting_node(
    make_tatolab_project: "Callable[..., Any]", start_tatolab: "Callable[..., RuntimeProcessUnderTest]"
) -> "tuple[RuntimeProcessUnderTest, LocalApiClient]":
    """Write `stream.py` beside its processors, `tatolab run` it, and hand back the
    run and a client of its local API once it runs."""
    project_directory = make_tatolab_project(
        {
            "processors/__init__.py": "",
            "processors/loss_counting.py": LOSS_COUNTING_PROCESSORS_SOURCE,
            "stream.py": LOSS_COUNTING_STREAM_SOURCE,
        }
    )
    tatolab = start_tatolab(
        "run",
        working_directory=project_directory,
        extra_environment={
            "STREAMLIB_MAX_PAYLOAD_BYTES_PER_CHANNEL_UNTRUSTED_SESSION": str(HELPER_LINK_CEILING_BYTES)
        },
    )
    local_api = tatolab.local_api_client()
    tatolab.await_stderr_containing(ENGINE_STARTED_LOG_LINE)
    return tatolab, local_api


def the_link_into(graph: "dict[str, Any]", name: str) -> str:
    """The id of the one link into `name`."""
    node_named(graph, name)
    link_ids = [link["id"] for link in graph["links"] if link["target"]["node"] == name]
    assert len(link_ids) == 1, f"expected one link into {name!r}: {graph['links']}"
    return link_ids[0]


def any_dropped_bags_on(link_id: str) -> "Callable[[dict[str, Any]], bool]":
    return lambda metrics: metrics.get("dropped_bags_by_link", {}).get(link_id, 0) > 0


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_an_overrun_helper_placed_ordered_destination_renders_its_dropped_bags_per_link(
    make_tatolab_project, start_tatolab
):
    """A Python `ordered` consumer far slower than its producer loses bags in
    its own process, and its node renders them on the link they were lost on,
    under exactly the keys a native node's `metrics` carries.

    Fail-without-fix: attach no metrics for a helper-placed destination and the
    node renders no `metrics` key however many bags its helper lost.
    """
    tatolab, local_api = launch_the_loss_counting_node(make_tatolab_project, start_tatolab)
    link_id = the_link_into(local_api.call_tool("graph", {}), "slow-sink")

    metrics = await_metrics_satisfying(
        local_api, "slow-sink", any_dropped_bags_on(link_id), f"dropped bags on {link_id}", tatolab
    )

    assert set(metrics) == APP_PROCESS_METRICS_KEYS, metrics
    assert list(metrics["dropped_bags_by_link"]) == [link_id]
    assert metrics["frames_dropped"] == metrics["dropped_bags_by_link"][link_id]
    assert metrics["refused_bags_by_output_port"] == {}


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_helper_placed_producers_write_refused_at_the_ceiling_renders_on_its_output_port(
    make_tatolab_project, start_tatolab
):
    """A Python producer's bag past its helper link's ceiling is refused in its
    own process, never raised, and its node renders the refusal on the port
    that refused it — not on the destination's link, which never saw the bag.

    Fail-without-fix: count the refusal in the helper and mirror nothing, and
    the producer's `refused_bags_by_output_port` stays at zero.
    """
    tatolab, local_api = launch_the_loss_counting_node(make_tatolab_project, start_tatolab)

    metrics = await_metrics_satisfying(
        local_api,
        "source",
        lambda metrics: metrics.get("refused_bags_by_output_port", {}).get("oversized_bags", 0) > 0,
        "a refused bag on `oversized_bags`",
        tatolab,
    )

    assert set(metrics) == APP_PROCESS_METRICS_KEYS, metrics
    assert metrics["refused_bags_by_output_port"]["bags"] == 0
    assert metrics["dropped_bags_by_link"] == {}, "the source has no inbound link"
    graph = local_api.call_tool("graph", {})
    assert metrics_of(local_api, "oversized-sink")["dropped_bags_by_link"] == {
        the_link_into(graph, "oversized-sink"): 0
    }, "a bag refused before it reached any link is no loss on the destination's link"
    tatolab.await_stderr_containing("refused a", timeout=COUNT_TIMEOUT_SECONDS)


@pytest.mark.linux_only_capability(reason="only Linux resolves the runtime directory from XDG_RUNTIME_DIR")
def test_a_killed_helpers_last_counts_render_until_its_processor_is_removed(
    make_tatolab_project, start_tatolab
):
    """The board belongs to `tatolabd`: a helper killed without warning leaves
    the counts it last wrote rendering in `graph`, where they stay until the
    processor is removed.

    Fail-without-fix: drop the board with the helper, or read the counts over a
    channel to the child, and the killed sink's node renders nothing — or
    zeros — for losses that happened.
    """
    tatolab, local_api = launch_the_loss_counting_node(make_tatolab_project, start_tatolab)
    graph = local_api.call_tool("graph", {})
    link_id = the_link_into(graph, "slow-sink")
    await_metrics_satisfying(
        local_api, "slow-sink", any_dropped_bags_on(link_id), f"dropped bags on {link_id}", tatolab
    )
    slow_sink_pid = tatolab.await_marker("SLOW_SINK_PID")["pid"]
    counted_before_the_kill = metrics_of(local_api, "slow-sink")["dropped_bags_by_link"][link_id]

    os.kill(slow_sink_pid, signal.SIGKILL)
    tatolab.await_stderr_containing("its helper process (pid=", timeout=COUNT_TIMEOUT_SECONDS)

    after_the_kill = metrics_of(local_api, "slow-sink")
    time.sleep(1.0)
    a_second_later = metrics_of(local_api, "slow-sink")
    assert after_the_kill == a_second_later, "a dead helper's counts no longer move"
    assert set(after_the_kill) == APP_PROCESS_METRICS_KEYS, after_the_kill
    assert after_the_kill["dropped_bags_by_link"][link_id] >= counted_before_the_kill > 0, (
        f"the counts the helper last wrote must still render: before the kill "
        f"{counted_before_the_kill}, after {after_the_kill}"
    )

    local_api.call_tool("remove_node", {"name": "slow-sink"})
    assert all(
        rendered["name"] != "slow-sink" for rendered in local_api.call_tool("graph", {})["nodes"]
    ), "a removed node, and the counts on it, go with it"
