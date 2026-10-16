# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Links between two streams on one machine, end to end through `tatolabd`.

The stream that holds a link's input owns it and pulls it from the source
stream's output port by address — `stream_builder.remote_output("a/pattern/video")`
in its function, or MCP `connect` with `from_stream`. Each test runs its two
streams attached on one `tatolabd`, the source loaded as `a` and the reader as
`b`, and reads every outcome off the runtime: the link's state and reason in
`b`'s `graph`, the bags a Python counter reports, the frames a native window
counts at its stop, and a `tatolab tap` on a port only another stream reads.

A started stream needs a GPU context, so the tests that run streams are
`requires_gpu`, and the one ending in a native window is rig-only, since a
window refuses on the software device; a refusal the load or the compile makes
is read on a `tatolabd` that reaches no Vulkan driver.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from conftest import AttachedTatolabRun, StreamRunWithNoVulkanDriverOutcome, TatolabdUnderTest
from helper_process_observation import HELPER_PROCESS_STARTED_LOG_LINE_TEXT, helper_process_ids_started_in
from links_between_streams_probes import bags_counted_marker_of
from links_between_streams_streams import (
    ADDRESS_OF_FIVE_CHUNKS,
    ADDRESS_OF_ONE_CHUNK,
    ADDRESS_WITH_A_DOT_CHUNK,
    ANOTHER_MACHINE,
    COUNTER_PORT,
    PATTERN_COUNTER_NODE,
    PATTERN_NODE,
    PATTERN_PORT,
    READER_STREAM_NAME,
    RELAY_NODE,
    RELAY_PORT,
    SECOND_PATTERN_COUNTER_NODE,
    SECOND_PATTERN_NODE,
    SOURCE_STREAM_NAME,
    WINDOW_NODE,
    WINDOW_PORT,
    a_pattern_and_a_python_relay_each_exposed_private,
    a_pattern_exposed_private,
    a_pattern_left_internal,
    a_two_patterns_each_exposed_private,
    b_linking_into_another_streams_input,
    b_naming_an_address_of_five_chunks,
    b_naming_an_address_of_one_chunk,
    b_naming_an_address_with_a_dot_chunk,
    b_reading_a_pattern_on_another_machine,
    b_reading_both_patterns_into_two_python_counters,
    b_reading_the_pattern_into_a_python_counter,
    b_reading_the_pattern_into_a_python_counter_and_the_relay_into_a_window,
)
from local_api_client import LocalApiClient
from runtime_process_under_test import ENGINE_GRACEFUL_STOP_LOG_LINE

RunStreamOnTatolabdWithNoVulkanDriver = Callable[..., StreamRunWithNoVulkanDriverOutcome]

#: A load is the project's compile, and on the runtime's first start its GPU context.
STREAM_RUNNING_TIMEOUT_SECONDS = 90.0
#: A link into a Python node is `pending` until its helper process has opened its port.
LINK_WIRED_TIMEOUT_SECONDS = 30.0
#: How long a lowered level may take to remove a link reading the port: the
#: cut is spawned the moment `expose` returns, so it lands in milliseconds.
LINK_CUT_BOUND_SECONDS = 2.0
TATOLABD_EXIT_TIMEOUT_SECONDS = 60.0

#: How `b`'s graph renders the source end of a link reading `a`'s ports.
PATTERN_IN_THE_SOURCE_STREAM = {
    "stream": SOURCE_STREAM_NAME,
    "node": PATTERN_NODE,
    "port": PATTERN_PORT,
}
SECOND_PATTERN_IN_THE_SOURCE_STREAM = {
    "stream": SOURCE_STREAM_NAME,
    "node": SECOND_PATTERN_NODE,
    "port": PATTERN_PORT,
}
RELAY_IN_THE_SOURCE_STREAM = {"stream": SOURCE_STREAM_NAME, "node": RELAY_NODE, "port": RELAY_PORT}

#: Why a link to `a`'s pattern waits while `a` is not loaded.
SOURCE_STREAM_NOT_LOADED_REASON = (
    f"the stream `{SOURCE_STREAM_NAME}` is not loaded on this machine; the link wires when it loads"
)
#: How a reader in another stream is refused `a`'s internal pattern, naming the port and its level.
INTERNAL_PATTERN_REFUSAL = (
    f"port `{PATTERN_PORT}` of node `{PATTERN_NODE}` in stream `{SOURCE_STREAM_NAME}` is "
    f"internal, and another stream or other code on this machine reads only a port that is "
    f"private or public"
)
#: Why a link loaded while `a`'s pattern is internal waits, naming the port and its level.
INTERNAL_PATTERN_AWAITING_REASON = (
    f"{INTERNAL_PATTERN_REFUSAL}; the link wires when the port is exposed private or public"
)

