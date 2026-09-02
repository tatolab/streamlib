# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes that read a codec round trip's two links from helper processes.

Each reports what it saw as JSON marker lines riding the child→parent log
forwarding — one line per frame rather than one array at the end, so a run
that stalls half way still says how far it got.

`EncodedFrameProbe` is the one under test: it reads the encoder's own output
with `into=EncodedVideoFrame`, which is the whole claim the cast makes.
`EncodedFrameTimestampProbe` reads the same link for its frame-header stamps,
which `read_with_timestamp` is the only read that yields and which takes no
`into=`. `DecodedVideoFrameProbe` reads the far side of the decoder, where the
published output is an ordinary video-frame bag.

The two stamp reports are uncapped, unlike the cast report. Each probe is its
own helper process attaching at its own pace, so a short window on either side
could close before the other opened, and a cross-check needs the two to
overlap.
"""

import json

from streamlib import EncodedVideoFrame, input, log, processor

DECODED_FRAMES_MARKER = "MARKER:DECODED_FRAMES_SEEN "
DECODED_FRAME_STAMP_MARKER = "MARKER:DECODED_FRAME_STAMP "
DECODED_FRAME_STAMPS_COMPLETE_MARKER = "MARKER:DECODED_FRAME_STAMPS_COMPLETE"
ENCODED_FRAME_MARKER = "MARKER:ENCODED_FRAME "
ENCODED_FRAME_STAMP_MARKER = "MARKER:ENCODED_FRAME_STAMP "
ENCODED_FRAMES_COMPLETE_MARKER = "MARKER:ENCODED_FRAMES_COMPLETE"

# Long enough to span a group boundary: the pattern publishes 30 fps and the
# app asks for a 1-second keyframe interval, so a sync point lands every 30
# frames and this window holds two groups. Without a second group, "the group
# index steps only at a sync point" is an assertion about nothing.
ENCODED_FRAMES_REPORTED = 40

# Enough decoded stamps that the cross-check is about the stream, and enough
# that the run cannot end before the decoder has produced its own evidence —
# the encoded probe's window is not a bound on the decoded one.
DECODED_STAMPS_REPORTED = 30

ENCODED_PORT = "encoded_video_from_upstream"


@processor
class EncodedFrameProbe:
    """Casts the encoder's output and reports each frame's wire fields."""

    def __init__(self) -> None:
        self.frames_admitted = 0
        self.entered_the_stream = False

    @input(delivery_profile="ordered")
    def encoded_video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        if self.frames_admitted >= ENCODED_FRAMES_REPORTED:
            return
        frame = ctx.inputs.read(ENCODED_PORT, into=EncodedVideoFrame)
        if frame is None:
            return
        # The doctrine every reader of an encoded stream owes it: enter only
        # at a sync point. The first bag off this link is not necessarily the
        # producer's first — attaching mid-group hands over frames whose sync
        # point is already gone.
        if not self.entered_the_stream:
            if not frame.is_sync_point:
                return
            self.entered_the_stream = True
        self.frames_admitted += 1
        log.info(
            ENCODED_FRAME_MARKER
            + json.dumps(
                {
                    "codec": frame.codec,
                    "is_sync_point": frame.is_sync_point,
                    "group_index": frame.group_index,
                    "sequence_index": frame.sequence_index,
                    "width": frame.width,
                    "height": frame.height,
                    "opening_bytes": list(frame.annex_b_access_unit_bytes[:4]),
                    "byte_count": len(frame.annex_b_access_unit_bytes),
                    "carries_color": frame.color is not None,
                }
            )
        )
        if self.frames_admitted == ENCODED_FRAMES_REPORTED:
            log.info(ENCODED_FRAMES_COMPLETE_MARKER)


@processor
class EncodedFrameTimestampProbe:
    """Reports the frame-header stamp riding each of the encoder's bags.

    No sync-point gate: this decodes nothing, so every bag that arrives is one
    whose stamp a decoded frame downstream may carry.
    """

    @input(delivery_profile="ordered")
    def encoded_video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        bag, timestamp_ns = ctx.inputs.read_with_timestamp(ENCODED_PORT)
        if bag is None:
            return
        frame = EncodedVideoFrame.from_bag(bag)
        log.info(
            ENCODED_FRAME_STAMP_MARKER
            + json.dumps(
                {"sequence_index": frame.sequence_index, "timestamp_ns": timestamp_ns}
            )
        )


@processor
class DecodedVideoFrameProbe:
    """Reports the decoder's first two bags whole, then every frame's stamp.

    The first two carry the whole bag because that is what the extent, colour
    and surface assertions read; the stamps keep coming so the encoded side's
    report has something to overlap with.
    """

    def __init__(self) -> None:
        self.bags_seen = []
        self.stamps_reported = 0

    @input(delivery_profile="ordered")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is None:
            return
        if len(self.bags_seen) < 2:
            self.bags_seen.append(dict(bag))
            if len(self.bags_seen) == 2:
                log.info(DECODED_FRAMES_MARKER + json.dumps(self.bags_seen))
        self.stamps_reported += 1
        log.info(
            DECODED_FRAME_STAMP_MARKER
            + json.dumps({"timestamp_ns": bag["timestamp_ns"]})
        )
        if self.stamps_reported == DECODED_STAMPS_REPORTED:
            log.info(DECODED_FRAME_STAMPS_COMPLETE_MARKER)
