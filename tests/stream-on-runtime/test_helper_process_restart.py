# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A Python node whose helper process dies on its own, read from `graph`.

The engine starts a new helper — a new instance of the class in a new process —
in place of one that died, after a backoff, as the same node. A helper that
keeps dying soon after each start is given up on: its node is left `Error` with
the last death's reason, and the rest of the stream keeps running. Asserted
from outside, through the `graph` a control-plane client reads.
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

#: The waits between five deaths in a row — one, two, four and eight seconds —
#: and a cold spawn and import for each start, with room for a loaded rig.
CRASH_LOOP_TIMEOUT_SECONDS = 60.0

CRASHING_NODES_SOURCE = '''\
"""A node whose helper process exits a moment after every start, beside one that keeps running."""

from typing import Callable
import json
import os

from tatolab.stream import (
    OutputPin,
    RuntimeContextLimitedAccess,
    Trigger,
    log,
    node,
)


@node(execution="continuous", interval_ms=10)
def ExitsSoonAfterEveryStart(
    *,
    nothing_to_downstream: OutputPin[Trigger],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Ends its own process on its tenth callback."""

    callbacks = 0

    log.info("MARKER:EXITING_PROBE_STARTED " + json.dumps({"pid": os.getpid()}))

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal callbacks
        callbacks += 1
        if callbacks == 10:
            os._exit(17)

    return each_frame


@node(execution="continuous", interval_ms=10)
def KeepsRunning(
    *,
    nothing_to_downstream: OutputPin[Trigger],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Does nothing, for as long as the stream runs."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        pass

    return each_frame
'''

CRASHING_STREAM_SOURCE = '''\
from tatolab.stream import StreamBuilder, graph_generator

from nodes.crashing import ExitsSoonAfterEveryStart, KeepsRunning


@graph_generator
def main(stream_builder: StreamBuilder) -> None:
    stream_builder.add(ExitsSoonAfterEveryStart, name="exits-soon")
    stream_builder.add(KeepsRunning, name="keeps-running")
'''


def node_named(graph: "dict[str, Any]", name: str) -> "dict[str, Any]":
    """The one node carrying `name`."""
    matches = [node for node in graph["nodes"] if node["name"] == name]
    assert len(matches) == 1, [node["name"] for node in graph["nodes"]]
    return matches[0]