DISPLAY_WINDOW_FRAME_COUNT = re.compile(r"DisplayWindow: stopped \((\d+) frames\)")
TAPPED_BAG_COUNT = 2


def stream_start_log_line(stream_name: str) -> str:
    """The engine's line as a stream's load begins its start."""
    return f"[start] Starting the stream `{stream_name}`"


def stream_stopped_log_line(stream_name: str) -> str:
    """The engine's line once a stream's graceful stop has finished."""
    return f"{ENGINE_GRACEFUL_STOP_LOG_LINE}{stream_name}` stopped"


def run_attached_and_await_its_load(
    tatolabd: TatolabdUnderTest, stream_function: "Callable[..., Any]", stream_name: str
) -> AttachedTatolabRun:
    """`tatolab run <module>:<function> --name <stream_name>` attached, once its stream loaded."""
    attached_run = tatolabd.run_stream_attached(stream_function, stream_name=stream_name)
    loaded = attached_run.await_loaded(timeout=STREAM_RUNNING_TIMEOUT_SECONDS)
    assert loaded["stream_name"] == stream_name, loaded.groups()
    return attached_run


def node_ids_of(local_api: LocalApiClient, stream_name: str) -> "list[str]":
    """Each node id of the stream: minted at each load, so the same ids are the same load."""
    return sorted(node["id"] for node in local_api.graph(stream_name)["nodes"])


def await_more_bags_counted(
    tatolabd: TatolabdUnderTest, counter: str, *, more_reports: int = 2
) -> "dict[str, Any]":
    """Wait for `more_reports` reports past those the counter has made so far; return the last."""
    marker = bags_counted_marker_of(counter)
    reports_so_far = len(tatolabd.marker_payloads(marker))
    return tatolabd.await_marker(
        marker, occurrence=reports_so_far + more_reports, timeout=STREAM_RUNNING_TIMEOUT_SECONDS
    )


def assert_tatolab_succeeded(completed: "subprocess.CompletedProcess[str]") -> str:
    """Require exit 0 from a `tatolab` verb; return its standard output."""
    assert completed.returncode == 0, (
        f"`{' '.join(map(str, completed.args))}` exited {completed.returncode}; "
        f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
    )
    return completed.stdout


def stop_tatolabd_cleanly(tatolabd: TatolabdUnderTest) -> None:
    """A Ctrl-C to `tatolabd`, and its clean exit."""
    tatolabd.interrupt()
    assert tatolabd.await_exit(timeout=TATOLABD_EXIT_TIMEOUT_SECONDS) == 0, tatolabd.recent_stderr()


def refused_naming(run_outcome: StreamRunWithNoVulkanDriverOutcome) -> str:
    """A refused compile's or load's reason; fails if the stream loaded."""
    assert not run_outcome.loaded, (
        f"the stream loaded; it should have been refused:\n"
        f"{run_outcome.tatolabd_stderr_text_during_the_run[-4000:]}"
    )
    assert run_outcome.tatolab_run_exit_status == 1, run_outcome.tatolab_run_stderr_text
    assert run_outcome.refusal is not None, run_outcome.tatolab_run_stderr_text
    return run_outcome.refusal


# --- with started streams ------------------------------------------------------


