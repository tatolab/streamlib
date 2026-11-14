# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A Python processor whose `process()` raises for a while, read from `graph`.

The node keeps running while it raises, and its `components` carry what it
raised under `process_raising_because` until a call runs cleanly again.
Asserted from outside, through the `graph` a control-plane client reads.
"""

from __future__ import annotations

import time
from collections.abc import Callable
from typing import Any

import pytest

from conftest import TatolabdUnderTest
from runtime_process_under_test import ENGINE_STARTED_LOG_LINE
from stream_runs_on_tatolabd import TatolabRunOfAProject

pytestmark = pytest.mark.requires_gpu

#: How long the probe's `process()` raises after its first call.
RAISES_FOR_SECONDS = 3.0

#: A cold spawn and import, the raising window, and room for a loaded rig.
RAISE_SEEN_AND_CLEARED_TIMEOUT_SECONDS = 30.0

RAISING_PROCESSORS_SOURCE = f'''\
"""A processor whose `process()` raises for its first seconds, then runs cleanly."""

import time

from tatolab.stream import Output, Records, RuntimeContextLimitedAccess, node


@node.process(execution="continuous", interval_ms=10)
class RaisesForItsFirstSeconds:
    """Raises on every call for {RAISES_FOR_SECONDS} s after its first."""

    nothing_to_downstream = Output(Records)

    def __init__(self) -> None:
        self.first_call_at = None

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        now = time.monotonic()
        if self.first_call_at is None:
            self.first_call_at = now
        if now - self.first_call_at < {RAISES_FOR_SECONDS}:
            raise RuntimeError("broke on purpose")
'''

RAISING_STREAM_SOURCE = '''\
from tatolab.stream import StreamBuilder, graph_generator

from processors.raising import RaisesForItsFirstSeconds


@graph_generator
def main(stream_builder: StreamBuilder) -> None:
    stream_builder.add(RaisesForItsFirstSeconds, name="raises-for-a-while")
'''


def node_named(graph: "dict[str, Any]", name: str) -> "dict[str, Any]":
    """The one node carrying `name`."""
    matches = [node for node in graph["nodes"] if node["name"] == name]
    assert len(matches) == 1, [node["name"] for node in graph["nodes"]]
    return matches[0]


def test_a_node_whose_process_raises_keeps_running_and_carries_what_it_raised_until_it_runs_cleanly(
    make_tatolab_project: "Callable[..., Any]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """While `process()` raises the node reads `Running` with the raise's words
    and where it was raised in `process_raising_because`; once a call runs
    cleanly that reads `null` and the node still runs.

    Fail-without-fix: the raise is only logged and `process_raising_because`
    is never anything but `null`.
    """
    project_directory = make_tatolab_project(
        {
            "processors/__init__.py": "",
            "processors/raising.py": RAISING_PROCESSORS_SOURCE,
            "stream.py": RAISING_STREAM_SOURCE,
        }
    )
    tatolabd = start_tatolabd()
    stream_id = tatolabd.run_stream(
        TatolabRunOfAProject(working_directory=project_directory)
    ).await_loaded()["stream_id"]
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE)
    local_api = tatolabd.local_api_client()

    def the_raising_node() -> "dict[str, Any]":
        return node_named(local_api.call_tool("graph", {"stream": stream_id}), "raises-for-a-while")

    deadline = time.monotonic() + RAISE_SEEN_AND_CLEARED_TIMEOUT_SECONDS
    raising_node = the_raising_node()
    while raising_node["components"].get("process_raising_because") is None:
        assert time.monotonic() < deadline, f"the raise never reached the graph: {raising_node}"
        time.sleep(0.1)
        raising_node = the_raising_node()

    raising_because = raising_node["components"]["process_raising_because"]
    assert raising_because.startswith("process() raised RuntimeError: broke on purpose ("), raising_because
    assert "raising.py, line " in raising_because, raising_because
    assert raising_node["components"]["state"] == "Running", raising_node
    assert raising_node["components"]["failed_because"] is None, raising_node

    while raising_node["components"]["process_raising_because"] is not None:
        assert time.monotonic() < deadline, f"the raise never cleared: {raising_node}"
        time.sleep(0.1)
        raising_node = the_raising_node()
    assert raising_node["components"]["state"] == "Running", raising_node
