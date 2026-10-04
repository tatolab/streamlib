# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run a Python frame producer in its real placement.

Run as a real `python app.py`: the producer executes in a helper process, and
what it observed reaches this app — and the test driving it — over the same log
forwarding every child's records ride.
"""

import sys

import streamlib
from streamlib import Stream, compile_stream_to_graph, stream
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


@stream
def ring_rotation(stream: Stream) -> None:
    """One frame more than the ring is deep, so the last one wraps onto the
    slot the first published from.

    The sink is here because an output port with no link refuses the write —
    a source alone is not a graph. It resolves nothing: frame 0's id is
    recycled by the wrap, and the sink's pixels are the other scenario's job.
    """
    source = stream.add(
        TextureRingPublishingVideoSource,
        config={"frames_to_publish": RING_DEPTH + 1},
    )
    sink = stream.add(PublishedFrameIdRecordingSink)
    stream.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )


@stream
def published_frames_reach_a_downstream_consumer(stream: Stream) -> None:
    """Python source → Python sink, the direction nothing else covers.

    Exactly `RING_DEPTH` frames, so no slot is republished while the consumer
    may still be reading it — the ring's documented reuse would otherwise make
    the pixel assertion a race rather than a contract.
    """
    source = stream.add(
        TextureRingPublishingVideoSource,
        config={"frames_to_publish": RING_DEPTH},
    )
    sink = stream.add(PublishedFramePixelReadingSink)
    stream.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )


def _add_a_source_holding_its_first_frame_downstream(
    stream: Stream, holding_sink_class: type
) -> None:
    source = stream.add(
        TextureRingPublishingVideoSource,
        config={
            "frames_to_publish": FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD + 1,
            "minimum_interval_between_publishes_ns": (
                MINIMUM_INTERVAL_BETWEEN_HELD_FRAME_PUBLISHES_NS
            ),
        },
    )
    sink = stream.add(holding_sink_class)
    stream.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )


@stream
def a_claimed_frame_holds_still(stream: Stream) -> None:
    """The consumer claims the first frame and holds it while the source
    publishes several times the ring's depth past it."""
    _add_a_source_holding_its_first_frame_downstream(stream, ClaimedFrameHoldingSink)


@stream
def an_unclaimed_frame_is_recycled(stream: Stream) -> None:
    """The same schedule with no claim: the first frame's slot is republished."""
    _add_a_source_holding_its_first_frame_downstream(stream, UnclaimedFrameHoldingSink)


STREAM_BY_SCENARIO = {
    "a_claimed_frame_holds_still": a_claimed_frame_holds_still,
    "an_unclaimed_frame_is_recycled": an_unclaimed_frame_is_recycled,
    "ring_rotation": ring_rotation,
    "published_frames_reach_a_downstream_consumer": (
        published_frames_reach_a_downstream_consumer
    ),
}


if __name__ == "__main__":
    graph = compile_stream_to_graph(STREAM_BY_SCENARIO[sys.argv[1]])
    runtime = streamlib.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
