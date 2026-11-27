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
`DecodedVideoFrameProbe` reads the far side of the decoder as Texture frames.

The two stamp reports are uncapped, unlike the cast report. Each probe is its
own helper process attaching at its own pace, so a short window on either side
could close before the other opened, and a cross-check needs the two to
overlap.
"""

import dataclasses
import json
from typing import Annotated, Callable

from tatolab.stream import EncodedVideo, InputPin, RuntimeContextLimitedAccess, StreamDelivery, Texture, log, node

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

EncodedVideoInOrder = Annotated[EncodedVideo, StreamDelivery("ordered")]


@node(execution="reactive")
def EncodedFrameProbe(
    *,
    encoded_video_from_upstream: InputPin[EncodedVideoInOrder],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads the encoder's output as access units and reports each one."""

    frames_admitted = 0
    entered_the_stream = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal entered_the_stream, frames_admitted
        if frames_admitted >= ENCODED_FRAMES_REPORTED:
            return
        message = encoded_video_from_upstream.read_message()
        if message is None:
            return
        # The doctrine every reader of an encoded stream owes it: enter only
        # at a group start. The first message off this link is not necessarily
        # the producer's first — attaching mid-group hands over frames whose
        # keyframe is already gone.
        if not entered_the_stream:
            if not message.starts_group:
                return
            entered_the_stream = True
        frames_admitted += 1
        access_unit = bytes(memoryview(message.payload.payload).cast("B"))
        report = {
            "starts_group": message.starts_group,
            "lost_since_last_read": message.lost_since_last_read,
            "timestamp_ns": message.timestamp_ns,
            "opening_bytes": list(access_unit[:4]),
            "byte_count": len(access_unit),
        }
        if frames_admitted == 1:
            upstream_stream_facts = encoded_video_from_upstream.upstream_stream_facts()
            report["stream_facts"] = (
                None if upstream_stream_facts is None else upstream_stream_facts.to_wire()
            )
        log.info(ENCODED_FRAME_MARKER + json.dumps(report))
        if frames_admitted == ENCODED_FRAMES_REPORTED:
            log.info(ENCODED_FRAMES_COMPLETE_MARKER)

    return each_frame


@node(execution="reactive")
def EncodedFrameTimestampProbe(
    *,
    encoded_video_from_upstream: InputPin[EncodedVideoInOrder],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the frame-header stamp riding each of the encoder's messages.

    No group gate: this decodes nothing, so every message that arrives is one
    whose stamp a decoded frame downstream may carry.
    """

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        message = encoded_video_from_upstream.read_message()
        if message is None:
            return
        log.info(ENCODED_FRAME_STAMP_MARKER + json.dumps({"timestamp_ns": message.timestamp_ns}))

    return each_frame


@node(execution="reactive")
def DecodedVideoFrameProbe(
    *,
    video_from_upstream: InputPin[Annotated[Texture, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the decoder's first two Texture frames whole, then every frame's stamp.

    The first two carry the whole frame because that is what the extent, colour
    and surface assertions read; the stamps keep coming so the encoded side's
    report has something to overlap with.
    """

    messages_seen = []
    stamps_reported = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal stamps_reported
        frame = video_from_upstream.read()
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
        if len(messages_seen) < 2:
            messages_seen.append(message)
            if len(messages_seen) == 2:
                log.info(DECODED_FRAMES_MARKER + json.dumps(messages_seen))
        stamps_reported += 1
        log.info(
            DECODED_FRAME_STAMP_MARKER
            + json.dumps({"timestamp_ns": message["timestamp_ns"]})
        )
        if stamps_reported == DECODED_STAMPS_REPORTED:
            log.info(DECODED_FRAME_STAMPS_COMPLETE_MARKER)

    return each_frame
