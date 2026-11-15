# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The behavioural gate on where a Python class node runs.

Every `@node.process` class runs in its own child process of `tatolabd` — own
interpreter, own GIL: `tatolabd` never imports the processor's module, and the
pid a bag was produced in is not `tatolabd`'s.
"""

from __future__ import annotations

import math
import time
from collections.abc import Callable
from pathlib import Path

import pytest

from conftest import TatolabdUnderTest
from helper_placement_observation import (
    StartTatolabdRunningStream,
    the_first_value_batch_read_after,
)
from helper_placement_processors import (
    MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE,
    RAW_MESSAGE_PAYLOAD_BYTES,
    STEPPED_LFO_STEP_MS,
)
from helper_placement_streams import (
    first_labelled_source_into_sink,
    function_node_run_as_python_into_a_class_lending_it_to_native_code,
    function_node_run_as_python_into_python_records_sink,
    function_nodes_read_by_two_native_converters_into_python_records_sink,
    lfo_into_python_sink_reporting_each_batch_it_reads,
    lfo_into_remap_into_python_sink,
    native_channels_to_records_into_python_records_sink,
    python_level_source_through_a_gate_into_a_native_converter,
    python_source_through_a_gate_into_python_sink,
    python_source_through_a_gate_that_crashes_on_cue_into_python_sink,
    python_source_through_a_gate_into_two_python_sinks,
    python_source_through_a_rust_gate_into_python_sink,
    only_labelled_source_into_sink,
    raw_message_payload_source_into_sink,
    rust_function_nodes_in_islands_beside_python_ones,
    stateful_counter_read_by_two_python_sinks,
    two_lfos_each_read_by_its_own_python_sink_on_a_stepped_clock,
    two_labelled_sources_each_into_its_own_sink,
)
from helper_process_observation import (
    assert_runs_in_a_process_of_its_own_beneath,
    helper_process_ids_started_in,
)


pytestmark = pytest.mark.requires_gpu


def importing_process_ids_recorded_in(module_import_record_directory: Path) -> "dict[int, int]":
    """Each process that imported the placement module, mapped to its parent's pid."""
    return {
        int(record.name): int(record.read_text() or 0)
        for record in module_import_record_directory.iterdir()
    }


def test_adding_a_processor_loads_nothing_into_the_app(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
    tmp_path: Path,
):
    """Every import of the processor's module is an interpreter `tatolabd` started.

    The compile in the project's interpreter, the describe that learns its
    ports at load, and the helper hosting it are each a process `tatolabd`
    started, never `tatolabd` itself. Checked once the stream has loaded and again once
    bags are flowing: a host that constructed the class lazily, on its first
    frame, would pass the first check and fail the second.

    `tatolabd` maps no Python at all; that half is asserted by
    `test_processor_interpreter_lend.py`.
    """
    module_import_record_directory = tmp_path / "module-import-records"
    module_import_record_directory.mkdir()
    tatolabd = start_tatolabd_running_stream(
        first_labelled_source_into_sink,
        extra_environment={
            MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE: str(module_import_record_directory)
        },
    )
    tatolabd.await_the_latest_stream_loaded()
    imported_by_the_load = importing_process_ids_recorded_in(module_import_record_directory)
    tatolabd.await_marker("SINK_PID")
    imported_while_running = importing_process_ids_recorded_in(module_import_record_directory)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert imported_by_the_load, (
        f"no processor interpreter recorded describing the module, so this proves nothing:\n"
        f"{tatolabd.recent_stderr()}"
    )
    assert tatolabd.pid not in imported_by_the_load, (
        f"`tatolabd` imported the processor's module while loading the stream: "
        f"{imported_by_the_load}"
    )
    assert tatolabd.pid not in imported_while_running, (
        f"`tatolabd` imported the processor's module while running the stream — a host "
        f"constructing the class on its first frame would show here: {imported_while_running}"
    )
    assert set(imported_while_running.values()) == {tatolabd.pid}, (
        f"every import of the processor's module is a processor interpreter `tatolabd` "
        f"started; the importers' parents were {imported_while_running}"
    )


