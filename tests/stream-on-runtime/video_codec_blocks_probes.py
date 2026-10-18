# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes that read a codec round trip's two links from helper processes.

Each reports what it saw as JSON marker lines on `tatolabd`'s standard
error — one line per frame rather than one array at the end, so a run
that stalls half way still says how far it got.

`EncodedFrameProbe` is the one under test: it reads the encoder's own output
the way a moq-dev player does — each message a media frame, a keyframe
opening a group, the port's catalog entry naming the codec.
`EncodedFrameTimestampProbe` reads the same link for its frame-header stamps.
`DecodedVideoFrameProbe` reads the far side of the decoder, where the
published output is an ordinary video-frame message.

The two stamp reports are uncapped, unlike the cast report. Each probe is its
own helper process attaching at its own pace, so a short window on either side
could close before the other opened, and a cross-check needs the two to
overlap.
"""

import json

from hang_media_frame_reading import split_hang_legacy_container_media_frame
from tatolab.stream import log, node
from tatolab.stream.msgpack import unpack

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


@node
class EncodedFrameProbe:
    """Reads the encoder's output as media frames and reports each one."""

    def __init__(self) -> None:
        self.frames_admitted = 0
        self.entered_the_stream = False

    @node.input(delivery_profile="ordered")
    def encoded_video_from_upstream(self) -> None: ...

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
        varint_timestamp_microseconds, access_unit = split_hang_legacy_container_media_frame(
            message.payload
        )
        report = {
            "starts_group": message.starts_group,
            "lost_since_last_read": message.lost_since_last_read,
            "varint_timestamp_microseconds": varint_timestamp_microseconds,
            "timestamp_ns": message.timestamp_ns,
            "opening_bytes": list(access_unit[:4]),
            "byte_count": len(access_unit),
        }
        if self.frames_admitted == 1:
            report["catalog_entry"] = ctx.inputs.inbound_link_catalog_entry(ENCODED_PORT)
        log.info(ENCODED_FRAME_MARKER + json.dumps(report))
        if self.frames_admitted == ENCODED_FRAMES_REPORTED:
            log.info(ENCODED_FRAMES_COMPLETE_MARKER)


@node
class EncodedFrameTimestampProbe:
    """Reports the frame-header stamp riding each of the encoder's messages.

    No group gate: this decodes nothing, so every message that arrives is one
    whose stamp a decoded frame downstream may carry.
    """

    @node.input(delivery_profile="ordered")
    def encoded_video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        payload, timestamp_ns = ctx.inputs.read_with_timestamp(ENCODED_PORT)
        if payload is None:
            return
        log.info(ENCODED_FRAME_STAMP_MARKER + json.dumps({"timestamp_ns": timestamp_ns}))


@node
class DecodedVideoFrameProbe:
    """Reports the decoder's first two messages whole, then every frame's stamp.

    The first two carry the whole message because that is what the extent, colour
    and surface assertions read; the stamps keep coming so the encoded side's
    report has something to overlap with.
    """

    def __init__(self) -> None:
        self.messages_seen = []
        self.stamps_reported = 0

    @node.input(delivery_profile="ordered")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        payload = ctx.inputs.read("video_from_upstream")
        if payload is None:
            return
        message = unpack(payload)
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
