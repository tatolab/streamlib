# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run a Python frame producer in its real placement.

Run as a real `python app.py`: the producer executes in a helper process, and
what it observed reaches this app — and the test driving it — over the same log
forwarding every child's records ride.
"""

import sys

import streamlib
from texture_ring_producer_probes import (
    FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD,
    MINIMUM_INTERVAL_BETWEEN_HELD_FRAME_PUBLISHES_NS,
    RING_DEPTH,
    ClaimedFrameHoldingSink,
    PublishedFrameIdRecordingSink,
    PublishedFramePixelReadingSink,
    TextureRingPublishingVideoSource,
    UnclaimedFrameHoldingSink,
)


def scenario_ring_rotation() -> None:
    """One frame more than the ring is deep, so the last one wraps onto the
    slot the first published from.

    The sink is here because an output port with no link refuses the write —
    a source alone is not a graph. It resolves nothing: frame 0's id is
    recycled by the wrap, and the sink's pixels are the other scenario's job.
    """
    runtime = streamlib.Runtime()
    source = runtime.add(
        TextureRingPublishingVideoSource,
        config={"frames_to_publish": RING_DEPTH + 1},
    )
    sink = runtime.add(PublishedFrameIdRecordingSink)
    runtime.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def scenario_published_frames_reach_a_downstream_consumer() -> None:
    """Python source → Python sink, the direction nothing else covers.

    Exactly `RING_DEPTH` frames, so no slot is republished while the consumer
    may still be reading it — the ring's documented reuse would otherwise make
    the pixel assertion a race rather than a contract.
    """
    runtime = streamlib.Runtime()
    source = runtime.add(
        TextureRingPublishingVideoSource,
        config={"frames_to_publish": RING_DEPTH},
    )
    sink = runtime.add(PublishedFramePixelReadingSink)
    runtime.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def _run_a_source_holding_its_first_frame_downstream(holding_sink_class) -> None:
    runtime = streamlib.Runtime()
    source = runtime.add(
        TextureRingPublishingVideoSource,
        config={
            "frames_to_publish": FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD + 1,
            "minimum_interval_between_publishes_ns": (
                MINIMUM_INTERVAL_BETWEEN_HELD_FRAME_PUBLISHES_NS
            ),
        },
    )
    sink = runtime.add(holding_sink_class)
    runtime.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def scenario_a_claimed_frame_holds_still() -> None:
    """The consumer claims the first frame and holds it while the source
    publishes several times the ring's depth past it."""
    _run_a_source_holding_its_first_frame_downstream(ClaimedFrameHoldingSink)


def scenario_an_unclaimed_frame_is_recycled() -> None:
    """The same schedule with no claim: the first frame's slot is republished."""
    _run_a_source_holding_its_first_frame_downstream(UnclaimedFrameHoldingSink)


SCENARIOS = {
    "a_claimed_frame_holds_still": scenario_a_claimed_frame_holds_still,
    "an_unclaimed_frame_is_recycled": scenario_an_unclaimed_frame_is_recycled,
    "ring_rotation": scenario_ring_rotation,
    "published_frames_reach_a_downstream_consumer": (
        scenario_published_frames_reach_a_downstream_consumer
    ),
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]]()
