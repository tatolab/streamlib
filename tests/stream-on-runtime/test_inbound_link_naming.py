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

from inbound_link_naming_processors import (
    FeedsOneValueSource,
    ReportsEachAttributionSink,
    ReportsWhichLinkEachBagCameFrom,
)
from runtime_process_under_test import RuntimeProcessUnderTest
from tatolab.stream import StreamBuilder, stream

pytestmark = [pytest.mark.requires_gpu]

BAG_TIMEOUT_SECONDS = 30.0

FEEDER_VALUES = {"firstfeeder": "from-the-first", "secondfeeder": "from-the-second"}


@stream
def two_feeders_into_one_port(stream_builder: StreamBuilder) -> None:
    """Both feeders linked into the one `tracks` port, a reporter on its output."""
    sink = stream_builder.add(ReportsWhichLinkEachBagCameFrom)
    for feeder_name, value in FEEDER_VALUES.items():
        feeder = stream_builder.add(FeedsOneValueSource, name=feeder_name, config={"value": value})
        stream_builder.connect(feeder.output("bags_to_downstream"), sink.input("tracks"))
    reporter = stream_builder.add(ReportsEachAttributionSink)
    stream_builder.connect(
        sink.output("attributions_to_downstream"), reporter.input("attributions_from_upstream")
    )


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
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
):
    """The read a many-track sink is built on, over real links.

    Each feeder's bags come back named by that feeder's own channel, so a bag
    carries no identity of its own and the sink still knows who sent it.
    """
    tatolabd = start_tatolabd(two_feeders_into_one_port)
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
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
):
    """Links are wired before `setup()` runs, which is how a many-track sink
    knows how many tracks it owes without waiting for a bag on each."""
    tatolabd = start_tatolabd(two_feeders_into_one_port)
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
