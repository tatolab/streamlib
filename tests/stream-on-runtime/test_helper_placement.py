# Copyright (c) 2025 Jonathan Fontanez
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
from helper_placement_processors import (
    MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE,
    CALLS_RETURNED_BEFORE_A_CALL_HANGS,
    COUNT_AT_WHICH_A_FRESH_COUNT_SINK_DIES,
    RAW_MESSAGE_PAYLOAD_BYTES,
    STEPPED_LFO_STEP_MS,
)
from helper_placement_streams import (
    clamp_with_its_input_linked_to_nothing_into_python_sink,
    dies_abruptly_beside_a_survivor_pair,
    first_labelled_source_into_sink,
    function_node_run_as_python_into_a_class_lending_it_to_native_code,
    function_node_run_as_python_into_python_records_sink,
    function_node_run_as_python_that_crashes_into_python_sink,
    function_node_slower_than_the_pools_frame_interval_read_by_a_native_converter,
    function_node_that_spins_forever_into_python_sink,
    function_nodes_read_by_two_native_converters_into_python_records_sink,
    held_level_read_by_two_python_sinks,
    level_scaled_by_its_gain_into_python_sink,
    lfo_into_python_sink_reporting_each_batch_it_reads,
    python_class_reporting_its_configured_level,
    python_sink_reading_the_pin_value_of_its_unlinked_input,
    lfo_into_remap_into_python_sink,
    native_channels_to_records_into_python_records_sink,
    one_probe_forking_a_worker_that_outlives_it,
    python_level_source_through_a_gate_into_a_native_converter,
    python_source_through_a_gate_into_python_sink,
    python_source_through_a_gate_that_crashes_on_cue_into_python_sink,
    python_source_through_a_gate_into_two_python_sinks,
    python_source_through_a_rust_gate_into_python_sink,
    one_probe_sleeping_through_its_own_setup,
    one_probe_sleeping_through_its_own_shutdown,
    pooled_counter_beside_a_pooled_node_that_hangs,
    only_labelled_source_into_sink,
    raw_message_payload_source_into_sink,
    reaped_labelled_source_into_sink,
    rust_function_nodes_in_islands_beside_python_ones,
    stateful_counter_into_python_sink_that_dies_once,
    stateful_counter_read_by_two_python_sinks,
    two_lfos_each_read_by_its_own_python_sink_on_a_stepped_clock,
    stale_build_labelled_source,
    two_labelled_sources_each_into_its_own_sink,
)
from helper_process_observation import (
    a_process_is_gone_within,
    assert_runs_in_a_process_of_its_own_beneath,
    helper_process_ids_started_in,
    helper_process_is_still_alive,
)
from local_api_client import RUNNING_NODE_STATE
from runtime_unit_under_test import STREAM_ON_RUNTIME_SUITE_DIRECTORY
from tatolab.stream import StreamBuilder, run_graph_generator

pytestmark = pytest.mark.requires_gpu

StartTatolabdRunningStream = Callable[..., TatolabdUnderTest]

# The ladder's own worst case is a second of interrupt plus five of teardown
# plus the group's grace; a helper that answers at once is far inside it. This
# is what separates "the ladder ran" from "the thirty-second callback did".
LADDER_BUDGET_SECONDS = 15.0

#: How `tatolabd` refuses a processor whose helper exited during its setup.
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


def the_first_value_batch_read_after(
    tatolabd: TatolabdUnderTest, monotonic_ns: int, *, from_occurrence: int
) -> "tuple[dict, int]":
    """The first batch the sink started reading after `monotonic_ns`, and its occurrence."""
    occurrence = from_occurrence
    while True:
        batch = tatolabd.await_marker("FUNCTION_NODE_VALUE_BATCH_READ", occurrence=occurrence)
        if batch["started_at_monotonic_ns"] > monotonic_ns:
            return batch, occurrence
        occurrence += 1


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


