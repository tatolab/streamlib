# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Gateway nodes carry a port off its stream and back, through a relay the test starts, and a
track renamed or a relay moved to while the stream plays carries it on with no node rebuilt."""

from __future__ import annotations

import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from conftest import TatolabdUnderTest
from fixture_moq_relay import (  # noqa: F401  (`fixture_moq_relay_url` is a pytest fixture)
    a_fixture_relay_serving,
    fixture_moq_relay_url,
)
from helper_process_observation import helper_process_ids_started_in
from local_api_client import LocalApiClient
from moq_gateway_nodes import RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER
from moq_gateway_streams import (
    COUNT_TRACK,
    FIXTURE_RELAY_URL_ENVIRONMENT_VARIABLE,
    RENAMED_COUNT_TRACK,
    counting_records_sent_to_the_relay_and_read_back,
)

pytestmark = pytest.mark.requires_gpu

#: Records read back before the test trusts the order: well past the first group.
RECORDS_READ_BACK = 20

#: A gateway's dial, the broadcast's announce and the receive's subscribe come first.
FIRST_RECORD_BUDGET_SECONDS = 60.0


def test_a_records_port_sent_through_a_gateway_reads_back_in_order_at_the_relay(
    start_tatolabd_running_stream: Callable[..., TatolabdUnderTest],
    fixture_moq_relay_url: str,  # noqa: F811
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(FIXTURE_RELAY_URL_ENVIRONMENT_VARIABLE, fixture_moq_relay_url)
    tatolabd = start_tatolabd_running_stream(counting_records_sent_to_the_relay_and_read_back)
    tatolabd.stream_runs[-1].await_loaded()

    first_count = tatolabd.await_marker(
        RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER, timeout=FIRST_RECORD_BUDGET_SECONDS
    )["count"]
    counts_read_back = [first_count] + [
        tatolabd.await_marker(RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER, occurrence=occurrence)[
            "count"
        ]
        for occurrence in range(2, RECORDS_READ_BACK + 1)
    ]

    assert counts_read_back == sorted(counts_read_back), counts_read_back
    assert len(set(counts_read_back)) == RECORDS_READ_BACK, counts_read_back
    tatolabd.interrupt()
    tatolabd.await_clean_exit()


#: How long records still in flight to the receive may take to drain after its track goes away.
RECORDS_IN_FLIGHT_DRAIN_SECONDS = 3.0

#: How long the test watches for a record that should not arrive: the source writes one every
#: 20 ms, so a track still carried shows dozens in it.
QUIET_WATCH_SECONDS = 3.0

#: Records read back after an edit before the test trusts that the stream carries on.
RECORDS_READ_AFTER_AN_EDIT = 5

#: A new relay session's dial, announce and subscribe come first after an edit.
RECORDS_AFTER_AN_EDIT_BUDGET_SECONDS = 60.0


def the_records_read_back(tatolabd: TatolabdUnderTest) -> int:
    """How many records the sink has reported so far."""
    return len(tatolabd.marker_payloads(RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER))


def await_more_records(tatolabd: TatolabdUnderTest, read_before: int) -> "list[int]":
    """Wait for `RECORDS_READ_AFTER_AN_EDIT` more records past the `read_before` the sink had
    reported, and answer their counts."""
    return [
        tatolabd.await_marker(
            RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER,
            occurrence=read_before + offset,
            timeout=RECORDS_AFTER_AN_EDIT_BUDGET_SECONDS,
        )["count"]
        for offset in range(1, RECORDS_READ_AFTER_AN_EDIT + 1)
    ]


def set_input_values_while_playing(
    local_api: LocalApiClient, stream_id: str, sets: "list[tuple[str, str, Any]]"
) -> None:
    """Set each `(node name, input, value)` of `sets` in one `transact` on the playing stream."""
    node_ids = {node["name"]: node["id"] for node in local_api.graph(stream=stream_id)["nodes"]}
    local_api.call_tool(
        "transact",
        {
            "operations": [
                {
                    "operation": "set_node_input_value",
                    "stream": stream_id,
                    "node": node_ids[node_name],
                    "input": input_name,
                    "value": value,
                }
                for node_name, input_name, value in sets
            ]
        },
    )


def assert_the_stream_still_runs_on_its_first_helpers(
    tatolabd: TatolabdUnderTest, local_api: LocalApiClient, stream_id: str, helpers: "list[int]"
) -> None:
    """Fail when a helper started since `helpers`, which a node rebuilt starts, or the stream no
    longer runs healthy."""
    assert helper_process_ids_started_in(tatolabd.stderr_text) == helpers, (
        f"no node was rebuilt, so no helper started:\n{tatolabd.recent_stderr()}"
    )
    (listed,) = [listed for listed in local_api.list_streams() if listed["stream"] == stream_id]
    assert listed["state"] == "running", listed
    assert listed["condition"]["health"] == "healthy", listed


def test_a_track_renamed_while_playing_carries_the_records_on_the_new_track_restarting_nothing(
    start_tatolabd_running_stream: Callable[..., TatolabdUnderTest],
    fixture_moq_relay_url: str,  # noqa: F811
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Mental-revert: a `MoqTrack` whose `track` is read only when it is set up is rebuilt, in a
    new helper, or goes on writing the old track, so records never stop when it is renamed; a
    receive whose new track is not read never reads the renamed one."""
    monkeypatch.setenv(FIXTURE_RELAY_URL_ENVIRONMENT_VARIABLE, fixture_moq_relay_url)
    tatolabd = start_tatolabd_running_stream(counting_records_sent_to_the_relay_and_read_back)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    tatolabd.await_marker(RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER, timeout=FIRST_RECORD_BUDGET_SECONDS)
    local_api = tatolabd.local_api_client()
    helpers = helper_process_ids_started_in(tatolabd.stderr_text)

    set_input_values_while_playing(local_api, stream_id, [("count_track", "track", RENAMED_COUNT_TRACK)])
    time.sleep(RECORDS_IN_FLIGHT_DRAIN_SECONDS)
    read_once_drained = the_records_read_back(tatolabd)
    time.sleep(QUIET_WATCH_SECONDS)
    assert the_records_read_back(tatolabd) == read_once_drained, (
        f"the renamed track no longer carries records on `{COUNT_TRACK}`"
    )

    set_input_values_while_playing(local_api, stream_id, [("receive", "track", RENAMED_COUNT_TRACK)])
    counts_on_the_renamed_track = await_more_records(tatolabd, read_once_drained)

    assert counts_on_the_renamed_track == sorted(counts_on_the_renamed_track), counts_on_the_renamed_track
    assert_the_stream_still_runs_on_its_first_helpers(tatolabd, local_api, stream_id, helpers)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()


