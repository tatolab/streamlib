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
    RING_DEPTH,
    PublishedFramePixelReadingSink,
    TextureRingPublishingVideoSource,
)


def scenario_ring_rotation() -> None:
    """One frame more than the ring is deep, so the last one wraps onto the
    slot the first published from.

    The sink is here because an output port with no link refuses the write —
    a source alone is not a graph. What the rotation test reads is the
    producer's own report; the sink's pixels are the other scenario's job.
    """
    runtime = streamlib.Runtime()
    source = runtime.add(
        TextureRingPublishingVideoSource,
        config={"frames_to_publish": RING_DEPTH + 1},
    )
    sink = runtime.add(PublishedFramePixelReadingSink)
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


SCENARIOS = {
    "ring_rotation": scenario_ring_rotation,
    "published_frames_reach_a_downstream_consumer": (
        scenario_published_frames_reach_a_downstream_consumer
    ),
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]]()
