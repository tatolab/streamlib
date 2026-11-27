# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Where a stateful function node runs: always in a helper of its own, so its
state has one copy whatever reads it, and a reader wired to it while the
stream plays is one more link out of that helper.
"""

from __future__ import annotations

import math

import pytest

from helper_placement_observation import StartTatolabdRunningStream
from helper_placement_nodes import STEPPED_LFO_STEP_MS
from helper_placement_streams import (
    stateful_counter_read_by_one_python_sink_reporting_each_batch,
    stateful_counter_read_by_two_python_sinks,
    two_lfos_each_read_by_its_own_python_sink_on_a_stepped_clock,
)
from helper_process_observation import helper_process_ids_started_in


pytestmark = pytest.mark.requires_gpu


def test_a_stateful_function_node_two_readers_read_runs_once_in_a_process_of_its_own(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A stateful function node read by two Python classes is evaluated once,
    in a helper of its own, and both read the counts that one evaluation wrote.

    Each reader reads every count in order, so each reads a run of consecutive
    counts from the one counter, and `graph` shows the counter holding a count
    at least as high as any either read, and that its own helper ran it
    compiled.

    Fail-without-fix: the counter runs inside each reader with a copy of its
    state each, and the stream starts only the two readers' helpers.
    """
    tatolabd = start_tatolabd_running_stream(stateful_counter_read_by_two_python_sinks)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    first_reader = tatolabd.await_marker("COUNTS_READ_IN_ORDER")
    second_reader = tatolabd.await_marker("COUNTS_READ_IN_ORDER", occurrence=2)
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)

    reader_pids = {first_reader["pid"], second_reader["pid"]}
    assert len(reader_pids) == 2, (first_reader, second_reader)
    assert len(set(helper_pids)) == 3 and reader_pids < set(helper_pids), (
        f"the two readers and the counter each get a helper, and the stream started "
        f"{helper_pids}:\n{tatolabd.recent_stderr()}"
    )
    for reader in (first_reader, second_reader):
        counts = reader["counts"]
        assert counts == [counts[0] + offset for offset in range(len(counts))], (
            f"a reader of the one counter reads each count it wrote once, in order: {counts}"
        )
    counter = next(node for node in graph["nodes"] if node["name"] == "counter")
    assert counter["components"]["function_node_state"]["calls"] >= max(
        first_reader["counts"][-1], second_reader["counts"][-1]
    ), counter["components"]
    assert counter["components"]["ran_compiled"] is True, counter["components"]


