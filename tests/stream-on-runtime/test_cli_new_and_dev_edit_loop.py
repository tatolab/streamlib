# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The MVP minute, with every node in its own node interpreter.

What `new` writes runs frame after frame, a graph of helpers goes live inside
the startup budget their interpreters cost, and the edit loop — `dev` loading
the stream again on each save — reports a bad save and loads the next good one,
on the same runtime. The starter opens no window, so live video is the count of
frames its source wrote while a viewer held its port open, read off `graph`,
and the tests that measure it run on the software device. Starting a stream
initializes a GPU context, so the module needs a device.
"""

from __future__ import annotations

import asyncio
import json
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
from mcp.client.client import Client
from mcp.client.stdio import StdioServerParameters
from mcp.types import TextContent

from cli_launch_projects import (
    CLEAN_EXIT_TIMEOUT_SECONDS,
    FIRST_FRAME_REPORTER_MODULE,
    LIVE_HELPER_MARKER_NAME,
    NODE_READY_TIMEOUT_SECONDS,
    STREAM_WITH_HELPER_PLACED_NODES_TEMPLATE,
    load_the_projects_stream,
    project_files_with_helper_placed_nodes,
    stream_id_of_the_project,
    the_stream_stopped_log_line,
)
from conftest import (
    PrivateMachineDirectories,
    TatolabdUnderTest,
    TatolabRunOrDevOnTheRuntime,
)
from local_api_client import call_operation_tool_call
from runtime_process_under_test import ENGINE_GRACEFUL_STOP_LOG_LINE, ENGINE_STARTED_LOG_LINE
from runtime_unit_under_test import RuntimeUnitUnderTest
from test_node_interpreter_lend import assert_runs_in_a_process_of_its_own_beneath


pytestmark = pytest.mark.requires_gpu


# Long enough that a per-frame failure or slowdown cannot hide inside it — and
# long enough to outlast a warm-up. An observation that ends before steady
# state proves less than its length suggests: a per-frame defect can need ~280
# delivered frames, nine seconds of them, before it appears at all.
SCAFFOLD_OBSERVATION_WINDOW_SECONDS = 12.0
# The save has to land on a stream that is already running, not one still booting.
SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS = SCAFFOLD_OBSERVATION_WINDOW_SECONDS / 2
# The source writes at 30fps, 360 frames in 12.0s. The floor keeps a third of
# that, well above a slideshow, so it fails on a regression and not on a slow
# machine.
MINIMUM_FRAMES_FOR_LIVE_VIDEO = 120


# A fleet rather than a pair, and few enough that the rig pays for it in
# seconds.
HELPER_PLACED_NODE_COUNT = 6
# The MVP sentence gives a minute for install, scaffold and run, and loading is
# the only part of that this test can measure — so the budget is the half of
# the minute the other parts do not need.
#
# It is a ceiling and nothing subtler. What this catches is a helper that
# stopped starting, or a per-child cost that grew by an order of magnitude —
# not the difference between a parallel spawn and a serial one, which lives
# inside the noise at this count. The distinct-pid assertions are what pin
# placement.
MAXIMUM_SECONDS_FOR_EVERY_HELPER_TO_GO_LIVE = 30.0


STARTER_SOURCE_NODE_NAME = "testpatternsource"
STARTER_SOURCE_OUTPUT_PORT = "video"

# The edit loop's project: one test pattern source read by the reporter, the
# node module each save edits.
STREAM_WITH_THE_REPORTER_READING_ONE_TEST_PATTERN_SOURCE = (
    STREAM_WITH_HELPER_PLACED_NODES_TEMPLATE % 1
)
EDITED_NODE_MODULE_PATH = FIRST_FRAME_REPORTER_MODULE.name


#: What `dev` says as a save makes it queue another load of the project.
DEV_QUEUING_ANOTHER_LOAD_AFTER_A_SAVE = "tatolab dev: a saved change — queuing another load of the project"

#: How often a wait on what `dev` printed of the project's log looks again.
DEV_OUTPUT_POLL_INTERVAL_SECONDS = 0.1


def await_dev_printed(dev: TatolabRunOrDevOnTheRuntime, text: str, *, timeout: float) -> None:
    """Wait until `dev`'s standard output — the project's log it follows — holds `text`."""
    deadline = time.monotonic() + timeout
    while text not in dev.stdout_text:
        assert time.monotonic() < deadline, (
            f"`dev` printed no record holding {text!r} within {timeout}s; it printed:\n"
            f"{dev.stdout_text[-4000:]}"
        )
        time.sleep(DEV_OUTPUT_POLL_INTERVAL_SECONDS)


def the_first_streams_output(tatolabd: TatolabdUnderTest, stream_id: str) -> str:
    """`tatolabd`'s standard error through the first stop of the stream `stream_id`."""
    the_first_streams_stop = tatolabd.await_stderr_containing(
        the_stream_stopped_log_line(stream_id), timeout=CLEAN_EXIT_TIMEOUT_SECONDS
    )
    return tatolabd.stderr_text.split(the_first_streams_stop, 1)[0] + the_first_streams_stop


