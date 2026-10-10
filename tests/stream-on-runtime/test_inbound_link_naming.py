# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A helper-placed Python processor telling two producers apart on one port.

Fan-in is legal — any number of links may enter one input port — and a read
that names the link a bag arrived on is how a processor tells which producer
sent what. This drives that read over real links, from a real child
interpreter: two Python feeders into the one port, and a reporter on its output.

Rig-only, and run nowhere in CI: running a stream brings up a GPU context whose
DMA-BUF pool pre-warm needs a driver that can allocate exportable device
memory. The same contract is gated GPU-free at the engine seam by
`iceoryx2::input::tests::two_inbound_links_hand_a_reader_the_link_each_bag_arrived_on`
and its four neighbours, which CI does run.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest

from conftest import TatolabdUnderTest
from inbound_link_naming_streams import FEEDER_VALUES, two_feeders_into_one_port
from runtime_process_under_test import RuntimeProcessUnderTest

pytestmark = [pytest.mark.requires_gpu]

BAG_TIMEOUT_SECONDS = 30.0


def first_attribution_of_each_value(
    tatolabd: RuntimeProcessUnderTest, wanted_values: "set[str]"
) -> "dict[str, dict[str, Any]]":
    """The first attribution the reporter logged for each of `wanted_values`."""
    first_attributions: "dict[str, dict[str, Any]]" = {}
    occurrence = 0
    while set(first_attributions) != wanted_values:
        occurrence += 1
        attribution = tatolabd.await_marker(
            "ATTRIBUTION", timeout=BAG_TIMEOUT_SECONDS, occurrence=occurrence
        )
        first_attributions.setdefault(attribution["value"], attribution)
    return first_attributions


def test_a_helper_placed_processor_tells_two_producers_apart_on_one_port(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """The read a many-track sink is built on, over real links.

    Each feeder's bags come back named by that feeder's own channel, so a bag
    carries no identity of its own and the sink still knows who sent it.
    """
    tatolabd = start_tatolabd_running_stream(two_feeders_into_one_port)
    attributed = {
        value: attribution["arrived_on"]
        for value, attribution in first_attribution_of_each_value(
            tatolabd, set(FEEDER_VALUES.values())
        ).items()
    }
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert sorted(attributed) == ["from-the-first", "from-the-second"]
    first, second = attributed["from-the-first"], attributed["from-the-second"]
    assert first != second, (
        f"two producers on one port must be distinguishable, got {first!r} for both"
    )
    for value, arrived_on in attributed.items():
        assert arrived_on.endswith("/bags_to_downstream"), (
            f"{value!r} must be named by its producer's source channel — "
            f"'{{source processor id}}/{{source output port}}' — got {arrived_on!r}"
        )


def test_a_sink_learns_its_producers_in_setup_before_any_bag_arrives(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """Links are wired before `setup()` runs, which is how a many-track sink
    knows how many tracks it owes without waiting for a bag on each."""
    tatolabd = start_tatolabd_running_stream(two_feeders_into_one_port)
    attribution = tatolabd.await_marker("ATTRIBUTION", timeout=BAG_TIMEOUT_SECONDS)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    links_at_setup = attribution["links_at_setup"]
    assert len(links_at_setup) == 2, (
        f"both links were wired before setup(), so both must be listed; got {links_at_setup!r}"
    )
    assert attribution["arrived_on"] in links_at_setup, (
        "the link a bag arrives on must be one setup() already listed"
    )
