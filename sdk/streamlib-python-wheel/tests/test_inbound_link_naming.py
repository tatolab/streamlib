# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A helper-placed Python processor telling two producers apart on one port.

Fan-in is already legal — any number of links may enter one input port — but
until now a read handed back bytes and a stamp, so a processor could not tell
which producer sent what. This drives the read that names the link, over real
links, from a real child interpreter.

Rig-only, and run nowhere in CI: every test here calls `runtime.run()`, which
brings up a GPU context whose DMA-BUF pool pre-warm needs a driver that can
allocate exportable device memory. A software rasterizer cannot supply one, so
on a GPU-less runner these do not fail meaningfully — they cannot start. The
same contract is gated GPU-free at the engine seam by
`iceoryx2::input::tests::two_inbound_links_hand_a_reader_the_link_each_bag_arrived_on`
and its four neighbours, which CI does run.
"""

import queue
import threading
from typing import Any, Literal, Optional

import pytest

import streamlib
from inbound_link_naming_processors import ReportsWhichLinkEachBagCameFrom
from streamlib._engine import (
    TestBagCollector,
    TestBagFeeder,
    await_test_harness_bag,
    close_test_harness_channel,
    feed_test_harness_bag,
    open_test_harness_channel,
)

pytestmark = [pytest.mark.requires_gpu]

BAG_TIMEOUT_SECONDS = 30.0
GRAPH_READY_TIMEOUT_SECONDS = 60.0
ENGINE_TEARDOWN_TIMEOUT_SECONDS = 60.0


class TwoFeedersIntoOnePort:
    """Two `TestBagFeeder`s on one input port, and a collector on the output.

    `SingleProcessorTestPipeline` gives every input port exactly one feeder,
    which is the one arrangement that cannot exercise fan-in — so this builds
    the graph by hand. Everything else is the harness's own shape: native
    endpoints in the app process, the processor under test in its own child.
    """

    def __init__(self, feeder_display_names: "list[str]") -> None:
        self._feeder_display_names = feeder_display_names
        self._feed_channels: "dict[str, str]" = {}
        self._collect_channel = ""
        self._runtime: "Optional[streamlib.Runtime]" = None
        self._run_loop: "Optional[threading.Thread]" = None
        self._run_failure: "queue.Queue[BaseException]" = queue.Queue()

    def __enter__(self) -> "TwoFeedersIntoOnePort":
        try:
            self._build_and_start()
        except BaseException:
            self.__exit__()
            raise
        return self

    def _build_and_start(self) -> None:
        runtime = streamlib.Runtime()
        self._runtime = runtime
        sink = runtime.add(ReportsWhichLinkEachBagCameFrom)

        for display_name in self._feeder_display_names:
            channel = f"inbound-link-naming-{display_name}"
            open_test_harness_channel(channel)
            self._feed_channels[display_name] = channel
            feeder = runtime.add(
                TestBagFeeder,
                config={"channel": channel},
                display_name=display_name,
            )
            runtime.connect(feeder.output("bags_to_downstream"), sink.input("tracks"))

        self._collect_channel = "inbound-link-naming-attributions"
        open_test_harness_channel(self._collect_channel)
        collector = runtime.add(
            TestBagCollector, config={"channel": self._collect_channel}
        )
        runtime.connect(
            sink.output("attributions_to_downstream"),
            collector.input("bags_from_upstream"),
        )

        self._run_loop = threading.Thread(
            target=self._run_until_shut_down, name="inbound-link-naming", daemon=True
        )
        self._run_loop.start()
        runtime.wait_until_every_processor_is_running(
            timeout=GRAPH_READY_TIMEOUT_SECONDS
        )

    def _run_until_shut_down(self) -> None:
        try:
            assert self._runtime is not None
            self._runtime.run()
        except BaseException as run_failure:  # noqa: BLE001 — re-raised in __exit__
            self._run_failure.put(run_failure)

    def feed(self, feeder_display_name: str, bag: "dict[str, Any]") -> None:
        feed_test_harness_bag(self._feed_channels[feeder_display_name], bag)

    def await_bags(self, count: int) -> "list[Any]":
        collected: "list[Any]" = []
        for _ in range(count):
            bag = await_test_harness_bag(self._collect_channel, BAG_TIMEOUT_SECONDS)
            assert bag is not None, (
                f"the sink produced only {len(collected)} of {count} attributions "
                f"within {BAG_TIMEOUT_SECONDS}s"
            )
            collected.append(bag)
        return collected

    def __exit__(self, *_exception_details: Any) -> "Literal[False]":
        if self._runtime is not None:
            self._runtime.shutdown()
        if self._run_loop is not None:
            self._run_loop.join(timeout=ENGINE_TEARDOWN_TIMEOUT_SECONDS)
            assert not self._run_loop.is_alive(), (
                f"the engine did not tear down within "
                f"{ENGINE_TEARDOWN_TIMEOUT_SECONDS}s"
            )
        for channel in self._feed_channels.values():
            close_test_harness_channel(channel)
        if self._collect_channel:
            close_test_harness_channel(self._collect_channel)
        try:
            raise self._run_failure.get_nowait()
        except queue.Empty:
            pass
        return False


def test_a_helper_placed_processor_tells_two_producers_apart_on_one_port():
    """The read a many-track sink is built on, over real links.

    Each feeder's bags come back named by that feeder's own channel, so a bag
    carries no identity of its own and the sink still knows who sent it.
    """
    with TwoFeedersIntoOnePort(["firstfeeder", "secondfeeder"]) as pipeline:
        pipeline.feed("firstfeeder", {"value": "from-the-first"})
        pipeline.feed("secondfeeder", {"value": "from-the-second"})

        attributed = {
            bag["value"]: bag["arrived_on"] for bag in pipeline.await_bags(2)
        }

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


def test_a_sink_learns_its_producers_in_setup_before_any_bag_arrives():
    """Links are wired before `setup()` runs, which is how a many-track sink
    knows how many tracks it owes without waiting for a bag on each."""
    with TwoFeedersIntoOnePort(["firstfeeder", "secondfeeder"]) as pipeline:
        pipeline.feed("firstfeeder", {"value": "any"})
        [attribution] = pipeline.await_bags(1)

    links_at_setup = attribution["links_at_setup"]
    assert len(links_at_setup) == 2, (
        f"both links were wired before setup(), so both must be listed; "
        f"got {links_at_setup!r}"
    )
    assert attribution["arrived_on"] in links_at_setup, (
        "the link a bag arrives on must be one setup() already listed"
    )
