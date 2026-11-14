# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Probes that read a codec round trip's two links from helper processes.

Each reports what it saw as JSON marker lines on `tatolabd`'s standard
error — one line per frame rather than one array at the end, so a run
that stalls half way still says how far it got.

`EncodedFrameProbe` is the one under test: it reads the encoder's own output
as a decoder reads it — each message one bare access unit, a keyframe
opening a group, the port's stream facts naming the codec.
`EncodedFrameTimestampProbe` reads the same link for its frame-header stamps.
`DecodedVideoFrameProbe` reads the far side of the decoder as Image frames.

The two stamp reports are uncapped, unlike the cast report. Each probe is its
own helper process attaching at its own pace, so a short window on either side
could close before the other opened, and a cross-check needs the two to
overlap.
"""

import dataclasses
import json

from tatolab.stream import Image, Input, Packets, log, node

DECODED_FRAMES_MARKER = "MARKER:DECODED_FRAMES_SEEN "
DECODED_FRAME_STAMP_MARKER = "MARKER:DECODED_FRAME_STAMP "
DECODED_FRAME_STAMPS_COMPLETE_MARKER = "MARKER:DECODED_FRAME_STAMPS_COMPLETE"
ENCODED_FRAME_MARKER = "MARKER:ENCODED_FRAME "
ENCODED_FRAME_STAMP_MARKER = "MARKER:ENCODED_FRAME_STAMP "
ENCODED_FRAMES_COMPLETE_MARKER = "MARKER:ENCODED_FRAMES_COMPLETE"

# Long enough to span a group boundary: the pattern publishes 30 fps and the
# app asks for a 1-second keyframe interval, so a keyframe lands every 30
# frames and this window holds two groups. Without a second group, "a group
# opens at a keyframe" is an assertion about nothing.
ENCODED_FRAMES_REPORTED = 40

# Enough decoded stamps that the cross-check is about the stream, and enough
# that the run cannot end before the decoder has produced its own evidence —
# the encoded probe's window is not a bound on the decoded one.
DECODED_STAMPS_REPORTED = 30

ENCODED_PORT = "encoded_video_from_upstream"


@node.process
class EncodedFrameProbe:
    """Reads the encoder's output as access units and reports each one."""

    def __init__(self) -> None:
        self.frames_admitted = 0
        self.entered_the_stream = False

    encoded_video_from_upstream = Input(Packets, delivery="ordered")

    def process(self, ctx) -> None:
        if self.frames_admitted >= ENCODED_FRAMES_REPORTED:
            return
        message = ctx.inputs.read_message(ENCODED_PORT)
        if message is None:
            return
        # The doctrine every reader of an encoded stream owes it: enter only
        # at a group start. The first message off this link is not necessarily
        # the producer's first — attaching mid-group hands over frames whose
        # keyframe is already gone.
        if not self.entered_the_stream:
            if not message.starts_group:
                return
            self.entered_the_stream = True
        self.frames_admitted += 1
        access_unit = bytes(memoryview(message.payload).cast("B"))
        report = {
            "starts_group": message.starts_group,
            "lost_since_last_read": message.lost_since_last_read,
            "timestamp_ns": message.timestamp_ns,
            "opening_bytes": list(access_unit[:4]),
            "byte_count": len(access_unit),
        }
        if self.frames_admitted == 1:
            upstream_stream_facts = ctx.inputs.upstream_stream_facts(ENCODED_PORT)
            report["stream_facts"] = (
                None if upstream_stream_facts is None else upstream_stream_facts.to_wire()
            )
        log.info(ENCODED_FRAME_MARKER + json.dumps(report))
        if self.frames_admitted == ENCODED_FRAMES_REPORTED:
            log.info(ENCODED_FRAMES_COMPLETE_MARKER)


@node.process
class EncodedFrameTimestampProbe:
    """Reports the frame-header stamp riding each of the encoder's messages.

    No group gate: this decodes nothing, so every message that arrives is one
    whose stamp a decoded frame downstream may carry.
    """

    encoded_video_from_upstream = Input(Packets, delivery="ordered")

    def process(self, ctx) -> None:
        payload, timestamp_ns = ctx.inputs.read_with_timestamp(ENCODED_PORT)
        if payload is None:
            return
        log.info(ENCODED_FRAME_STAMP_MARKER + json.dumps({"timestamp_ns": timestamp_ns}))


@node.process
class DecodedVideoFrameProbe:
    """Reports the decoder's first two Image frames whole, then every frame's stamp.

    The first two carry the whole frame because that is what the extent, colour
    and surface assertions read; the stamps keep coming so the encoded side's
    report has something to overlap with.
    """

    def __init__(self) -> None:
        self.messages_seen = []
        self.stamps_reported = 0

    video_from_upstream = Input(Image, delivery="ordered")

    def process(self, ctx) -> None:
        frame = ctx.inputs.read(self.video_from_upstream)
        if frame is None:
            return
        message = {
            "surface_id": frame.surface_id,
            "width": frame.width,
            "height": frame.height,
            "format": frame.format,
            "colour": dataclasses.asdict(frame.colour),
            "timestamp_ns": frame.timestamp_ns,
        }
        if len(self.messages_seen) < 2:
            self.messages_seen.append(message)
            if len(self.messages_seen) == 2:
                log.info(DECODED_FRAMES_MARKER + json.dumps(self.messages_seen))
        self.stamps_reported += 1
        log.info(
            DECODED_FRAME_STAMP_MARKER
            + json.dumps({"timestamp_ns": message["timestamp_ns"]})
        )
        if self.stamps_reported == DECODED_STAMPS_REPORTED:
            log.info(DECODED_FRAME_STAMPS_COMPLETE_MARKER)