def test_a_second_reader_wired_to_a_playing_stateful_function_node_reads_it_and_the_first_reads_on(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A second Python class added and linked to a playing stateful counter
    reads its counts at once, with no stop: the counter runs in a helper of its
    own, so the new reader is one more link out of it.

    The second reader reads a run of consecutive counts from the one counter,
    not a fresh count from one, and every count it reads the first one reads
    too. The first reader reads every count in order across the edit, none
    skipped and none started over, in the one helper it started in.

    Fail-without-fix: the second link is refused, naming the running first
    reader, until the stream is stopped.
    """
    tatolabd = start_tatolabd_running_stream(
        stateful_counter_read_by_one_python_sink_reporting_each_batch
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    before_the_edit = tatolabd.await_marker("COUNT_BATCH_READ_IN_ORDER")
    assert before_the_edit["reader"] == "first", before_the_edit
    local_api = tatolabd.local_api_client()

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(tool, {**arguments, "stream": stream_id, "mode": "live"})

    (counter,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "counter"
    ]
    (second_reader,) = edit_live(
        "add_node",
        {
            "type": "helper_placement_nodes:ReportsEachBatchOfCountsItReadInOrderAsTheSecondReader",
            "name": "second_reader",
        },
    )["added_nodes"]
    edit_live(
        "connect",
        {
            "source": {"node": counter["id"], "port": "result"},
            "target": {"node": second_reader["node"], "port": "count"},
        },
    )

    occurrence = 1
    second_readers_batch = None
    while second_readers_batch is None:
        occurrence += 1
        batch = tatolabd.await_marker("COUNT_BATCH_READ_IN_ORDER", occurrence=occurrence)
        if batch["reader"] == "second":
            second_readers_batch = batch
    while True:
        occurrence += 1
        batch = tatolabd.await_marker("COUNT_BATCH_READ_IN_ORDER", occurrence=occurrence)
        if batch["reader"] == "first" and batch["counts"][-1] >= second_readers_batch["counts"][-1]:
            break
    links = local_api.call_tool("graph", {"stream": stream_id})["links"]
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)

    assert [link["state"] for link in links] == ["wired", "wired"], links
    batches = tatolabd.marker_payloads("COUNT_BATCH_READ_IN_ORDER")
    first_readers_batches = [batch for batch in batches if batch["reader"] == "first"]
    assert {batch["pid"] for batch in first_readers_batches} == {before_the_edit["pid"]}, (
        f"the first reader reads on in the helper it started in: {first_readers_batches}"
    )
    first_readers_counts = [count for batch in first_readers_batches for count in batch["counts"]]
    assert first_readers_counts == [
        first_readers_counts[0] + offset for offset in range(len(first_readers_counts))
    ], f"the first reader reads each count once, in order, across the edit: {first_readers_counts}"
    second_readers_counts = second_readers_batch["counts"]
    assert second_readers_counts == [
        second_readers_counts[0] + offset for offset in range(len(second_readers_counts))
    ], second_readers_counts
    assert second_readers_counts[0] > before_the_edit["counts"][-1], (
        f"the second reader reads the counter that was counting, not a count of its own from one: "
        f"{second_readers_counts}, after the first read {before_the_edit['counts']}"
    )
    assert set(second_readers_counts) <= set(first_readers_counts), (
        f"both readers read the one counter's values: {second_readers_counts} and "
        f"{first_readers_counts}"
    )
    assert len(set(helper_pids)) == 3 and before_the_edit["pid"] in helper_pids, (
        f"the two readers and the counter each get a helper, and the first reader's is never "
        f"started again: {helper_pids}:\n{tatolabd.recent_stderr()}"
    )


def test_a_stateful_function_node_replays_the_same_values_on_a_stepped_clock(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """The state a stateful node carries is a parameter, not a side effect.

    Two runs of the same `Lfo`, each in a helper of its own read by its own
    class, read the same value at each step: the one the step's stream time gives, a sine
    advanced by exactly one step of stream time per step, whatever the machine
    took to run it.

    Fail-without-fix: with the node's state or time taken from anywhere but
    its parameters, the two runs drift apart and off the step's sine.
    """
    tatolabd = start_tatolabd_running_stream(
        two_lfos_each_read_by_its_own_python_sink_on_a_stepped_clock
    )
    first_run = tatolabd.await_marker("STEPPED_LFO_VALUES_READ")
    second_run = tatolabd.await_marker("STEPPED_LFO_VALUES_READ", occurrence=2)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert first_run["pid"] != second_run["pid"], (first_run, second_run)
    assert first_run["steps"] == second_run["steps"], (
        f"two runs of one stateful node on one stepped clock read different values: "
        f"{first_run['steps']} and {second_run['steps']}"
    )
    first_step_read = first_run["steps"][0][0]
    lfo_rate_hz = 1.0
    for step_index, value in first_run["steps"]:
        stream_seconds_since_the_first_read = (step_index - first_step_read) * STEPPED_LFO_STEP_MS / 1000
        expected = math.sin(2.0 * math.pi * ((lfo_rate_hz * stream_seconds_since_the_first_read) % 1.0))
        assert value == pytest.approx(expected, abs=1e-9), (
            f"at step {step_index} the LFO read {value}, and one step of stream time a step "
            f"puts it at {expected}: {first_run['steps']}"
        )