@pytest.mark.requires_gpu
@pytest.mark.rig_only_capability(reason="a display")
def test_a_link_between_streams_carries_a_native_source_into_python_and_a_python_source_into_a_native_window(
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    tatolabd = start_tatolabd()
    local_api = tatolabd.local_api_client()
    run_attached_and_await_its_load(
        tatolabd, a_pattern_and_a_python_relay_each_exposed_private, SOURCE_STREAM_NAME
    )
    reader_run = run_attached_and_await_its_load(
        tatolabd,
        b_reading_the_pattern_into_a_python_counter_and_the_relay_into_a_window,
        READER_STREAM_NAME,
    )

    native_into_python = local_api.await_link_reading(
        PATTERN_IN_THE_SOURCE_STREAM,
        "wired",
        stream=READER_STREAM_NAME,
        timeout=LINK_WIRED_TIMEOUT_SECONDS,
    )
    python_into_native = local_api.await_link_reading(
        RELAY_IN_THE_SOURCE_STREAM,
        "wired",
        stream=READER_STREAM_NAME,
        timeout=LINK_WIRED_TIMEOUT_SECONDS,
    )
    assert (native_into_python["state"], native_into_python["target"]) == (
        "wired",
        {"node": PATTERN_COUNTER_NODE, "port": COUNTER_PORT},
    ), native_into_python
    assert (python_into_native["state"], python_into_native["target"]) == (
        "wired",
        {"node": WINDOW_NODE, "port": WINDOW_PORT},
    ), python_into_native
    counted = await_more_bags_counted(tatolabd, PATTERN_COUNTER_NODE, more_reports=3)
    assert counted["bags_counted"] >= 20, counted

    relay_in_its_own_stream = {"node": RELAY_NODE, "port": RELAY_PORT}
    assert local_api.links_reading(relay_in_its_own_stream, stream=SOURCE_STREAM_NAME) == [], (
        "the relay's only reader is `b`'s window, so `a` holds no link out of it"
    )
    tapped = json.loads(
        assert_tatolab_succeeded(
            run_tatolab(
                "tap",
                f"{SOURCE_STREAM_NAME}/{RELAY_NODE}/{RELAY_PORT}",
                "--count",
                str(TAPPED_BAG_COUNT),
                working_directory=tmp_path,
            )
        )
    )
    assert tapped["received"] > 0, f"a port read only from another stream is tapped: {tapped}"

    reader_run.interrupt()
    assert reader_run.await_exit(timeout=TATOLABD_EXIT_TIMEOUT_SECONDS) == 0, reader_run.recent_stderr()
    frames_shown = DISPLAY_WINDOW_FRAME_COUNT.search(
        tatolabd.await_stderr_containing("DisplayWindow: stopped")
    )
    assert frames_shown is not None and int(frames_shown.group(1)) > 0, (
        f"`b`'s window showed none of the frames `a`'s Python relay wrote:\n{tatolabd.recent_stderr()}"
    )
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_a_reader_run_before_its_source_awaits_naming_it_then_wires_once_the_source_runs(
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd()
    local_api = tatolabd.local_api_client()
    run_attached_and_await_its_load(
        tatolabd, b_reading_the_pattern_into_a_python_counter, READER_STREAM_NAME
    )

    [awaiting] = local_api.links_reading(PATTERN_IN_THE_SOURCE_STREAM, stream=READER_STREAM_NAME)
    assert (awaiting["state"], awaiting.get("awaiting_remote_reason")) == (
        "awaiting_remote",
        SOURCE_STREAM_NOT_LOADED_REASON,
    ), awaiting

    run_attached_and_await_its_load(tatolabd, a_pattern_exposed_private, SOURCE_STREAM_NAME)

    wired = local_api.await_link_reading(
        PATTERN_IN_THE_SOURCE_STREAM,
        "wired",
        stream=READER_STREAM_NAME,
        timeout=LINK_WIRED_TIMEOUT_SECONDS,
    )
    assert (wired["id"], wired["state"]) == (awaiting["id"], "wired"), wired
    assert "awaiting_remote_reason" not in wired, wired
    counted = await_more_bags_counted(tatolabd, PATTERN_COUNTER_NODE)
    assert counted["bags_counted"] >= 10, counted
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_a_link_to_an_internal_port_awaits_naming_the_port_and_its_level_and_wires_once_it_is_exposed(
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    tatolabd = start_tatolabd()
    local_api = tatolabd.local_api_client()
    run_attached_and_await_its_load(tatolabd, a_pattern_left_internal, SOURCE_STREAM_NAME)
    run_attached_and_await_its_load(
        tatolabd, b_reading_the_pattern_into_a_python_counter, READER_STREAM_NAME
    )

    awaiting_its_port = local_api.await_link_reading(
        PATTERN_IN_THE_SOURCE_STREAM,
        "awaiting_remote",
        stream=READER_STREAM_NAME,
        timeout=LINK_WIRED_TIMEOUT_SECONDS,
    )
    assert (awaiting_its_port["state"], awaiting_its_port.get("awaiting_remote_reason")) == (
        "awaiting_remote",
        INTERNAL_PATTERN_AWAITING_REASON,
    ), awaiting_its_port
    assert "error_reason" not in awaiting_its_port, awaiting_its_port

    refusal = local_api.connect_refusal(
        stream=READER_STREAM_NAME,
        from_stream=SOURCE_STREAM_NAME,
        from_node=PATTERN_NODE,
        from_port=PATTERN_PORT,
        to_node=PATTERN_COUNTER_NODE,
        to_port=COUNTER_PORT,
    )
    assert INTERNAL_PATTERN_REFUSAL in refusal, refusal
    assert [
        link["id"]
        for link in local_api.links_reading(PATTERN_IN_THE_SOURCE_STREAM, stream=READER_STREAM_NAME)
    ] == [awaiting_its_port["id"]], "a live connect to an internal port adds no link"
    reader_node_ids_before_the_expose = node_ids_of(local_api, READER_STREAM_NAME)
    tatolabd.await_stderr_containing(HELPER_PROCESS_STARTED_LOG_LINE_TEXT)
    helper_pids_before_the_expose = helper_process_ids_started_in(tatolabd.stderr_text)
    assert len(helper_pids_before_the_expose) == 1, (
        f"the reader's counter is the one helper:\n{tatolabd.recent_stderr()}"
    )

    exposed = assert_tatolab_succeeded(
        run_tatolab(
            "expose", SOURCE_STREAM_NAME, PATTERN_NODE, PATTERN_PORT, working_directory=tmp_path
        )
    )
    assert exposed == (
        f"{SOURCE_STREAM_NAME}/{PATTERN_NODE}/{PATTERN_PORT} is private (live only: an attached "
        f"stream keeps no record)\n"
    )
    wired = local_api.await_link_reading(
        PATTERN_IN_THE_SOURCE_STREAM,
        "wired",
        stream=READER_STREAM_NAME,
        timeout=LINK_WIRED_TIMEOUT_SECONDS,
    )
    assert (wired["id"], wired["state"]) == (awaiting_its_port["id"], "wired"), wired
    assert "awaiting_remote_reason" not in wired, wired
    counted = await_more_bags_counted(tatolabd, PATTERN_COUNTER_NODE)
    assert counted["bags_counted"] >= 10, counted

    assert node_ids_of(local_api, READER_STREAM_NAME) == reader_node_ids_before_the_expose, (
        "exposing the source port must never reload the reader"
    )
    assert tatolabd.stderr_text.count(stream_start_log_line(READER_STREAM_NAME)) == 1, (
        f"`{READER_STREAM_NAME}` restarted:\n{tatolabd.recent_stderr()}"
    )
    assert counted["pid"] in helper_pids_before_the_expose, (counted, helper_pids_before_the_expose)
    assert helper_process_ids_started_in(tatolabd.stderr_text) == helper_pids_before_the_expose, (
        "exposing the source port must never start a helper process"
    )
    stop_tatolabd_cleanly(tatolabd)


@pytest.mark.requires_gpu
def test_lowering_the_source_to_internal_cuts_the_link_at_once_and_a_link_to_a_still_private_port_keeps_flowing(
    run_tatolab: "Callable[..., subprocess.CompletedProcess[str]]",
    start_tatolabd: "Callable[..., TatolabdUnderTest]",
    tmp_path: Path,
):
    tatolabd = start_tatolabd()
    local_api = tatolabd.local_api_client()
    run_attached_and_await_its_load(tatolabd, a_two_patterns_each_exposed_private, SOURCE_STREAM_NAME)
    run_attached_and_await_its_load(
        tatolabd, b_reading_both_patterns_into_two_python_counters, READER_STREAM_NAME
    )
    to_be_cut, still_private = (
        local_api.await_link_reading(
            source, "wired", stream=READER_STREAM_NAME, timeout=LINK_WIRED_TIMEOUT_SECONDS
        )
        for source in (PATTERN_IN_THE_SOURCE_STREAM, SECOND_PATTERN_IN_THE_SOURCE_STREAM)
    )
    assert (to_be_cut["state"], still_private["state"]) == ("wired", "wired"), (
        to_be_cut,
        still_private,
    )
    cut_counter_pid = await_more_bags_counted(tatolabd, PATTERN_COUNTER_NODE)["pid"]
    still_private_counter_pid = await_more_bags_counted(tatolabd, SECOND_PATTERN_COUNTER_NODE)["pid"]
    node_ids_before_the_cut = {
        stream_name: node_ids_of(local_api, stream_name)
        for stream_name in (SOURCE_STREAM_NAME, READER_STREAM_NAME)
    }

    lowered = assert_tatolab_succeeded(
        run_tatolab(
            "expose",
            SOURCE_STREAM_NAME,
            PATTERN_NODE,
            PATTERN_PORT,
            "--remove",
            working_directory=tmp_path,
        )
    )
    took = local_api.await_link_gone(
        to_be_cut["id"], stream=READER_STREAM_NAME, timeout=LINK_CUT_BOUND_SECONDS
    )

    assert lowered == (
        f"{SOURCE_STREAM_NAME}/{PATTERN_NODE}/{PATTERN_PORT} is internal (live only: an attached "
        f"stream keeps no record)\n"
    )
    assert took < LINK_CUT_BOUND_SECONDS
    assert [
        (link["id"], link["source"], link["state"])
        for link in local_api.graph(READER_STREAM_NAME)["links"]
    ] == [(still_private["id"], SECOND_PATTERN_IN_THE_SOURCE_STREAM, "wired")]
    assert local_api.graph(SOURCE_STREAM_NAME)["exposed"] == [
        {"node": SECOND_PATTERN_NODE, "port": PATTERN_PORT, "level": "private"}
    ]

    await_more_bags_counted(tatolabd, SECOND_PATTERN_COUNTER_NODE)
    cut_counter_reports_once_the_cut_drained = len(
        tatolabd.marker_payloads(bags_counted_marker_of(PATTERN_COUNTER_NODE))
    )
    flowing_on = await_more_bags_counted(tatolabd, SECOND_PATTERN_COUNTER_NODE)
    assert (
        len(tatolabd.marker_payloads(bags_counted_marker_of(PATTERN_COUNTER_NODE)))
        == cut_counter_reports_once_the_cut_drained
    ), "the cut counter kept counting bags after its link was cut"
    assert flowing_on["pid"] == still_private_counter_pid, flowing_on

    os.kill(cut_counter_pid, 0)
    assert {
        stream_name: node_ids_of(local_api, stream_name)
        for stream_name in (SOURCE_STREAM_NAME, READER_STREAM_NAME)
    } == node_ids_before_the_cut, "lowering a level must never reload a stream"
    for stream_name in (SOURCE_STREAM_NAME, READER_STREAM_NAME):
        assert tatolabd.stderr_text.count(stream_start_log_line(stream_name)) == 1, (
            f"`{stream_name}` restarted:\n{tatolabd.recent_stderr()}"
        )
        assert stream_stopped_log_line(stream_name) not in tatolabd.stderr_text
    local_api.await_every_node_running(stream=READER_STREAM_NAME)
    stop_tatolabd_cleanly(tatolabd)


# --- refused before a stream starts ---------------------------------------------


def test_a_link_naming_another_machine_is_refused_by_name(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    refusal = refused_naming(
        run_stream_on_tatolabd_with_no_vulkan_driver(
            b_reading_a_pattern_on_another_machine, stream_name=READER_STREAM_NAME
        )
    )

    assert f"names the machine `{ANOTHER_MACHINE}`" in refusal, refusal
    assert "which is where links between machines arrive" in refusal, refusal


@pytest.mark.parametrize(
    ("stream_function", "refusal_fragments"),
    [
        (
            b_naming_an_address_with_a_dot_chunk,
            (f"the node chunk '.' of the port address {ADDRESS_WITH_A_DOT_CHUNK!r}", "cannot name a node"),
        ),
        (
            b_naming_an_address_of_one_chunk,
            (f"the port address {ADDRESS_OF_ONE_CHUNK!r} has 1 chunk", "an address has 2 to 4"),
        ),
        (
            b_naming_an_address_of_five_chunks,
            (f"the port address {ADDRESS_OF_FIVE_CHUNKS!r} has 5 chunks", "an address has 2 to 4"),
        ),
        (
            b_linking_into_another_streams_input,
            ("A link is only pulled", "no reference names another stream's input"),
        ),
    ],
    ids=["a-dot-chunk", "one-chunk", "five-chunks", "into-another-streams-input"],
)
def test_the_builders_address_refusals_are_the_compiles_refusal(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
    stream_function: "Callable[..., Any]",
    refusal_fragments: "tuple[str, ...]",
):
    refusal = refused_naming(
        run_stream_on_tatolabd_with_no_vulkan_driver(stream_function, stream_name=READER_STREAM_NAME)
    )

    for refusal_fragment in refusal_fragments:
        assert refusal_fragment in refusal, refusal
