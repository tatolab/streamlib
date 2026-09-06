# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The data track the MoQ live proof carries beside its video and its audio.

A source that publishes one small bag per tick and a sink that consumes them
on the far side of the relay. Between the two sit the publisher, a real
draft-16 relay and the subscriber, and what the run has to show is that the
bag came back byte-for-byte and stamped as it was sent.

Both halves of that check are in the bag itself, so the verdict needs no state
carried across the network. `blob` is derived from `frame`, so a checker can
recompute it and a stream replaying one bag forever does not pass. `stamp_ns`
is the instant the source wrote, written into the bag *and* passed as the
write's own stamp — the far end reads them from two different places, the
frame header and the payload, and they are equal only if the producer's stamp
crossed the relay untouched.

A module of its own rather than a class in the node script: a processor is
named by its `__module__` and `__qualname__`, and one declared in `__main__`
would have its helper process re-import the entry file to find it.
"""

from streamlib import (
    RuntimeContextLimitedAccess,
    input,
    log,
    monotonic_now_ns,
    output,
    processor,
)

TELEMETRY_OUTPUT_PORT = "telemetry"
DATA_BAGS_INPUT_PORT = "data_bags"

#: A tick under a frame interval at any rate the camera runs, so the data
#: track is never the thing the run is waiting on.
TELEMETRY_INTERVAL_MS = 100

#: How many bags apart the sink says what it has read. Short, because the run
#: is a couple of minutes and a line at the media cadence would arrive twice.
BAGS_BETWEEN_SINK_REPORTS = 20


def telemetry_blob_for_frame(frame: int) -> bytes:
    """The `bytes` value a given frame's bag carries.

    Derived rather than constant so a far end that delivered one bag over and
    over would be caught, and spelled with the byte values a round trip
    through text would not survive.
    """
    return (frame.to_bytes(4, "little") + b"\x00\xff\x10\x7f") * 2


@processor(
    execution="continuous",
    interval_ms=TELEMETRY_INTERVAL_MS,
    description="Publishes one telemetry bag per tick onto the broadcast's data track",
)
class TelemetryBagSource:
    """A frame counter, the instant it was written, and a blob that counter
    determines."""

    def __init__(self) -> None:
        self._frame = 0

    @output()
    def telemetry(self) -> None:
        """The bags the publisher classifies as data — no `bitstream` key."""

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # The same instant in both places: as the write's stamp, which the
        # publisher reads off the link and the envelope carries, and inside
        # the bag, which crosses as payload. Equal at the far end is what says
        # nothing restamped on the way.
        stamp_ns = monotonic_now_ns()
        ctx.outputs.write(
            TELEMETRY_OUTPUT_PORT,
            {
                "frame": self._frame,
                "stamp_ns": stamp_ns,
                "blob": telemetry_blob_for_frame(self._frame),
            },
            timestamp_ns=stamp_ns,
        )
        self._frame += 1


@processor(description="Reads the data track's bags on the far side of the relay")
class TelemetryBagSink:
    """The data track's consumer for the whole run.

    The verdict is the driving script's, taken off the channel — but a track
    with no reader is a track no consumer ever proved reachable, which is the
    same shape as the window and the speaker the media arms give their
    decoders.
    """

    def __init__(self) -> None:
        self._bags_read = 0

    @input(delivery_profile="ordered")
    def data_bags(self) -> None:
        """The subscriber's `data_bags`, each bag as its producer wrote it."""

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read(DATA_BAGS_INPUT_PORT)
        if bag is None:
            return
        self._bags_read += 1
        if self._bags_read == 1 or self._bags_read % BAGS_BETWEEN_SINK_REPORTS == 0:
            log.info(
                f"TelemetryBagSink: bags_read={self._bags_read}, "
                f"latest frame={bag.get('frame')}"
            )
