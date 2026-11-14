# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""A manual Python processor whose `start()` raises, read from `graph`.

Its node reads `Error` with what `start()` raised under `failed_because`, the
rest of the stream keeps running, and a node that started cleanly renders no
reason. Asserted from outside, through the `graph` a control-plane client reads.
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

#: A cold spawn and import for each helper, with room for a loaded rig.
FAILED_START_TIMEOUT_SECONDS = 30.0

START_RAISING_PROCESSORS_SOURCE = '''\
"""A manual processor whose `start()` raises, beside one whose `start()` returns."""

from tatolab.stream import Output, Records, RuntimeContextFullAccess, node


@node.process(execution="manual")
class StartRaises:
    """Cannot read the file its `start()` opens."""

    video = Output(Records)

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        raise FileNotFoundError('could not read "lower_third.riv"')


@node.process(execution="manual")
class StartReturns:
    """Starts and does nothing."""

    video = Output(Records)

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        pass
'''

START_RAISING_STREAM_SOURCE = '''\
from tatolab.stream import StreamBuilder, graph_generator

from processors.start_raising import StartRaises, StartReturns


@graph_generator
def main(stream_builder: StreamBuilder) -> None:
    stream_builder.add(StartRaises, name="start-raises")
    stream_builder.add(StartReturns, name="start-returns")
'''


def node_named(graph: "dict[str, Any]", name: str) -> "dict[str, Any]":
    """The one node carrying `name`."""
    matches = [node for node in graph["nodes"] if node["name"] == name]
    assert len(matches) == 1, [node["name"] for node in graph["nodes"]]
    return matches[0]


def test_a_node_whose_start_raised_reads_error_with_what_it_raised(
    make_tatolab_project: "Callable[..., Any]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The node whose `start()` raised reads `Error` with the exception's words
    under `failed_because`; the node beside it reads `Running` with none.

    Fail-without-fix: log what `start()` raised and carry on, and the node reads
    `Running` producing nothing, which is what a user watching the graph saw.
    """
    project_directory = make_tatolab_project(
        {
            "processors/__init__.py": "",
            "processors/start_raising.py": START_RAISING_PROCESSORS_SOURCE,
            "stream.py": START_RAISING_STREAM_SOURCE,
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

    deadline = time.monotonic() + FAILED_START_TIMEOUT_SECONDS
    start_raises = node_named(graph(), "start-raises")
    while start_raises["components"]["state"] != "Error" and time.monotonic() < deadline:
        time.sleep(0.2)
        start_raises = node_named(graph(), "start-raises")

    assert start_raises["components"]["state"] == "Error", start_raises
    failed_because = start_raises["components"]["failed_because"]
    assert failed_because.startswith("start() raised: "), failed_because
    assert 'could not read "lower_third.riv"' in failed_because, failed_because

    start_returns = node_named(graph(), "start-returns")
    assert start_returns["components"]["state"] == "Running", start_returns
    assert start_returns["components"]["failed_because"] is None, start_returns
