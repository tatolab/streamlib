# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""Gateway nodes carry a port off its stream and back, through a relay the test starts."""

from __future__ import annotations

from collections.abc import Callable

import pytest

from conftest import TatolabdUnderTest
from fixture_moq_relay import fixture_moq_relay_url  # noqa: F401  (a pytest fixture)
from moq_gateway_processors import RECORD_RECEIVED_THROUGH_THE_RELAY_MARKER
from moq_gateway_streams import (
    FIXTURE_RELAY_URL_ENVIRONMENT_VARIABLE,
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