def test_a_bag_is_produced_in_a_process_that_is_not_the_apps(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """The pid rides in the bag, so the claim is about where `process` ran —
    not about what the engine logged it was going to do."""
    tatolabd = start_tatolabd_running_stream(only_labelled_source_into_sink)
    sink_report = tatolabd.await_marker("SINK_PID")
    sink_pid, upstream_pid = sink_report["sink_pid"], sink_report["upstream_pid"]
    assert_runs_in_a_process_of_its_own_beneath(upstream_pid, tatolabd.pid)
    assert_runs_in_a_process_of_its_own_beneath(sink_pid, tatolabd.pid)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert upstream_pid != tatolabd.pid, (
        f"the source produced its bag in tatolabd's own process ({tatolabd.pid})"
    )
    assert sink_pid != tatolabd.pid, (
        f"the sink consumed the bag in tatolabd's own process ({tatolabd.pid})"
    )
    assert sink_pid != upstream_pid, (
        f"both processors shared one process ({sink_pid}) — one processor, one helper"
    )


def test_a_payload_one_helper_writes_reaches_a_reader_in_another_helper_unchanged(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A message's payload is bytes end to end: what a Python node writes is
    what a Python node in another helper borrows, byte for byte, with nothing
    encoding or decoding it on the way. The payload is not msgpack, so any
    hidden decode would refuse it rather than pass it through."""
    tatolabd = start_tatolabd_running_stream(raw_message_payload_source_into_sink)
    dict_write_outcome = tatolabd.await_marker("DICT_WRITE_OUTCOME")
    payload_read = tatolabd.await_marker("RAW_MESSAGE_PAYLOAD_READ")
    source_pid, sink_pid = dict_write_outcome["source_pid"], payload_read["sink_pid"]
    assert_runs_in_a_process_of_its_own_beneath(source_pid, tatolabd.pid)
    assert_runs_in_a_process_of_its_own_beneath(sink_pid, tatolabd.pid)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert source_pid != sink_pid, (
        f"the writer and the reader shared one process ({sink_pid}) — one processor, one helper"
    )
    assert payload_read["payload_type"] == "PortPayloadBorrow", (
        "an untyped read borrows the sample rather than copying it into bytes; "
        f"got {payload_read['payload_type']}"
    )
    assert bytes.fromhex(payload_read["payload_hex"]) == RAW_MESSAGE_PAYLOAD_BYTES, (
        f"the reader saw {payload_read['payload_hex']}, not the "
        f"{RAW_MESSAGE_PAYLOAD_BYTES.hex()} the writer wrote"
    )


def test_a_python_write_refuses_a_dict_naming_bytes(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`write` takes bytes and nothing else, so a dict is refused at the write,
    in the writer's own helper, by a `TypeError` that says what it wanted and
    what it got."""
    tatolabd = start_tatolabd_running_stream(raw_message_payload_source_into_sink)
    dict_write_outcome = tatolabd.await_marker("DICT_WRITE_OUTCOME")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert dict_write_outcome["refusal_type"] == "TypeError", (
        f"writing a dict must raise TypeError; it raised {dict_write_outcome['refusal_type']}"
    )
    refusal_text = dict_write_outcome["refusal_text"]
    assert "bytes" in refusal_text and "dict" in refusal_text, (
        f"the refusal must name bytes and the dict it got: {refusal_text!r}"
    )


def test_two_instances_of_one_class_get_two_processes(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """Registration is per class; placement is per instance."""
    tatolabd = start_tatolabd_running_stream(two_labelled_sources_each_into_its_own_sink)
    # Awaited twice without naming a label: the two instances report in
    # whichever order they finish booting.
    tatolabd.await_marker("SOURCE_PID", occurrence=2)
    reported_pids = {report["pid"] for report in tatolabd.marker_payloads("SOURCE_PID")}
    for reported_pid in reported_pids:
        assert_runs_in_a_process_of_its_own_beneath(reported_pid, tatolabd.pid)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert len(reported_pids) == 2, (
        f"two instances of one class reported {reported_pids} — expected two distinct pids:\n"
        f"{tatolabd.recent_stderr()}"
    )
    assert tatolabd.pid not in reported_pids


def test_a_native_builtin_stays_in_the_app_process(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """The other side of the boundary, discriminated.

    Every Python processor is a child; a native built-in is not.
    `ChannelsToRecords` is statically linked into `tatolabd` and runs on an
    engine thread, in `tatolabd`'s own process — and the way to see that is
    that nothing was spawned for it. A two-processor stream starts exactly one helper, and it
    belongs to the sink.

    This is the clause that keeps the ban from reading as "nothing may run in
    the runtime process". Native built-ins do, by design — their per-frame path
    never enters an interpreter.
    """
    tatolabd = start_tatolabd_running_stream(native_channels_to_records_into_python_records_sink)
    sink_pid = tatolabd.await_marker("RECORDS_SINK_PID")["pid"]
    assert_runs_in_a_process_of_its_own_beneath(sink_pid, tatolabd.pid)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)

    assert helper_pids == [sink_pid], (
        f"a stream of one native built-in and one Python processor started {helper_pids} — "
        f"the built-in must not get a process of its own, and the sink must:\n"
        f"{tatolabd.recent_stderr()}"
    )
    assert sink_pid != tatolabd.pid, f"the Python sink ran in tatolabd's own process ({tatolabd.pid})"


def test_function_nodes_run_inside_the_read_of_the_class_they_feed_with_no_process_of_their_own(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`Lfo -> Remap -> <Python class>` starts exactly one helper, the class's.

    The two function nodes are compiled into the class's helper and evaluated
    at each of its reads: the class reads a value the island computed, one f64
    sample at no regular rate, that moves with the stream's time and stays in
    `Remap`'s 0..1, and no process was started for either function node.
    """
    tatolabd = start_tatolabd_running_stream(lfo_into_remap_into_python_sink)
    values_read = tatolabd.await_marker("FUNCTION_NODE_VALUES_READ")
    sink_pid = values_read["sink_pid"]
    assert_runs_in_a_process_of_its_own_beneath(sink_pid, tatolabd.pid)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)

    assert helper_pids == [sink_pid], (
        f"a stream of two function nodes and one Python class started {helper_pids}; only "
        f"the class gets a process:\n{tatolabd.recent_stderr()}"
    )
    assert values_read["sample_rates"] == [0], (
        f"a control value on a wire states no regular rate; read {values_read['sample_rates']}"
    )
    values = values_read["values"]
    assert values[0] == pytest.approx(0.5), (
        f"the LFO starts at 0, which `Remap` maps to 0.5, and the class first read {values[0]}"
    )
    assert all(0.0 <= value <= 1.0 for value in values), (
        f"`Remap` maps the LFO's -1..1 onto 0..1, and the class read {values}"
    )
    assert len(set(values)) > 1, (
        f"the island is evaluated at each read on the stream's moving clock, and every read "
        f"returned {values[0]}"
    )


def test_islands_no_reader_can_evaluate_run_in_one_function_node_pool_per_stream(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """Two islands read by native built-ins start one pool, beside the Python class.

    A built-in runs in `tatolabd`, which runs no node code, so neither
    `ChannelsToRecords` can evaluate the island it reads. The stream's one
    function node pool evaluates both and publishes each on a channel: the
    class reads the record the first converter makes of `Remap`'s value, and
    the stream started exactly two helpers, the pool's and the class's.

    Fail-without-fix: the link into a converter is refused, since the app
    process runs no node code, and the stream never loads.
    """
    tatolabd = start_tatolabd_running_stream(
        function_nodes_read_by_two_native_converters_into_python_records_sink
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    records_read = tatolabd.await_marker("LEVEL_RECORDS_READ")
    sink_pid = records_read["sink_pid"]
    assert_runs_in_a_process_of_its_own_beneath(sink_pid, tatolabd.pid)
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)

    assert len(helper_pids) == 2 and sink_pid in helper_pids, (
        f"two islands into built-ins and one Python class started {helper_pids}; the class and "
        f"one pool get a process:\n{tatolabd.recent_stderr()}"
    )
    assert_graph_shows_the_graph_it_was_given(graph, "while the stream runs")
    pool = graph["function_node_pool"]
    assert len(pool["config"]["carries"]) == 2, pool
    carried_links = [
        link for link in graph["links"] if "carried_through_the_function_node_pool" in link["components"]
    ]
    assert len(carried_links) == 2, f"`graph` shows each carried link as it was given: {graph['links']}"
    levels = [record["level"] for record in records_read["records"]]
    assert all(0.0 <= level <= 1.0 for level in levels), (
        f"`Remap` maps the LFO onto 0..1, and the class read {levels}"
    )
    assert len(set(levels)) > 1, f"the pool evaluates the island on a moving clock: {levels}"


def the_first_gated_count_batch_read_after(
    tatolabd: TatolabdUnderTest, monotonic_ns: int, *, from_occurrence: int
) -> "tuple[dict, int]":
    """The first batch of gated counts the sink started reading after `monotonic_ns`, and its occurrence."""
    occurrence = from_occurrence
    while True:
        batch = tatolabd.await_marker("GATED_COUNT_BATCH_READ", occurrence=occurrence)
        if batch["started_at_monotonic_ns"] > monotonic_ns:
            return batch, occurrence
        occurrence += 1


def a_gated_count_batch_read_wholly_after_a_live_edit(
    tatolabd: TatolabdUnderTest, edit_returned_at_monotonic_ns: int, *, from_occurrence: int
) -> "tuple[dict, int]":
    """The batch after the first one the sink started once a live edit returned, which the sink reads with the edit applied.

    A live pin edit returns before the sink's helper has installed the island
    it changes, so the first batch started after it may span the change.
    """
    _, first_occurrence = the_first_gated_count_batch_read_after(
        tatolabd, edit_returned_at_monotonic_ns, from_occurrence=from_occurrence
    )
    batch_after_it = tatolabd.await_marker("GATED_COUNT_BATCH_READ", occurrence=first_occurrence + 1)
    return batch_after_it, first_occurrence + 1


@pytest.mark.parametrize(
    "gated_stream",
    [python_source_through_a_gate_into_python_sink, python_source_through_a_rust_gate_into_python_sink],
    ids=["python-gate", "rust-gate"],
)
def test_a_gate_hands_a_python_classs_messages_on_unread_while_open_and_nothing_while_closed(
    start_tatolabd_running_stream: StartTatolabdRunningStream, gated_stream: Callable[..., None]
):
    """`<Python class> -> Gate -> <Python class>`, with a Python
    `Gate` and with a Rust one: the sink's helper reads the source's channel and
    evaluates the gate inside each read, with no process for it. While open the
    sink reads each count once, in order; closed live, it reads nothing, not the
    last count; opened again, it reads counts again.
    """
    tatolabd = start_tatolabd_running_stream(gated_stream)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    opened, occurrence = the_first_gated_count_batch_read_after(tatolabd, 0, from_occurrence=2)
    counts = opened["counts"]
    assert counts, f"an open gate hands the source's counts on, and the sink read none: {opened}"
    assert counts == sorted(set(counts)), (
        f"each count is handed on once, in order, and the sink read {counts}"
    )

    graph = local_api.call_tool("graph", {"stream": stream_id})
    (gate,) = [node for node in graph["nodes"] if node["name"] == "gate"]

    def set_open_live(is_open: bool) -> None:
        local_api.call_tool(
            "set_pin_value",
            {
                "stream": local_api.stream_asset_of(stream_id),
                "node": gate["id"],
                "input": "open",
                "value": is_open,
                "mode": "live",
            },
        )

    set_open_live(False)
    closed, occurrence = a_gated_count_batch_read_wholly_after_a_live_edit(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert closed["counts"] == [], (
        f"a closed gate hands out nothing, never the last count, and the sink read {closed}"
    )
    assert closed["sink_pid"] == opened["sink_pid"], "a pin edit restarts nothing"

    set_open_live(True)
    reopened, _ = a_gated_count_batch_read_wholly_after_a_live_edit(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert reopened["counts"] and reopened["counts"][0] > counts[-1], (
        f"opened again, the gate hands on the counts written since, and the sink read {reopened}"
    )
    assert reopened["counts"] == sorted(set(reopened["counts"])), (
        f"opened again, each count is handed on once, in order, and the sink read {reopened}"
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)
    assert len(helper_pids) == 2, (
        f"only the two classes get a process, and the stream started {helper_pids}:\n"
        f"{tatolabd.recent_stderr()}"
    )


def test_a_crash_moves_a_gate_reading_a_python_class_out_of_its_reader_and_the_reader_reads_on_through_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`<Python class> -> crash-on-cue gate -> <Python class>`: the gate runs
    inside the sink's reads until its `crash` pin is set live, when its call
    takes the sink's helper down. Before the sink's next helper starts, the gate
    moves into a helper of its own, which reads the source's channel itself;
    once `crash` is cleared live, the sink reads each count once, in order,
    through it, and the sink's helper started only twice.

    Fail-without-fix: a function node reading a class node is refused promotion,
    so it stays inline and the sink's every new helper calls it and dies.
    """
    tatolabd = start_tatolabd_running_stream(python_source_through_a_gate_that_crashes_on_cue_into_python_sink)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    inline, occurrence = the_first_gated_count_batch_read_after(tatolabd, 0, from_occurrence=2)
    assert inline["counts"], f"the inline gate hands the source's counts on, and the sink read none: {inline}"

    graph = local_api.call_tool("graph", {"stream": stream_id})
    (gate,) = [node for node in graph["nodes"] if node["name"] == "gate"]

    def set_crash_live(crash: bool) -> None:
        local_api.call_tool(
            "set_pin_value",
            {"stream": local_api.stream_asset_of(stream_id), "node": gate["id"], "input": "crash", "value": crash, "mode": "live"},
        )

    set_crash_live(True)
    tatolabd.await_stderr_containing("promoted out of its islands into a helper process of its own")
    set_crash_live(False)
    cleared_at_monotonic_ns = time.monotonic_ns()
    occurrence = len(tatolabd.marker_payloads("GATED_COUNT_BATCH_READ"))
    while True:
        first_with_counts, occurrence = the_first_gated_count_batch_read_after(
            tatolabd, cleared_at_monotonic_ns, from_occurrence=occurrence + 1
        )
        if first_with_counts["counts"]:
            break
    promoted = tatolabd.await_marker("GATED_COUNT_BATCH_READ", occurrence=occurrence + 1)
    graph = local_api.call_tool("graph", {"stream": stream_id})
    sink_pids = {batch["sink_pid"] for batch in tatolabd.marker_payloads("GATED_COUNT_BATCH_READ")}
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert promoted["sink_pid"] != inline["sink_pid"], (
        f"the crashed helper is replaced by a new process:\n{tatolabd.recent_stderr()}"
    )
    assert len(sink_pids) == 2, (
        f"the sink's helper started {len(sink_pids)} times; once the gate moved out, its crash "
        f"no longer ends the sink's helper:\n{tatolabd.recent_stderr()}"
    )
    counts = promoted["counts"]
    assert counts and counts == sorted(set(counts)), (
        f"the promoted gate hands each count on once, in order, and the sink read {promoted}"
    )
    assert counts[0] > inline["counts"][-1], (
        f"the promoted gate reads the source's channel as it runs now, and the sink read {promoted}"
    )
    (gate,) = [node for node in graph["nodes"] if node["name"] == "gate"]
    promotion = gate["components"]["metrics"]["promoted_out_of_its_readers_islands"]
    assert promotion["promotions"] == 1, promotion
    assert promotion["after_crashing_the_helper_of"] == "sink", promotion
    assert "function node `gate` crashed its helper process during its call" in promotion["reason"], (
        promotion
    )
    assert gate["components"]["ran_compiled"] is False, gate


def test_a_gate_reading_a_python_class_hands_each_message_on_to_each_of_two_python_classes(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`<Python class> -> Gate -> two <Python class>es`: each sink's helper reads
    the source's channel and evaluates `Gate` inside its own reads, so each
    reads the source's counts once, in order, with no process for `Gate`.
    """
    tatolabd = start_tatolabd_running_stream(python_source_through_a_gate_into_two_python_sinks)
    tatolabd.await_the_latest_stream_loaded()
    counts_read_by_each_sink: "dict[int, list[int]]" = {}
    occurrence = 2
    while len(counts_read_by_each_sink) < 2 or not all(counts_read_by_each_sink.values()):
        batch = tatolabd.await_marker("GATED_COUNT_BATCH_READ", occurrence=occurrence)
        counts_read_by_each_sink.setdefault(batch["sink_pid"], []).extend(batch["counts"])
        occurrence += 1
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    for sink_pid, counts in counts_read_by_each_sink.items():
        assert counts == sorted(set(counts)), (
            f"each sink reads each count once, in order, and sink {sink_pid} read {counts}"
        )
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)
    assert len(helper_pids) == 3 and set(counts_read_by_each_sink) <= set(helper_pids), (
        f"only the three classes get a process, and the stream started {helper_pids}:\n"
        f"{tatolabd.recent_stderr()}"
    )


def test_the_pool_reads_a_python_class_for_a_gate_a_native_converter_reads(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`<Python class> -> Gate -> ChannelsToRecords -> <Python class>`: the
    converter runs no node code, so the stream's pool reads the source's
    channel, evaluates `Gate` and hands each level on unread, in its family.
    """
    tatolabd = start_tatolabd_running_stream(
        python_level_source_through_a_gate_into_a_native_converter
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    records_read = tatolabd.await_marker("LEVEL_RECORDS_READ")
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    (carry,) = graph["function_node_pool"]["config"]["carries"]
    assert carry["family"] == "message", f"the pool hands `Gate`'s message on unread: {carry}"
    levels = [record["level"] for record in records_read["records"]]
    assert levels == sorted(levels) and len(set(levels)) > 1 and levels[0] >= 1.0, (
        f"the pool hands the source's levels on in order, and the sink read {levels}"
    )
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)
    assert len(helper_pids) == 3 and records_read["sink_pid"] in helper_pids, (
        f"the source, the sink and one pool get a process, and the stream started {helper_pids}:\n"
        f"{tatolabd.recent_stderr()}"
    )


def test_remap_added_and_removed_live_between_lfo_and_a_python_class_changes_what_it_reads(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`Lfo -> <Python class>`, then `Remap` linked in between and taken out,
    all while the class runs: each edit is handed to the running class as its
    island, so what it reads swings -1..1, then stays in `Remap`'s 0..1, then
    swings -1..1 again, in the one process the class started in.
    """
    tatolabd = start_tatolabd_running_stream(lfo_into_python_sink_reporting_each_batch_it_reads)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    direct, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    sink_pid = direct["sink_pid"]
    assert direct["lowest"] < -0.5 and direct["highest"] > 0.5, (
        f"a second of a 1 Hz `Lfo` swings -1..1, and the class read {direct}"
    )

    graph = local_api.call_tool("graph", {"stream": stream_id})
    uuid_of = {node["name"]: node["id"] for node in graph["nodes"]}

    def end_of(node_uuid: str, port: str) -> "dict[str, str]":
        return {"node": node_uuid, "port": port}

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(
            tool, {**arguments, "stream": local_api.stream_asset_of(stream_id), "mode": "live"}
        )

    def the_link_into_the_sink() -> str:
        (link,) = [
            link
            for link in local_api.call_tool("graph", {"stream": stream_id})["links"]
            if link["target"] == {"node": "sink", "port": "brightness"}
        ]
        return link["id"]

    (remap,) = edit_live("add_node", {"type": "helper_placement_function_nodes:Remap", "name": "remap"})[
        "added_nodes"
    ]
    edit_live(
        "connect",
        {"source": end_of(uuid_of["lfo"], "result"), "target": end_of(remap["node"], "x")},
    )
    edit_live(
        "disconnect",
        {
            "source": end_of(uuid_of["lfo"], "result"),
            "target": end_of(uuid_of["sink"], "brightness"),
        },
    )
    edit_live(
        "connect",
        {
            "source": end_of(remap["node"], "result"),
            "target": end_of(uuid_of["sink"], "brightness"),
        },
    )
    assert (
        local_api.await_link_state(the_link_into_the_sink(), "wired", stream=stream_id)
        == "wired"
    )
    remapped_from_ns = time.monotonic_ns()
    graph = local_api.call_tool("graph", {"stream": stream_id})
    hosts = {
        node["name"]: node["components"].get("host")
        for node in graph["nodes"]
        if node["name"] in ("lfo", "remap")
    }
    assert hosts == {
        "lfo": {"inside_the_read_of": ["sink"]},
        "remap": {"inside_the_read_of": ["sink"]},
    }, f"`graph` names the class each function node runs inside the read of: {hosts}"
    remapped, occurrence = the_first_value_batch_read_after(
        tatolabd, remapped_from_ns, from_occurrence=occurrence + 1
    )
    assert 0.0 <= remapped["lowest"] and remapped["highest"] <= 1.0, (
        f"`Remap` maps the LFO's -1..1 onto 0..1, and the class read {remapped}"
    )
    assert remapped["highest"] - remapped["lowest"] > 0.5, (
        f"the `Lfo` keeps swinging through `Remap`: {remapped}"
    )

    edit_live("remove_node", {"node": remap["node"]})
    edit_live(
        "connect",
        {
            "source": end_of(uuid_of["lfo"], "result"),
            "target": end_of(uuid_of["sink"], "brightness"),
        },
    )
    assert (
        local_api.await_link_state(the_link_into_the_sink(), "wired", stream=stream_id)
        == "wired"
    )
    direct_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert direct_again["lowest"] < -0.5 and direct_again["highest"] > 0.5, (
        f"with `Remap` gone the class reads the `Lfo` directly again, and read {direct_again}"
    )
    assert {remapped["sink_pid"], direct_again["sink_pid"]} == {sink_pid}, (
        "each edit reaches the running class; none restarts its helper"
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert helper_process_ids_started_in(tatolabd.stderr_text) == [sink_pid], (
        f"the function nodes added and removed live got no process of their own:\n"
        f"{tatolabd.recent_stderr()}"
    )


FUNCTION_NODE_POOL_NODE_TYPE = "tatolab.runtime._function_node_pool:FunctionNodePool"


def assert_graph_shows_the_graph_it_was_given(graph: dict, when: str) -> None:
    """Require `graph` to show no pool node and every link's ends to be nodes it shows, so its
    output loads and an editor never draws a node no graph document holds."""
    node_names = {node["name"] for node in graph["nodes"]}
    assert all(node["type"] != FUNCTION_NODE_POOL_NODE_TYPE for node in graph["nodes"]), (
        f"{when}, the pool is no node of the graph: {graph['nodes']}"
    )
    assert all(
        link["source"]["node"] in node_names and link["target"]["node"] in node_names
        for link in graph["links"]
    ), f"{when}, each link's ends are nodes `graph` shows: {graph['links']}"


def test_a_stream_whose_pool_runs_takes_an_edit_and_stops_and_plays_again(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A stream whose pool runs shows the graph it was given, takes an edit applied by
    reconciling the live graph to its document, and stops and plays again with the pool.

    Fail-without-fix: `graph` shows the pool as a node, a reconcile treats it as a node the
    document lacks, and the put-back of a stop connects a link to a pool port no pool has.
    """
    tatolabd = start_tatolabd_running_stream(
        python_level_source_through_a_gate_into_a_native_converter
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    tatolabd.await_marker("LEVEL_RECORDS_READ")
    local_api = tatolabd.local_api_client()
    (listed,) = [listed for listed in local_api.list_streams() if listed["stream"] == stream_id]
    project_directory = listed["location"]["project_directory"]
    stream_asset = listed["location"]["stream_asset"]

    running = local_api.call_tool("graph", {"stream": stream_id})
    assert_graph_shows_the_graph_it_was_given(running, "while the stream runs")
    (carried_link,) = [
        link for link in running["links"] if "carried_through_the_function_node_pool" in link["components"]
    ]
    local_api.call_tool(
        "add_note",
        {
            "stream": stream_asset,
            "text": "the pool carries the gate",
            "position": [0, 0],
            "size": [200, 100],
            "author": {"kind": "agent", "name": "test"},
        },
    )
    edited = local_api.call_tool("graph", {"stream": stream_id})
    assert_graph_shows_the_graph_it_was_given(edited, "after an edit")
    assert [link["id"] for link in edited["links"] if link["id"] == carried_link["id"]], (
        f"the edit keeps the carried link: {edited['links']}"
    )
    assert len(edited["function_node_pool"]["config"]["carries"]) == 1, edited["function_node_pool"]

    stopped = local_api.call_tool("stop_playback", {"project_directory": project_directory})
    stop_ended = local_api.stream_action_once_ended(stopped["action"])
    assert stop_ended["state"] == "done", f"the stop failed: {stop_ended}"
    assert_graph_shows_the_graph_it_was_given(
        local_api.call_tool("graph", {"stream": stream_id}), "held stopped"
    )

    played = local_api.call_tool("start_playback", {"project_directory": project_directory})
    play_ended = local_api.stream_action_once_ended(played["action"])
    assert play_ended["state"] == "done", f"playing again failed: {play_ended}"
    records_read_again = tatolabd.await_marker("LEVEL_RECORDS_READ", occurrence=2)
    played_again = local_api.call_tool("graph", {"stream": stream_id})
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert_graph_shows_the_graph_it_was_given(played_again, "played again")
    assert len(played_again["function_node_pool"]["config"]["carries"]) == 1, played_again[
        "function_node_pool"
    ]
    assert len(records_read_again["records"]) > 0


def test_a_function_node_run_as_python_that_a_native_half_reads_runs_in_the_pool(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A Python body a native half reads runs in the pool, so the lend is taken.

    `LabelsTheLevel` gives a `dict`, which no compiled body carries, and its
    describe says so. Its reader is driven manually, as a class lending ports
    to native code is, and a native read holds no Python interpreter lock, so
    the stream places the node in its pool before it runs: the class's port is
    fed by a channel from the pool, and lending it to native code is taken.

    Fail-without-fix: the island stays inside the class's read, and the lend
    is refused naming `labeller`.
    """
    tatolabd = start_tatolabd_running_stream(
        function_node_run_as_python_into_a_class_lending_it_to_native_code
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    lend = tatolabd.await_marker("LABEL_LEND")
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)

    assert lend["refused"] is None, f"the lend was refused: {lend['refused']}"
    assert len(helper_pids) == 2 and lend["pid"] in helper_pids, (
        f"a Python body read by a native half started {helper_pids}; the class and one pool "
        f"get a process:\n{tatolabd.recent_stderr()}"
    )
    pool = graph["function_node_pool"]
    assert pool["config"]["carries"] == [
        {"input_port": "island_0", "output_port": "island_0_output", "family": "records"}
    ], pool


def test_a_function_node_that_does_not_compile_runs_as_python_inside_the_read_of_the_class_it_feeds(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`LabelsTheLevel -> <Python class>` starts exactly one helper, the class's.

    `LabelsTheLevel` gives a `dict`, which no compiled body carries, so it
    runs as Python inside the class's helper, called at each of its reads: the
    class reads the dict it gave as a Records message, no process was started
    for it, and `graph` shows it ran uncompiled.
    """
    tatolabd = start_tatolabd_running_stream(function_node_run_as_python_into_python_records_sink)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    records_read = tatolabd.await_marker("RECORDS_READ")
    sink_pid = records_read["sink_pid"]
    assert_runs_in_a_process_of_its_own_beneath(sink_pid, tatolabd.pid)
    graph = tatolabd.local_api_client().call_tool("graph", {"stream": stream_id})
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)

    assert helper_pids == [sink_pid], (
        f"a stream of one function node and one Python class started {helper_pids}; only the "
        f"class gets a process:\n{tatolabd.recent_stderr()}"
    )
    assert records_read["records"] == [{"label": "high", "spelled": "0.75"}] * 10, (
        f"the class reads the dict the node gave at its pin's 0.75: {records_read['records']}"
    )
    nodes_by_name = {node["name"]: node for node in graph["nodes"]}
    assert nodes_by_name["labeller"]["components"]["ran_compiled"] is False, nodes_by_name["labeller"]


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


def test_rust_function_nodes_run_inside_the_read_of_the_class_they_feed_beside_python_ones(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A fixture module's Rust function nodes join islands beside Python ones
    and are evaluated inside each reader's read, with no process of their own.

    `Lfo -> ScaleAndOffset (Rust) -> Clamp` maps the LFO's -1..1 onto 0..1, and
    `CountsItsOwnCalls (Rust) -> Remap` hands its ordered reader each count once,
    as `(count + 1) / 2`. Only the two Python classes get a helper, and the
    catalog and `graph` show each Rust node as `function` / `rust`.
    """
    tatolabd = start_tatolabd_running_stream(rust_function_nodes_in_islands_beside_python_ones)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    values_read = tatolabd.await_marker("FUNCTION_NODE_VALUES_READ")
    counts_read = tatolabd.await_marker("COUNTS_READ_IN_ORDER")
    local_api = tatolabd.local_api_client()
    graph = local_api.call_tool("graph", {"stream": stream_id})
    registry = local_api.registry(stream_id)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    helper_pids = helper_process_ids_started_in(tatolabd.stderr_text)

    assert sorted(helper_pids) == sorted([values_read["sink_pid"], counts_read["pid"]]), (
        f"a stream of five function nodes and two Python classes started {helper_pids}; "
        f"only the classes get a process:\n{tatolabd.recent_stderr()}"
    )
    values = values_read["values"]
    assert values[0] == pytest.approx(0.5), (
        f"the LFO starts at 0, which `ScaleAndOffset` maps to 0.5, and the class first read "
        f"{values[0]}"
    )
    assert all(0.0 <= value <= 1.0 for value in values), values
    assert len(set(values)) > 1, f"the island moves with the stream's clock: {values}"
    counts = [2.0 * value - 1.0 for value in counts_read["counts"]]
    assert counts == [float(count) for count in range(1, len(counts) + 1)], (
        f"the Rust counter counts each read once from its first, and its reader read {counts}"
    )

    rust_node_types = {
        "rust_function_node_fixture:ScaleAndOffset",
        "rust_function_node_fixture:CountsItsOwnCalls",
    }
    assert rust_node_types <= {graph_node["type"] for graph_node in graph["nodes"]}, graph["nodes"]
    for graph_node in graph["nodes"]:
        if graph_node["type"] in rust_node_types:
            assert (graph_node.get("kind"), graph_node.get("language")) == ("function", "rust"), (
                graph_node
            )
    catalog_entries = {entry["type"]: entry for entry in registry["nodes"]}
    for rust_node_type in rust_node_types:
        entry = catalog_entries[rust_node_type]
        assert (entry["kind"], entry.get("language")) == ("function", "rust"), entry
    rust_counter = next(node for node in graph["nodes"] if node["name"] == "rust_counter")
    assert rust_counter["components"]["function_node_state"]["calls"] >= counts[-1], (
        rust_counter["components"]
    )


def test_a_stateful_function_node_replays_the_same_values_on_a_stepped_clock(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """The state a stateful node carries is a parameter, not a side effect.

    Two runs of the same `Lfo` island, each in its own reader's helper, read
    the same value at each step: the one the step's stream time gives, a sine
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
        f"two runs of one stateful island on one stepped clock read different values: "
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