def frames_written_on_a_port_while_a_viewer_holds_it(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    stream_id: str,
    node_name: str,
    port: str,
    seconds_held: float,
) -> int:
    """The bags `node_name` wrote on `port` while a viewer held the port open for
    `seconds_held`, as Tato Studio's viewer does, read off `graph` before the viewer
    lets go.

    A port is written only once something reads it, so a stream that links nothing
    publishes nothing until a viewer opens it."""
    tatolab_mcp = StdioServerParameters(
        command=str(runtime_unit.tatolab_executable),
        args=["mcp"],
        env=private_machine_directories.environment,
    )

    def tool_answer(result: Any) -> Any:
        assert result.is_error is False, result.content
        stated = result.content[0]
        assert isinstance(stated, TextContent), result.content
        return json.loads(stated.text)

    async def held_viewer() -> int:
        async with Client(tatolab_mcp) as client:
            graph = tool_answer(await client.call_tool("graph", {"stream": stream_id}))
            [viewed_node] = [node for node in graph["nodes"] if node["name"] == node_name]
            tool_answer(
                await client.call_tool(
                    *call_operation_tool_call(
                        "open_viewer",
                        {"stream": stream_id, "node_id": viewed_node["id"], "port": port},
                    )
                )
            )
            await asyncio.sleep(seconds_held)
            graph = tool_answer(await client.call_tool("graph", {"stream": stream_id}))
            [viewed_node] = [node for node in graph["nodes"] if node["name"] == node_name]
            return viewed_node["components"]["metrics"]["written_bags_by_output_port"][port]

    return asyncio.run(asyncio.wait_for(held_viewer(), seconds_held + NODE_READY_TIMEOUT_SECONDS))


