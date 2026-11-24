# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A connection on a running stream: a hub's connection output wired into a spoke's connection
field reaches the spoke as the hub, straight and through Set Variable and Get Variable."""

import pytest

import connection_wire_streams

pytestmark = pytest.mark.requires_gpu


def test_a_hub_wired_into_a_spoke_reaches_it_straight_and_through_a_variable(
    start_tatolabd_running_stream,
):
    """Fail-without-fix: a connection holds no stored value, so the runtime hands the spoke's
    field nothing and it stays `None`, and neither marker is logged."""
    tatolabd = start_tatolabd_running_stream(
        connection_wire_streams.a_hub_wired_into_one_spoke_and_through_a_variable_into_another
    )
    reads = tatolabd.await_every_marker("CONNECTION_REACHED_WIRED", "CONNECTION_REACHED_VARIABLE")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert reads == {
        "CONNECTION_REACHED_WIRED": {"hub": connection_wire_streams.HUB_NAME},
        "CONNECTION_REACHED_VARIABLE": {"hub": connection_wire_streams.HUB_NAME},
    }, reads