def test_gateways_moved_to_another_relay_while_playing_carry_the_records_through_it_restarting_nothing(
    start_tatolabd_running_stream: Callable[..., TatolabdUnderTest],
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Mental-revert: a gateway whose `relay_url` is read only when it is set up is rebuilt, in a
    new helper, or stays on the first relay, so stopping that relay stops the records."""
    with (
        a_fixture_relay_serving(tmp_path / "first-moq-relay") as first_relay,
        a_fixture_relay_serving(tmp_path / "second-moq-relay") as second_relay,
    ):
        monkeypatch.setenv(FIXTURE_RELAY_URL_ENVIRONMENT_VARIABLE, first_relay.url)
        tatolabd = start_tatolabd_running_stream(counting_records_sent_to_the_relay_and_read_back)
        stream_id = tatolabd.await_the_latest_stream_loaded()
        tatolabd.await_marker(
            RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER, timeout=FIRST_RECORD_BUDGET_SECONDS
        )
        local_api = tatolabd.local_api_client()
        helpers = helper_process_ids_started_in(tatolabd.stderr_text)

        set_input_values_while_playing(
            local_api,
            stream_id,
            [
                ("sending_gateway", "relay_url", second_relay.url),
                ("receiving_gateway", "relay_url", second_relay.url),
            ],
        )
        first_relay.stop()
        counts_through_the_second_relay = await_more_records(
            tatolabd, the_records_read_back(tatolabd)
        )

        assert counts_through_the_second_relay == sorted(counts_through_the_second_relay)
        assert_the_stream_still_runs_on_its_first_helpers(tatolabd, local_api, stream_id, helpers)
        tatolabd.interrupt()
        tatolabd.await_clean_exit()