def test_the_scaffolded_app_reaches_a_running_graph(
    runtime_unit: RuntimeUnitUnderTest,
    private_machine_directories: PrivateMachineDirectories,
    make_scaffolded_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """What `tatolab new` writes must actually run, frame after frame.

    Run exactly as scaffolded: `new` has the runtime write the starter, one test
    pattern source, and open it held stopped, and `start_playback` plays it. A
    listed stream alone proves almost nothing here: it is listed whether or not
    the source ever writes, so the assertion that carries this test is the count
    of frames the source wrote on `video` while a viewer read it, which catches a
    source that is correct but so slow the first minute is a slideshow.
    """
    tatolabd = start_tatolabd()
    app_directory = make_scaffolded_tatolab_project(tatolabd)
    local_api = tatolabd.local_api_client()
    queued_playback = local_api.call_tool(
        "start_playback", {"project_directory": str(app_directory)}
    )
    assert local_api.stream_action_once_ended(queued_playback["action"])["state"] == "done"

    stream_id = stream_id_of_the_project(app_directory)
    live_graph = local_api.await_every_node_running(stream=stream_id)
    assert live_graph["stream"] == stream_id, (
        f"the runtime must render the stream it was loaded as; graph was {live_graph}"
    )
    assert [graph_node["name"] for graph_node in live_graph["nodes"]] == [
        STARTER_SOURCE_NODE_NAME
    ]

    frames_written = frames_written_on_a_port_while_a_viewer_holds_it(
        runtime_unit,
        private_machine_directories,
        stream_id,
        STARTER_SOURCE_NODE_NAME,
        STARTER_SOURCE_OUTPUT_PORT,
        SCAFFOLD_OBSERVATION_WINDOW_SECONDS,
    )
    queued_stop = local_api.call_tool("stop_playback", {"project_directory": str(app_directory)})
    assert local_api.stream_action_once_ended(queued_stop["action"])["state"] == "done"

    stream_output = the_first_streams_output(tatolabd, stream_id)
    assert_the_stream_carried_live_video(
        tatolabd, stream_output, frames_written, "the app `tatolab new` writes"
    )


def assert_the_stream_carried_live_video(
    tatolabd: TatolabdUnderTest, output: str, frames_written: int, what_ran: str
) -> None:
    """Require one stream to have carried live video, not a slideshow: the
    frames it wrote while it ran, and its records once it stopped."""
    process_failures = [line for line in output.splitlines() if "process() failed" in line]
    assert not process_failures, (
        f"{what_ran}: a node failed a live frame; standard error ended:\n"
        f"{tatolabd.recent_stderr()}"
    )
    assert frames_written >= MINIMUM_FRAMES_FOR_LIVE_VIDEO, (
        f"{what_ran} wrote only {frames_written} frames in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — that is a slideshow, not live video"
    )
    undeliverable_notifications = output.count("Unable to send notification")
    assert undeliverable_notifications == 0, (
        f"{what_ran}: {undeliverable_notifications} undeliverable link notifications in "
        f"{SCAFFOLD_OBSERVATION_WINDOW_SECONDS}s — a notify failed to reach its listener; "
        f"standard error ended:\n{tatolabd.recent_stderr()}"
    )


def test_every_helper_interpreter_goes_live_inside_the_startup_budget(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The N-node-interpreter startup budget the MVP minute has to pay.

    Every Python node is its own node interpreter, so a graph's load
    cost grows with its node count, and the sentence gives that growth a
    minute to disappear into. The budget is a flat ceiling on the whole fleet —
    see the constant for what a ceiling this generous can and cannot catch.

    What it measures is live traffic, not liveness: a helper reports only once
    a frame has reached it, so an interpreter that started and received nothing
    does not count. The distinct-pid assertions are the placement half — N
    nodes must be N processes beneath `tatolabd`, so a spawn path that
    quietly reused one fails here rather than passing on the timing alone.
    """
    fleet_app = make_tatolab_project(
        project_files_with_helper_placed_nodes(HELPER_PLACED_NODE_COUNT),
        directory_name="fleet",
    )
    tatolabd = start_tatolabd()
    launched_at = time.monotonic()
    fleet = load_the_projects_stream(tatolabd, fleet_app, "dev")
    # Waited for on a bound far above the budget rather than on the budget
    # itself: a wait that expires exactly at the ceiling can only ever report a
    # timeout, and what a blown budget should say is how long it actually took.
    tatolabd.await_marker(
        LIVE_HELPER_MARKER_NAME,
        occurrence=HELPER_PLACED_NODE_COUNT,
        timeout=NODE_READY_TIMEOUT_SECONDS,
    )
    seconds_for_every_helper = time.monotonic() - launched_at

    reporting_pids = set(tatolabd.marker_payloads(LIVE_HELPER_MARKER_NAME))
    assert len(reporting_pids) == HELPER_PLACED_NODE_COUNT, (
        f"{HELPER_PLACED_NODE_COUNT} nodes reported from "
        f"{len(reporting_pids)} processes — every Python node gets its own"
    )
    assert fleet.pid not in reporting_pids, "a node reported from tatolab's own process"
    for reporting_pid in reporting_pids:
        assert_runs_in_a_process_of_its_own_beneath(reporting_pid, tatolabd.pid)
    assert seconds_for_every_helper < MAXIMUM_SECONDS_FOR_EVERY_HELPER_TO_GO_LIVE, (
        f"{HELPER_PLACED_NODE_COUNT} node interpreters took "
        f"{seconds_for_every_helper:.2f}s to reach live traffic — the minute does "
        f"not absorb that"
    )
    fleet.interrupt()
    assert fleet.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, fleet.recent_stderr()
    assert fleet.stop_the_stream(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, (
        f"a graph of {HELPER_PLACED_NODE_COUNT} helpers must still unload cleanly"
    )
    tatolabd.await_stderr_containing(
        the_stream_stopped_log_line(stream_id_of_the_project(fleet_app)),
        timeout=CLEAN_EXIT_TIMEOUT_SECONDS,
    )


def the_reporter_edited(reporter_source: str) -> str:
    """The reporter with one more marker on its first frame, applied to its own
    source so the edit fails here, not downstream, if the reporter changes."""
    anchor = '        log.info(f"MARKER:LIVE {os.getpid()}")\n'
    assert reporter_source.count(anchor) == 1, (
        f"the reporter carries {reporter_source.count(anchor)} copies of the anchor "
        f"{anchor!r} this edit needs exactly one of"
    )
    return reporter_source.replace(
        anchor, anchor + '        log.info("MARKER:EDITED_NODE")\n', 1
    )


def node_ids_of(graph: "dict[str, Any]") -> "list[str]":
    """The ids of a live graph's nodes: the UUIDs their graph document gives them."""
    return sorted(node["id"] for node in graph["nodes"])


def test_the_edit_loop_survives_a_bad_save_and_runs_a_good_one(
    make_tatolab_project: "Callable[..., Path]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    """The MVP edit loop: `dev` queuing another load of the project on every save.

    A save is a file write, and `dev` queues a load over its connection. A
    broken save is never refused: the runtime unloads the running stream and
    records it failed with the traceback, which the project's log carries and
    `dev` prints, and the next good save loads the edited code. Both halves run
    against the same `dev` and the same `tatolabd` in sequence because the first
    is only meaningful if the second follows: a loop that survives a bad save by
    ignoring the file entirely would pass the first alone.
    """
    app_directory = make_tatolab_project(
        {
            "stream.py": STREAM_WITH_THE_REPORTER_READING_ONE_TEST_PATTERN_SOURCE,
            EDITED_NODE_MODULE_PATH: FIRST_FRAME_REPORTER_MODULE.read_text(encoding="utf-8"),
        }
    )
    tatolabd = start_tatolabd()
    edited_node_module = app_directory / EDITED_NODE_MODULE_PATH
    last_good_node_source = edited_node_module.read_text()
    stream_id = stream_id_of_the_project(app_directory)
    local_api = tatolabd.local_api_client()

    dev = load_the_projects_stream(tatolabd, app_directory, "dev")
    dev.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolabd.await_stderr_containing(ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS)
    node_ids_before_the_bad_save = node_ids_of(
        local_api.await_every_node_running(stream=stream_id)
    )

    time.sleep(SECONDS_OF_LIVE_VIDEO_BEFORE_THE_BAD_SAVE_LANDS)
    edited_node_module.write_text("def process(self ctx:\n    this does not parse\n")
    dev.await_stderr_containing(
        DEV_QUEUING_ANOTHER_LOAD_AFTER_A_SAVE, timeout=NODE_READY_TIMEOUT_SECONDS
    )
    bad_saves_failed_stream = dev.failed_stream(timeout=NODE_READY_TIMEOUT_SECONDS, occurrence=2)
    assert bad_saves_failed_stream is not None, dev.recent_stderr()
    assert bad_saves_failed_stream[0] == stream_id, bad_saves_failed_stream
    await_dev_printed(dev, f"the stream `{stream_id}` failed: ", timeout=NODE_READY_TIMEOUT_SECONDS)
    await_dev_printed(dev, "SyntaxError", timeout=NODE_READY_TIMEOUT_SECONDS)
    assert EDITED_NODE_MODULE_PATH in dev.stdout_text, dev.stdout_text[-4000:]
    assert dev.process.poll() is None, (
        f"a bad save must not take `dev` down; standard error ended:\n{dev.recent_stderr()}"
    )
    tatolabd.await_stderr_containing(
        the_stream_stopped_log_line(stream_id), timeout=NODE_READY_TIMEOUT_SECONDS
    )
    [listed] = local_api.list_streams()
    assert (listed["stream"], listed["state"]) == (stream_id, "failed"), listed
    assert "SyntaxError" in listed["failed_because"], listed

    edited_node_module.write_text(the_reporter_edited(last_good_node_source))
    dev.await_loaded(timeout=NODE_READY_TIMEOUT_SECONDS, occurrence=3)
    tatolabd.await_marker("EDITED_NODE", timeout=NODE_READY_TIMEOUT_SECONDS)
    tatolabd.await_stderr_containing(
        ENGINE_STARTED_LOG_LINE, timeout=NODE_READY_TIMEOUT_SECONDS, occurrence=2
    )
    stderr_lines = list(tatolabd.stderr_lines)
    engine_started_line_indices = [
        line_index
        for line_index, stderr_line in enumerate(stderr_lines)
        if ENGINE_STARTED_LOG_LINE in stderr_line
    ]
    assert any(
        ENGINE_GRACEFUL_STOP_LOG_LINE in stderr_line
        for stderr_line in stderr_lines[: engine_started_line_indices[1]]
    ), "the stream the bad save failed did not stop gracefully before the edited one started"
    node_ids_after_the_good_save = node_ids_of(
        local_api.await_every_node_running(stream=stream_id)
    )
    assert node_ids_after_the_good_save == node_ids_before_the_bad_save, (
        "a node keeps the UUID its graph document gives it on every load"
    )
    assert [listed["state"] for listed in local_api.list_streams()] == ["running"]
    assert tatolabd.process.poll() is None, "every load of the edit loop lands on the one runtime"

    dev.interrupt()
    assert dev.await_exit(timeout=CLEAN_EXIT_TIMEOUT_SECONDS) == 0, dev.recent_stderr()
