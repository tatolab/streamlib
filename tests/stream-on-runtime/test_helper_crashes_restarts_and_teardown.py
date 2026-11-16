# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""How a Python node's helper process is moved, restarted and torn down: a
pooled call that overruns, hangs or crashes, a crashed helper, teardown on an
interrupt, and a helper that imported another engine build.
"""

from __future__ import annotations

import time
from collections.abc import Callable
from pathlib import Path

import pytest

from conftest import TatolabdUnderTest
from helper_placement_observation import (
    StartTatolabdRunningStream,
)
from helper_placement_nodes import (
    CALLS_RETURNED_BEFORE_A_CALL_HANGS,
    COUNT_AT_WHICH_A_FRESH_COUNT_SINK_DIES,
)
from helper_placement_streams import (
    dies_abruptly_beside_a_survivor_pair,
    function_node_run_as_python_that_crashes_into_python_sink,
    function_node_slower_than_the_pools_frame_interval_read_by_a_native_converter,
    function_node_that_spins_forever_into_python_sink,
    one_probe_forking_a_worker_that_outlives_it,
    one_probe_sleeping_through_its_own_setup,
    one_probe_sleeping_through_its_own_shutdown,
    pooled_counter_beside_a_pooled_node_that_hangs,
    reaped_labelled_source_into_sink,
    stateful_counter_into_python_sink_that_dies_once,
    stale_build_labelled_source,
)
from helper_process_observation import (
    a_process_is_gone_within,
    helper_process_ids_started_in,
    helper_process_is_still_alive,
)
from local_api_client import RUNNING_NODE_STATE
from runtime_unit_under_test import STREAM_ON_RUNTIME_SUITE_DIRECTORY
from tatolab.stream import StreamBuilder, run_graph_generator


pytestmark = pytest.mark.requires_gpu


# The ladder's own worst case is a second of interrupt plus five of teardown
# plus the group's grace; a helper that answers at once is far inside it. This
# is what separates "the ladder ran" from "the thirty-second callback did".
LADDER_BUDGET_SECONDS = 15.0

HELPER_DIED_SETTING_UP_LOG_LINE_FRAGMENT = "its helper process died before it finished setting up"

#: A build id no build of this checkout mints: its nonce is all zeros.
ENGINE_BUILD_ID_OF_ANOTHER_BUILD = (
    "0.0.1+0123456789abcdef0123456789abcdef01234567.00000000000000000000000000000000"
)


def run_until_marker_then_interrupt(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
    stream_function: Callable[[StreamBuilder], None],
    marker_name: str,
) -> TatolabdUnderTest:
    """Run a stream until `marker_name` shows up, then Ctrl-C and require a clean exit.

    Sequencing on the marker rather than a timer is what keeps the wait as
    short as the event and as long as the machine needs — and the interrupt
    exercises the same teardown a terminal Ctrl-C does.
    """
    tatolabd = start_tatolabd_running_stream(stream_function)
    tatolabd.await_marker(marker_name)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    return tatolabd


def test_a_pooled_function_node_that_overruns_the_pools_frame_interval_three_times_moves_out_of_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A node slower than the pool's frame interval holds every island back, so it leaves the pool.

    Each call of `sleeper` sleeps past the pool's 10 ms interval. On its third
    such call running, the pool names it on its board; the engine ends the pool
    and moves `sleeper` into a helper process of its own, which `graph` shows,
    and the converter goes on reading its value from there.

    Fail-without-fix: no call outlasts the stall watchdog's floor, so the pool
    runs on at the sleeper's pace and `graph` shows no promotion.
    """
    tatolabd = start_tatolabd_running_stream(
        function_node_slower_than_the_pools_frame_interval_read_by_a_native_converter
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    overrun_line = tatolabd.await_stderr_containing("frame interval 3 times running")
    tatolabd.await_stderr_containing("promoted out of its islands into a helper process of its own")
    records_read = tatolabd.await_marker("LEVEL_RECORDS_READ")
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert "function node `sleeper` overran its island's 10 ms frame interval" in overrun_line, (
        overrun_line
    )
    sleeper = next(node for node in graph["nodes"] if node["name"] == "sleeper")
    promotion = sleeper["components"]["metrics"]["promoted_out_of_its_readers_islands"]
    assert promotion["promotions"] == 1, promotion
    assert "overran its island's 10 ms frame interval" in promotion["reason"], promotion
    levels = [record["level"] for record in records_read["records"]]
    assert all(level == pytest.approx(0.5) for level in levels), levels


def test_a_hung_call_in_the_pool_respawns_it_and_its_stateful_nodes_keep_their_state(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A call that never returns ends the pool; its next process resumes every node's state.

    `hanger` returns from its first calls and then never does, so the pool's
    helper is ended as stalled and `hanger` moves into a helper process of its
    own. The pool's next process resumes `counter` from the count its board
    last held, so the counts the sink reads never start over.

    Fail-without-fix: with the state kept only in the pool's process, the
    count starts again from one after the respawn.
    """
    tatolabd = start_tatolabd_running_stream(pooled_counter_beside_a_pooled_node_that_hangs)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    tatolabd.await_marker("COUNT_RECORDS_READ")
    stall_line = tatolabd.await_stderr_containing("stalled: its call had run for")
    tatolabd.await_stderr_containing("promoted out of its islands into a helper process of its own")
    reports_by_the_promotion = len(tatolabd.marker_payloads("COUNT_RECORDS_READ"))
    tatolabd.await_marker("COUNT_RECORDS_READ", occurrence=reports_by_the_promotion + 2)
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert "function node `hanger` stalled" in stall_line, stall_line
    counts = [
        count
        for report in tatolabd.marker_payloads("COUNT_RECORDS_READ")
        for count in report["counts"]
    ]
    assert counts[-1] > CALLS_RETURNED_BEFORE_A_CALL_HANGS, (
        f"the counter ran past the hanger's first calls and on after the respawn: {counts}"
    )
    assert all(later >= earlier for earlier, later in zip(counts, counts[1:])), (
        f"the pool's next process resumes the count, and the sink read {counts}"
    )
    hanger = next(node for node in graph["nodes"] if node["name"] == "hanger")
    promotion = hanger["components"]["metrics"]["promoted_out_of_its_readers_islands"]
    assert promotion["promotions"] == 1, promotion
    assert "function node `hanger` stalled" in promotion["reason"], promotion


def test_a_function_node_call_that_never_returns_moves_the_node_out_of_its_reader(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A stalled function-node call is handled as a crash, and the node leaves its reader.

    No thread can pre-empt a compiled call, so the reader's helper is held
    inside it. The engine reads the call's breadcrumb off the helper's board,
    ends the helper once the call has run past its watchdog budget, and names
    the node that stalled. Before the reader's next helper starts, the node
    moves into a helper process of its own, which `graph` shows under its
    `metrics`: the reader's new helper evaluates it no more, so the next stall
    is the node's own helper's, and the reader keeps running through it.

    Fail-without-fix: with no move, the reader's every new helper calls the
    node again and stalls, and `graph` shows no promotion.
    """
    tatolabd = start_tatolabd_running_stream(function_node_that_spins_forever_into_python_sink)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    first_helper_pid = tatolabd.await_marker("FUNCTION_NODE_SINK_STARTED")["pid"]
    stall_line = tatolabd.await_stderr_containing("stalled: its call had run for")
    tatolabd.await_stderr_containing("promoted out of its islands into a helper process of its own")
    restarted_helper_pid = tatolabd.await_marker("FUNCTION_NODE_SINK_STARTED", occurrence=2)["pid"]
    stall_line_of_the_nodes_own_helper = tatolabd.await_stderr_containing(
        "[spinner] its helper process (pid="
    )
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    sink_starts = len(tatolabd.marker_payloads("FUNCTION_NODE_SINK_STARTED"))
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert restarted_helper_pid != first_helper_pid, (
        f"the stalled helper is replaced by a new process:\n{tatolabd.recent_stderr()}"
    )
    assert "function node `spinner` stalled" in stall_line, (
        f"the stall must name the node whose call never returned: {stall_line}"
    )
    assert "stalled in a function node" in stall_line_of_the_nodes_own_helper, (
        f"once moved, the node stalls in its own helper: {stall_line_of_the_nodes_own_helper}"
    )
    assert sink_starts == 2, (
        f"the reader's helper started {sink_starts} times; once the node moved out, its stall "
        f"no longer ends the reader's helper:\n{tatolabd.recent_stderr()}"
    )
    assert len(set(helper_process_ids_started_in(tatolabd.stderr_text))) >= 3, (
        f"the reader's two helpers and the node's own:\n{tatolabd.recent_stderr()}"
    )
    spinner = next(node for node in graph["nodes"] if node["name"] == "spinner")
    promotion = spinner["components"]["metrics"]["promoted_out_of_its_readers_islands"]
    assert promotion["promotions"] == 1, promotion
    assert promotion["after_crashing_the_helper_of"] == "sink", promotion
    assert "function node `spinner` stalled" in promotion["reason"], promotion
    assert not tatolabd.marker_payloads("FUNCTION_NODE_VALUES_READ"), (
        f"no value reaches the reader while the node spins:\n{tatolabd.recent_stderr()}"
    )


def test_a_function_node_run_as_python_that_crashes_its_reader_moves_out_of_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A call run as Python that crashes its reader's helper moves the node out of the reader.

    The call's breadcrumb is noted on the helper's board as a compiled call's
    is, so the engine names the node the helper died in. Before the reader's
    next helper starts, the node moves into a helper process of its own, which
    `graph` shows under its `metrics`; once there, its crash is its own
    helper's, and the reader keeps running through it.

    Fail-without-fix: a call run as Python with no breadcrumb leaves the death
    unattributed, so the reader's every new helper calls the node and dies.
    """
    tatolabd = start_tatolabd_running_stream(function_node_run_as_python_that_crashes_into_python_sink)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    first_helper_pid = tatolabd.await_marker("FUNCTION_NODE_SINK_STARTED")["pid"]
    tatolabd.await_stderr_containing("promoted out of its islands into a helper process of its own")
    restarted_helper_pid = tatolabd.await_marker("FUNCTION_NODE_SINK_STARTED", occurrence=2)["pid"]
    tatolabd.await_stderr_containing("[crasher] its helper process (pid=")
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    sink_starts = len(tatolabd.marker_payloads("FUNCTION_NODE_SINK_STARTED"))
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert restarted_helper_pid != first_helper_pid, (
        f"the crashed helper is replaced by a new process:\n{tatolabd.recent_stderr()}"
    )
    assert sink_starts == 2, (
        f"the reader's helper started {sink_starts} times; once the node moved out, its crash "
        f"no longer ends the reader's helper:\n{tatolabd.recent_stderr()}"
    )
    crasher = next(node for node in graph["nodes"] if node["name"] == "crasher")
    promotion = crasher["components"]["metrics"]["promoted_out_of_its_readers_islands"]
    assert promotion["promotions"] == 1, promotion
    assert promotion["after_crashing_the_helper_of"] == "sink", promotion
    assert "function node `crasher` crashed its helper process during its call" in promotion["reason"], (
        promotion
    )
    assert crasher["components"]["ran_compiled"] is False, crasher


def test_a_stateful_function_node_carries_its_state_through_a_helper_crash_and_a_restart(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
    start_tatolabd: Callable[..., TatolabdUnderTest],
):
    """A stateful function node's state belongs to its stream, not to the
    process that evaluates it.

    The counter starts at one in its reader's first helper, which takes itself
    down at twenty; the reader's next helper resumes the count where the dead
    one left it, `graph` shows the count the node holds, and a restart of the
    runtime resumes it again.

    Fail-without-fix: with the state kept only in the helper, each new helper
    counts from one again, and its reader dies at twenty every time.
    """
    tatolabd = start_tatolabd_running_stream(stateful_counter_into_python_sink_that_dies_once)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    first_read_of_the_first_helper = tatolabd.await_marker("FIRST_COUNT_READ")
    count_its_helper_died_at = tatolabd.await_marker("FRESH_COUNT_SINK_ABOUT_TO_DIE")["count"]
    first_read_of_the_restarted_helper = tatolabd.await_marker("FIRST_COUNT_READ", occurrence=2)
    local_api = tatolabd.local_api_client()
    graph = local_api.call_tool("graph", {"stream": stream_id})
    counter = next(node for node in graph["nodes"] if node["name"] == "counter")
    count_the_graph_showed = counter["components"]["function_node_state"]["calls"]
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    restarted_tatolabd = start_tatolabd()
    first_read_after_the_restart = restarted_tatolabd.await_marker("FIRST_COUNT_READ")
    restarted_tatolabd.interrupt()
    restarted_tatolabd.await_clean_exit()

    assert first_read_of_the_first_helper["count"] == 1.0, first_read_of_the_first_helper
    assert count_its_helper_died_at >= COUNT_AT_WHICH_A_FRESH_COUNT_SINK_DIES
    assert first_read_of_the_restarted_helper["pid"] != first_read_of_the_first_helper["pid"]
    assert first_read_of_the_restarted_helper["count"] > count_its_helper_died_at, (
        f"the reader's new helper resumes the count its dead one reached, "
        f"{count_its_helper_died_at}, and first read {first_read_of_the_restarted_helper}"
    )
    assert count_the_graph_showed >= first_read_of_the_restarted_helper["count"], (
        f"`graph` shows the count the node holds: {counter['components']}"
    )
    assert first_read_after_the_restart["count"] > count_the_graph_showed, (
        f"a runtime restart resumes the count from {count_the_graph_showed}, and the reader "
        f"first read {first_read_after_the_restart}"
    )
    assert len(tatolabd.marker_payloads("FRESH_COUNT_SINK_ABOUT_TO_DIE")) == 1, (
        f"only the fresh count reaches twenty:\n{tatolabd.recent_stderr()}"
    )


def test_no_helper_survives_the_app(start_tatolabd_running_stream: StartTatolabdRunningStream):
    tatolabd = run_until_marker_then_interrupt(
        start_tatolabd_running_stream, reaped_labelled_source_into_sink, "SINK_PID"
    )
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)
    assert len(helper_pids) == 2, (
        f"expected the source and the sink to each report a helper pid, got {helper_pids}:\n"
        f"{tatolabd.recent_stderr()}"
    )

    survivors = [pid for pid in helper_pids if helper_process_is_still_alive(pid)]
    assert not survivors, f"helper processes {survivors} outlived the tatolabd that spawned them"


def test_a_node_asleep_in_its_callback_still_runs_its_teardown(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """The shutdown ladder, end to end.

    Fail-without-fix: with the old pair of five-second reply deadlines the
    sleeping callback misses `stopped`, the helper is marked gone, and its
    `teardown()` is skipped outright — so `SLEEPER_TORE_DOWN` never arrives.
    """
    tatolabd = start_tatolabd_running_stream(one_probe_sleeping_through_its_own_shutdown)
    tatolabd.await_marker("ASLEEP_IN_PROCESS")
    interrupted_at = time.monotonic()
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    ended_in = time.monotonic() - interrupted_at

    assert tatolabd.marker_payloads("SLEEPER_STOPPED"), (
        f"`stop()` did not run after the interrupt:\n{tatolabd.recent_stderr()}"
    )
    assert tatolabd.marker_payloads("SLEEPER_TORE_DOWN"), (
        f"`teardown()` did not run after the interrupt:\n{tatolabd.recent_stderr()}"
    )
    assert "MARKER:SLEPT_THE_WHOLE_WAY" not in tatolabd.stderr_text, (
        f"the callback returned on its own, so nothing interrupted it:\n{tatolabd.recent_stderr()}"
    )
    assert ended_in < LADDER_BUDGET_SECONDS, (
        f"tatolabd took {ended_in:.1f}s to end, which is the callback's thirty seconds rather "
        f"than the ladder's budget:\n{tatolabd.recent_stderr()}"
    )


def test_a_node_interrupted_while_still_setting_up_still_tears_down(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """The route onto the ladder the engine's `stop()` hook never reaches.

    A helper still inside `setup()` has never had `stop()` called on it, so the
    commands whose replies the ladder waits for are only on the wire because
    the ladder asks for them itself.

    Fail-without-fix: leave the ask in `stop()` alone and this helper is
    SIGINT'd, answers its refusal, and is then killed with its group — its
    `teardown()` never asked for and never run.
    """
    tatolabd = start_tatolabd_running_stream(one_probe_sleeping_through_its_own_setup)
    tatolabd.await_marker("ASLEEP_IN_SETUP")
    interrupted_at = time.monotonic()
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    ended_in = time.monotonic() - interrupted_at

    assert tatolabd.marker_payloads("INTERRUPTED_SETUP_TORE_DOWN"), (
        f"an interrupted `setup()` was never given its `teardown()`:\n{tatolabd.recent_stderr()}"
    )
    assert "MARKER:SLEPT_THE_WHOLE_SETUP" not in tatolabd.stderr_text, (
        f"`setup()` returned on its own, so nothing interrupted it:\n{tatolabd.recent_stderr()}"
    )
    assert ended_in < LADDER_BUDGET_SECONDS, (
        f"tatolabd took {ended_in:.1f}s to end, which is the registration budget rather than "
        f"the ladder's:\n{tatolabd.recent_stderr()}"
    )


def test_a_worker_a_node_forked_goes_down_with_the_apps_helper(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    tatolabd = run_until_marker_then_interrupt(
        start_tatolabd_running_stream, one_probe_forking_a_worker_that_outlives_it, "WORKER_PID"
    )
    worker_pid = tatolabd.marker_payloads("WORKER_PID")[0]["worker_pid"]

    assert a_process_is_gone_within(worker_pid, 2.0), (
        f"the worker a node forked outlived the tatolabd that spawned it:\n"
        f"{tatolabd.recent_stderr()}"
    )


def test_a_crashed_helper_is_restarted_in_a_new_process_and_the_pipeline_keeps_running(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    tatolabd = start_tatolabd_running_stream(dies_abruptly_beside_a_survivor_pair)
    first_helper_pid = tatolabd.await_marker("ABOUT_TO_DIE")["pid"]
    death_line = tatolabd.await_stderr_containing(
        f"its helper process (pid={first_helper_pid}) died"
    )
    tatolabd.await_stderr_containing("a new one starts in")
    sink_reports_before_the_death_was_surfaced = sum(
        1
        for line in tatolabd.stderr_lines[: tatolabd.stderr_lines.index(death_line) + 1]
        if "MARKER:SINK_PID" in line
    )
    restarted_helper_pid = tatolabd.await_marker("ABOUT_TO_DIE", occurrence=2)["pid"]
    assert restarted_helper_pid != first_helper_pid, "a restart is a new helper process"
    tatolabd.await_marker("SINK_PID", occurrence=sink_reports_before_the_death_was_surfaced + 1)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()


def test_a_helper_that_imported_another_engine_build_is_refused_naming_both_builds(
    start_tatolabd_running_stream: StartTatolabdRunningStream, tmp_path: Path
):
    child_startup_directory = tmp_path / "stale-build-project"
    child_startup_directory.mkdir()
    handed_engine_build_id_record = tmp_path / "engine-build-id-tatolabd-handed-over"
    (child_startup_directory / "sitecustomize.py").write_text(
        "import os\n"
        "import sys\n"
        f"sys.path.append({str(STREAM_ON_RUNTIME_SUITE_DIRECTORY)!r})\n"
        "if 'STREAMLIB_ENTRYPOINT' in os.environ:\n"
        f"    with open({str(handed_engine_build_id_record)!r}, 'w') as record:\n"
        "        record.write(os.environ.get('STREAMLIB_ENGINE_BUILD_ID', ''))\n"
        f"    os.environ['STREAMLIB_ENGINE_BUILD_ID'] = {ENGINE_BUILD_ID_OF_ANOTHER_BUILD!r}\n"
    )

    tatolabd = start_tatolabd_running_stream(
        run_graph_generator(stale_build_labelled_source),
        project_directory=child_startup_directory,
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    tatolabd.await_stderr_containing(HELPER_DIED_SETTING_UP_LOG_LINE_FRAGMENT)
    node_states = {
        node["name"]: node.get("components", {}).get("state")
        for node in tatolabd.local_api_client().graph(stream_id)["nodes"]
    }
    tatolabd.interrupt()
    tatolabd.await_exit()

    tatolabd_engine_build_id = handed_engine_build_id_record.read_text()
    assert tatolabd_engine_build_id, "tatolabd handed its helper no engine build id"
    refusal = (
        "[reportsitsownprocesssource] its helper process died before it finished setting "
        "up. Its standard error ended with:\n"
        f"[streamlib] this helper imported engine build {tatolabd_engine_build_id}"
    )
    assert refusal in tatolabd.stderr_text, tatolabd.recent_stderr()
    assert f"its parent is engine build {ENGINE_BUILD_ID_OF_ANOTHER_BUILD}" in tatolabd.stderr_text
    assert "reportsitsownprocesssource" in node_states, node_states
    assert node_states["reportsitsownprocesssource"] != RUNNING_NODE_STATE, (
        f"the node started anyway: {node_states}"
    )