def test_a_helper_dying_soon_after_every_start_is_restarted_four_times_then_left_in_error(
    make_tatolab_project: "Callable[..., Any]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """Each death starts a new helper process until the fifth death in a row,
    each within a minute of its start, leaves the node `Error` naming the crash
    loop and the last exit; the stream's other node keeps running throughout.

    Fail-without-fix: latch the first death and only one helper ever starts;
    drop the crash-loop limit and the node reads `Restarting` for good.
    """
    project_directory = make_tatolab_project(
        {
            "nodes/__init__.py": "",
            "nodes/crashing.py": CRASHING_NODES_SOURCE,
            "stream.py": CRASHING_STREAM_SOURCE,
        }
    )
    tatolabd = start_tatolabd()
    stream_id = tatolabd.run_stream(
        TatolabRunOfAProject(working_directory=project_directory)
    ).await_loaded()["stream_id"]
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE)
    local_api = tatolabd.local_api_client()

    def graph() -> "dict[str, Any]":
        return local_api.call_tool("graph", {"stream": stream_id})

    helper_process_ids = {
        tatolabd.await_marker(
            "EXITING_PROBE_STARTED", occurrence=start, timeout=CRASH_LOOP_TIMEOUT_SECONDS
        )["pid"]
        for start in range(1, 6)
    }
    assert len(helper_process_ids) == 5, "every restart is a new helper process"

    deadline = time.monotonic() + CRASH_LOOP_TIMEOUT_SECONDS
    exits_soon = node_named(graph(), "exits-soon")
    while exits_soon["components"]["state"] != "Error" and time.monotonic() < deadline:
        time.sleep(0.2)
        exits_soon = node_named(graph(), "exits-soon")

    assert exits_soon["components"]["state"] == "Error", exits_soon
    restarts = exits_soon["components"]["helper_process_restarts"]
    assert restarts["restart_count"] == 4, restarts
    assert "exit status: 17" in restarts["last_exit_reason"], restarts
    tatolabd.await_stderr_containing("died 5 times in a row")
    assert node_named(graph(), "keeps-running")["components"]["state"] == "Running"
    time.sleep(3.0)
    assert node_named(graph(), "exits-soon")["components"]["state"] == "Error", (
        "a node past its crash-loop limit stays down"
    )


DIES_IN_ITS_SECOND_SETUP_NODES_SOURCE = '''\
"""A reader whose helper dies a moment after its first start, then again in its second setup,
then keeps running, beside a source that keeps running."""

import json
import os
import time
from pathlib import Path
from typing import Callable

from tatolab.stream import (
    InputPin,
    OutputPin,
    RuntimeContextLimitedAccess,
    Trigger,
    log,
    node,
)

STARTS_COUNTED_IN = Path(__file__).with_name("reader_starts")
LINK_CONNECTED_IN_THE_SECOND_SETUP = Path(__file__).with_name("link_connected")


@node(execution="continuous", interval_ms=10)
def KeepsRunning(*, ticks: OutputPin[Trigger]) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Does nothing, for as long as the stream runs."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        pass

    return each_frame


@node(execution="continuous", interval_ms=10)
def DiesInItsSecondSetup(
    *,
    ticks_from_upstream: InputPin[Trigger],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Ends its first process on its tenth frame, and its second in its body once the test
    has connected the link."""
    start = (int(STARTS_COUNTED_IN.read_text()) if STARTS_COUNTED_IN.exists() else 0) + 1
    STARTS_COUNTED_IN.write_text(str(start))
    log.info("MARKER:READER_SETTING_UP " + json.dumps({"start": start}))
    if start == 2:
        deadline = time.monotonic() + 30.0
        while not LINK_CONNECTED_IN_THE_SECOND_SETUP.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        os._exit(18)
    frames = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal frames
        frames += 1
        if start == 1 and frames == 10:
            os._exit(17)

    return each_frame
'''

DIES_IN_ITS_SECOND_SETUP_STREAM_SOURCE = '''\
from tatolab.stream import StreamBuilder, graph_generator

from nodes.dying_reader import DiesInItsSecondSetup, KeepsRunning


@graph_generator
def main(stream_builder: StreamBuilder) -> None:
    stream_builder.add(KeepsRunning, name="source")
    stream_builder.add(DiesInItsSecondSetup, name="reader")
'''

#: The second start's setup, the death that ends it, the two-second wait before the third start,
#: a cold spawn and import, and the wait before the link is wired again, with room for a loaded rig.
REFUSED_LINK_WIRED_AGAIN_TIMEOUT_SECONDS = 45.0


def test_a_link_its_reader_refused_by_dying_in_setup_is_wired_again_once_its_next_helper_runs(
    make_tatolab_project: "Callable[..., Any]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """A link connected live while its reader's helper is still setting up is refused when that
    helper dies before answering, and is wired by itself once the next helper is running, with
    no stop of the stream.

    Fail-without-fix: the link reads `error` for good, and its diagnostic asks for the stream to
    be stopped and started again.
    """
    project_directory = make_tatolab_project(
        {
            "nodes/__init__.py": "",
            "nodes/dying_reader.py": DIES_IN_ITS_SECOND_SETUP_NODES_SOURCE,
            "stream.py": DIES_IN_ITS_SECOND_SETUP_STREAM_SOURCE,
        }
    )
    tatolabd = start_tatolabd()
    stream_id = tatolabd.run_stream(
        TatolabRunOfAProject(working_directory=project_directory)
    ).await_loaded()["stream_id"]
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE)
    local_api = tatolabd.local_api_client()

    def graph() -> "dict[str, Any]":
        return local_api.call_tool("graph", {"stream": stream_id})

    tatolabd.await_marker(
        "READER_SETTING_UP", occurrence=2, timeout=REFUSED_LINK_WIRED_AGAIN_TIMEOUT_SECONDS
    )
    nodes = graph()
    source, reader = node_named(nodes, "source"), node_named(nodes, "reader")
    local_api.call_tool(
        "connect",
        {
            "stream": stream_id,
            "source": {"node": source["id"], "port": "ticks"},
            "target": {"node": reader["id"], "port": "ticks_from_upstream"},
            "mode": "live",
        },
    )
    (project_directory / "nodes" / "link_connected").write_text("")

    tatolabd.await_marker(
        "READER_SETTING_UP", occurrence=3, timeout=REFUSED_LINK_WIRED_AGAIN_TIMEOUT_SECONDS
    )
    states_read: "list[str]" = []
    deadline = time.monotonic() + REFUSED_LINK_WIRED_AGAIN_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        (link,) = graph()["links"]
        if not states_read or states_read[-1] != link["state"]:
            states_read.append(link["state"])
        if link["state"] == "wired":
            break
        time.sleep(0.1)

    assert states_read[-1] == "wired", (
        f"the link read {states_read}, last as {link}\n{tatolabd.recent_stderr()}"
    )
    after = graph()
    assert node_named(after, "reader")["components"]["state"] == "Running", after
    assert node_named(after, "source")["components"]["state"] == "Running", after
    restarts = node_named(after, "reader")["components"]["helper_process_restarts"]
    # A restart counts once its helper is set up, so the second start, dying in its setup,
    # is none.
    assert restarts["restart_count"] == 1, restarts
    assert "before it finished setting up" in restarts["last_exit_reason"], restarts