def test_no_helper_survives_the_app(start_tatolabd_running_stream: StartTatolabdRunningStream):
    """`tatolabd` exiting means every helper was reaped.

    Asserted against the helpers' own pids, which `tatolabd` logs as it starts
    each one. Its process group cannot answer this: every helper leads a group
    of its own — that is what keeps a terminal Ctrl-C from reaching them
    directly — so an assertion on `tatolabd`'s group passes over a leak.

    A survivor holds this processor's iceoryx2 ports open, and the next run
    fails to open them — which reads as a transport bug rather than a leak.
    """
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


def test_a_processor_asleep_in_its_callback_still_runs_its_teardown(
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


def test_a_processor_interrupted_while_still_setting_up_still_tears_down(
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


def test_a_worker_a_processor_forked_goes_down_with_the_apps_helper(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A processor's descendants die with it.

    Fail-without-fix: the kills target the helper's pid, the worker outlives
    `tatolabd` holding whatever it inherited, and this finds it still running.
    """
    tatolabd = run_until_marker_then_interrupt(
        start_tatolabd_running_stream, one_probe_forking_a_worker_that_outlives_it, "WORKER_PID"
    )
    worker_pid = tatolabd.marker_payloads("WORKER_PID")[0]["worker_pid"]

    assert a_process_is_gone_within(worker_pid, 2.0), (
        f"the worker a processor forked outlived the tatolabd that spawned it:\n"
        f"{tatolabd.recent_stderr()}"
    )


def test_a_crashed_helper_is_restarted_in_a_new_process_and_the_pipeline_keeps_running(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """The owner's crash policy: surface, restart, keep running.

    A processor that takes its own process down mid-run is reported, a new
    helper process takes its place after a backoff, and the rest of the stream
    is unaffected. What notices is the Manual loop's hundred-millisecond poll
    asking the process itself; the bridge reader seeing EOF is the second
    signal, and a descendant holding that socket defers it indefinitely.

    Fail-without-fix: break the poll and the death goes unreported until
    shutdown; latch the death and no second helper ever starts.
    """
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
    # The survivors were already producing before the crash and must still be
    # producing after it — the stream is not brought down with one processor.
    tatolabd.await_marker("SINK_PID", occurrence=sink_reports_before_the_death_was_surfaced + 1)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()


def test_a_helper_that_imported_another_engine_build_is_refused_naming_both_builds(
    start_tatolabd_running_stream: StartTatolabdRunningStream, tmp_path: Path
):
    """The whole handshake through a real `tatolabd`: it hands its build id
    over, the child compares it with its own and refuses on raw stderr, and the
    processor's start is refused by name carrying what the child wrote — the
    only place an operator reads which two builds disagreed.

    The child is made to see another build the one way a test can reach it
    before `main()` runs: a `sitecustomize` in the stream's project directory —
    on the child's `PYTHONPATH` — rewrites the id `tatolabd` handed it, which is
    what a stale `tatolab.runtime` amounts to from the check's side. Describing
    sets no `STREAMLIB_ENTRYPOINT`, so the load itself is untouched. The id it
    rewrites is recorded first: `tatolabd`'s, which the runtime unit's lend
    shares.

    Fail-without-fix: drop the stderr tail from the refusal and the refusal
    names the processor but neither build.
    """
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
    processor_states = {
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
    assert "reportsitsownprocesssource" in processor_states, processor_states
    assert processor_states["reportsitsownprocesssource"] != RUNNING_NODE_STATE, (
        f"the processor started anyway: {processor_states}"
    )


def test_a_python_class_reads_its_unlinked_inputs_default_then_each_pin_value_set_live(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A Python class whose input is linked to nothing reads the 0.25 its port
    declares; a live `set_pin_value` changes what it reads to the pin, in the
    one process it started in, and taking the pin off reads the default again.
    """
    tatolabd = start_tatolabd_running_stream(
        python_sink_reading_the_pin_value_of_its_unlinked_input
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    defaulted, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert defaulted["lowest"] == defaulted["highest"] == pytest.approx(0.25), (
        f"an unlinked input reads the default its port declares, and the class read {defaulted}"
    )

    graph = local_api.call_tool("graph", {"stream": stream_id})
    (sink,) = [node for node in graph["nodes"] if node["name"] == "sink"]

    def set_the_pin_live(value: "float | None") -> None:
        local_api.call_tool(
            "set_pin_value",
            {
                "stream": local_api.stream_asset_of(stream_id),
                "node": sink["id"],
                "input": "brightness",
                "value": value,
                "mode": "live",
            },
        )

    set_the_pin_live(0.75)
    pinned, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert pinned["lowest"] == pinned["highest"] == pytest.approx(0.75), (
        f"a pin value set live is what the unlinked input reads next, and the class read {pinned}"
    )
    assert pinned["sink_pid"] == defaulted["sink_pid"], "a pin edit restarts nothing"
    (sink_after_the_edit,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "sink"
    ]
    pins = sink_after_the_edit["pins"]
    assert pins == {"brightness": 0.75}, f"`graph` shows the pin value set: {pins}"

    set_the_pin_live(None)
    defaulted_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert defaulted_again["lowest"] == defaulted_again["highest"] == pytest.approx(0.25), (
        f"taking the pin off reads the declared default again, and the class read {defaulted_again}"
    )


def test_a_function_node_input_no_value_reaches_refuses_the_streams_start_naming_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`Clamp`'s `x` is linked to nothing, unpinned, and declares no default, so
    the stream is refused as it starts, naming the node and the input."""
    tatolabd = start_tatolabd_running_stream(clamp_with_its_input_linked_to_nothing_into_python_sink)
    failed_stream = tatolabd.stream_runs[-1].failed_stream()
    assert failed_stream is not None, "the stream must be recorded failed"
    _, failed_because = failed_stream
    assert "`clamp`'s input `x`" in failed_because, failed_because
    assert "no value reaches" in failed_because, failed_because


def test_a_function_node_linked_live_with_an_input_no_value_reaches_holds_until_its_pin_is_set(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`Clamp` linked live into a running Python class with its `x` linked to
    nothing holds: `graph` names `x` as waiting, and the class reads nothing
    from it. Setting `x`'s pin live is what the class reads next, and linking
    the `Lfo` into `x` takes the waiting input off `graph`.
    """
    tatolabd = start_tatolabd_running_stream(lfo_into_python_sink_reporting_each_batch_it_reads)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    _, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    uuid_of = {
        node["name"]: node["id"]
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
    }

    def end_of(node_uuid: str, port: str) -> "dict[str, str]":
        return {"node": node_uuid, "port": port}

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(
            tool, {**arguments, "stream": local_api.stream_asset_of(stream_id), "mode": "live"}
        )

    def the_clamp_node() -> dict:
        (clamp_node,) = [
            node
            for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
            if node["name"] == "clamp"
        ]
        return clamp_node

    (clamp,) = edit_live("add_node", {"type": "helper_placement_function_nodes:Clamp", "name": "clamp"})[
        "added_nodes"
    ]
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
            "source": end_of(clamp["node"], "result"),
            "target": end_of(uuid_of["sink"], "brightness"),
        },
    )
    assert the_clamp_node().get("inputs_waiting_for_a_value") == ["x"], (
        f"`graph` names the input the held node waits on: {the_clamp_node()}"
    )

    pinned_from_ns = time.monotonic_ns()
    edit_live("set_pin_value", {"node": clamp["node"], "input": "x", "value": 0.3})
    pinned, occurrence = the_first_value_batch_read_after(
        tatolabd, pinned_from_ns, from_occurrence=occurrence + 1
    )
    assert pinned["lowest"] == pinned["highest"] == pytest.approx(0.3), (
        f"the held node evaluates once its input has a value, and the class read {pinned}"
    )
    assert "inputs_waiting_for_a_value" not in the_clamp_node()

    edit_live(
        "connect",
        {"source": end_of(uuid_of["lfo"], "result"), "target": end_of(clamp["node"], "x")},
    )
    clamped, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert 0.0 <= clamped["lowest"] and clamped["highest"] <= 1.0 and (
        clamped["highest"] - clamped["lowest"] > 0.5
    ), f"a link into `x` wins over its pin, clamped to 0..1, and the class read {clamped}"


def test_a_config_field_exposed_live_reads_the_config_until_a_link_drives_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`LevelScaledByItsGain`, its config's `gain` 0.25, read by a running Python
    class. Exposing `gain` live adds a `gain` input that `graph` shows, and with
    nothing linked into it the node still reads the config. Linking an `Lfo` into
    `gain` drives the field, so the class reads the level scaled by the `Lfo`'s
    -1..1 in place of 0.25, and disconnecting it reads the config's 0.25 again.
    """
    tatolabd = start_tatolabd_running_stream(level_scaled_by_its_gain_into_python_sink)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    configured, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert configured["lowest"] == configured["highest"] == pytest.approx(0.25), (
        f"the level 1.0 scaled by the config's gain 0.25, and the class read {configured}"
    )

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(
            tool, {**arguments, "stream": local_api.stream_asset_of(stream_id), "mode": "live"}
        )

    def the_scaled_node() -> dict:
        (scaled_node,) = [
            node
            for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
            if node["name"] == "scaled"
        ]
        return scaled_node

    edit_live(
        "set_exposed_config_fields",
        {"node": the_scaled_node()["id"], "exposed_config_fields": ["gain"]},
    )
    scaled_node = the_scaled_node()
    assert scaled_node.get("exposed_config_fields") == ["gain"], scaled_node
    assert "gain" in [port["name"] for port in scaled_node["ports"]["inputs"]], scaled_node
    assert "inputs_waiting_for_a_value" not in scaled_node, (
        f"an exposed field no link drives reads the config, so nothing waits: {scaled_node}"
    )
    exposed_unlinked, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert exposed_unlinked["lowest"] == exposed_unlinked["highest"] == pytest.approx(0.25), (
        f"an exposed field no link drives reads the config, and the class read {exposed_unlinked}"
    )

    (lfo,) = edit_live("add_node", {"type": "helper_placement_function_nodes:Lfo", "name": "lfo"})["added_nodes"]
    edit_live(
        "connect",
        {
            "source": {"node": lfo["node"], "port": "result"},
            "target": {"node": scaled_node["id"], "port": "gain"},
        },
    )
    driven, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert driven["highest"] - driven["lowest"] > 0.5, (
        f"the `Lfo` linked into `gain` drives the field in place of 0.25, and the class read {driven}"
    )

    edit_live(
        "disconnect",
        {
            "source": {"node": lfo["node"], "port": "result"},
            "target": {"node": scaled_node["id"], "port": "gain"},
        },
    )
    unlinked_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert unlinked_again["lowest"] == unlinked_again["highest"] == pytest.approx(0.25), (
        f"a field its link stops driving reads the config again, and the class read {unlinked_again}"
    )


def test_a_config_field_a_promoted_function_node_exposes_reads_the_config_until_a_link_drives_it(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """`HoldsItsConfiguredLevelCountingItsCalls`, its config's `level` 0.25, is
    stateful and read by two Python classes, so it runs in a helper of its own.
    Exposing `level` live with nothing linked into it leaves the node reading the
    config. Linking an `Lfo` into `level` drives the field, so the classes read
    the `Lfo`'s -1..1 in place of 0.25, and disconnecting it reads the config's
    0.25 again.

    Fail-without-fix: the promoted node keeps its config's 0.25 with the `Lfo`
    linked, and its helper logs that it cannot be reconfigured while running.
    """
    tatolabd = start_tatolabd_running_stream(held_level_read_by_two_python_sinks)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    configured, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert configured["lowest"] == configured["highest"] == pytest.approx(0.25), (
        f"the config's level 0.25, and a class read {configured}"
    )
    assert len(set(helper_process_ids_started_in(tatolabd.stderr_text))) == 3, (
        f"the two readers and the held node each get a helper:\n{tatolabd.recent_stderr()}"
    )

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(
            tool, {**arguments, "stream": local_api.stream_asset_of(stream_id), "mode": "live"}
        )

    (held_node,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "held"
    ]
    edit_live(
        "set_exposed_config_fields",
        {"node": held_node["id"], "exposed_config_fields": ["level"]},
    )
    exposed_unlinked, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    assert exposed_unlinked["lowest"] == exposed_unlinked["highest"] == pytest.approx(0.25), (
        f"an exposed field no link drives reads the config, and a class read {exposed_unlinked}"
    )

    (lfo,) = edit_live("add_node", {"type": "helper_placement_function_nodes:Lfo", "name": "lfo"})["added_nodes"]
    edit_live(
        "connect",
        {
            "source": {"node": lfo["node"], "port": "result"},
            "target": {"node": held_node["id"], "port": "level"},
        },
    )
    driven, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    edit_live(
        "disconnect",
        {
            "source": {"node": lfo["node"], "port": "result"},
            "target": {"node": held_node["id"], "port": "level"},
        },
    )
    unlinked_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert driven["highest"] - driven["lowest"] > 0.5, (
        f"the `Lfo` linked into `level` drives the field in place of 0.25, and a class read {driven}"
    )
    assert unlinked_again["lowest"] == unlinked_again["highest"] == pytest.approx(0.25), (
        f"a field its link stops driving reads the config again, and a class read {unlinked_again}"
    )
    assert "cannot be reconfigured while running" not in tatolabd.stderr_text, (
        f"the promoted node takes the linked value as its config:\n{tatolabd.recent_stderr()}"
    )


def test_a_config_field_a_python_class_exposes_reads_the_config_again_once_its_link_is_removed(
    start_tatolabd_running_stream: StartTatolabdRunningStream,
):
    """A Python class taking live config updates, its config's `level` 0.25.
    Exposing `level` live and linking an `Lfo` into it drives the field, and
    disconnecting the `Lfo` hands the class its config's 0.25 again.

    Fail-without-fix: the class keeps the last value the `Lfo` gave it.
    """
    tatolabd = start_tatolabd_running_stream(python_class_reporting_its_configured_level)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    configured, occurrence = the_first_value_batch_read_after(tatolabd, 0, from_occurrence=1)
    assert configured["lowest"] == configured["highest"] == pytest.approx(0.25), configured

    def edit_live(tool: str, arguments: dict) -> dict:
        return local_api.call_tool(
            tool, {**arguments, "stream": local_api.stream_asset_of(stream_id), "mode": "live"}
        )

    (configured_node,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "configured"
    ]
    edit_live(
        "set_exposed_config_fields",
        {"node": configured_node["id"], "exposed_config_fields": ["level"]},
    )
    (lfo,) = edit_live("add_node", {"type": "helper_placement_function_nodes:Lfo", "name": "lfo"})["added_nodes"]
    lfo_into_the_level = {
        "source": {"node": lfo["node"], "port": "result"},
        "target": {"node": configured_node["id"], "port": "level"},
    }
    edit_live("connect", lfo_into_the_level)
    driven, occurrence = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    edit_live("disconnect", lfo_into_the_level)
    unlinked_again, _ = the_first_value_batch_read_after(
        tatolabd, time.monotonic_ns(), from_occurrence=occurrence + 1
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert driven["highest"] - driven["lowest"] > 0.5, (
        f"the `Lfo` linked into `level` drives the field in place of 0.25, and the class read {driven}"
    )
    assert unlinked_again["lowest"] == unlinked_again["highest"] == pytest.approx(0.25), (
        f"a field its link stops driving reads the config again, and the class read {unlinked_again}"
    )
