# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run a Python frame producer in its real placement.

Run as its own `python <script>.py` process: the producer executes in a helper
process, and what it observed reaches this app — and the test driving it — over
the same log forwarding every child's records ride.
"""

import sys

import tatolab.runtime
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream
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
def ring_rotation(stream_builder: StreamBuilder) -> None:
    """One frame more than the ring is deep, so the last one wraps onto the
    slot the first published from.

    The sink is here because an output port with no link refuses the write —
    a source alone is not a graph. It resolves nothing: frame 0's id is
    recycled by the wrap, and the sink's pixels are the other scenario's job.
    """
    source = stream_builder.add(
        TextureRingPublishingVideoSource,
        config={"frames_to_publish": RING_DEPTH + 1},
    )
    sink = stream_builder.add(PublishedFrameIdRecordingSink)
    stream_builder.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )


@stream
def published_frames_reach_a_downstream_consumer(stream_builder: StreamBuilder) -> None:
    """Python source → Python sink, the direction nothing else covers.

    Exactly `RING_DEPTH` frames, so no slot is republished while the consumer
    may still be reading it — the ring's documented reuse would otherwise make
    the pixel assertion a race rather than a contract.
    """
    source = stream_builder.add(
        TextureRingPublishingVideoSource,
        config={"frames_to_publish": RING_DEPTH},
    )
    sink = stream_builder.add(PublishedFramePixelReadingSink)
    stream_builder.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )


def _add_a_source_holding_its_first_frame_downstream(
    stream_builder: StreamBuilder, holding_sink_class: type
) -> None:
    source = stream_builder.add(
        TextureRingPublishingVideoSource,
        config={
            "frames_to_publish": FRAMES_PUBLISHED_WHILE_THE_FIRST_IS_HELD + 1,
            "minimum_interval_between_publishes_ns": (
                MINIMUM_INTERVAL_BETWEEN_HELD_FRAME_PUBLISHES_NS
            ),
        },
    )
    sink = stream_builder.add(holding_sink_class)
    stream_builder.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )


@stream
def a_claimed_frame_holds_still(stream_builder: StreamBuilder) -> None:
    """The consumer claims the first frame and holds it while the source
    publishes several times the ring's depth past it."""
    _add_a_source_holding_its_first_frame_downstream(stream_builder, ClaimedFrameHoldingSink)


@stream
def an_unclaimed_frame_is_recycled(stream_builder: StreamBuilder) -> None:
    """The same schedule with no claim: the first frame's slot is republished."""
    _add_a_source_holding_its_first_frame_downstream(stream_builder, UnclaimedFrameHoldingSink)


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
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
