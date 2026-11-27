# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The nodes `test_stream_facts.py` drives: three sources and a reader.

In its own module because a helper process imports each class by its import
path, and a class declared inside a pytest module would have the child import
the test suite.
"""

import json
from dataclasses import dataclass
from typing import Annotated, Callable

from tatolab.stream import (
    Description,
    EncodedVideo,
    InputPin,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    Texture,
    log,
    node,
)
from tatolab.stream.stream_facts import ImageStreamFacts, ImageStreamFrameRate, PacketsStreamFacts

DECLARED_FRAMES_PER_SECOND = 30
REPLACEMENT_STREAM_FACTS = PacketsStreamFacts(
    rfc6381_codec="avc1.64001f", coded_width=1280, coded_height=720
)


@dataclass(frozen=True)
class Kept:
    """A record that says it was kept."""

    kept: bool


@node(execution="continuous", interval_ms=50)
def DeclaresItsFrameRateAndWritesRecords(
    *,
    video: OutputPin[Annotated[Texture, Description("Frames, never written")]],
    records: OutputPin[Annotated[Kept, Description("Records")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """States a Texture frame rate on its first tick and never replaces it, reporting
    `MARKER:FRAME_RATE_STATED`, and writes a bundle every tick."""

    stated = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal stated
        if not stated:
            stated = True
            video.set_stream_facts(
                ImageStreamFacts(frame_rate=ImageStreamFrameRate(DECLARED_FRAMES_PER_SECOND, 1))
            )
            log.info("MARKER:FRAME_RATE_STATED")
        records.write(Kept(True))

    return each_frame


@node(execution="continuous", interval_ms=50)
def ReplacesItsStreamFactsOnItsFirstTick(
    *,
    encoded: OutputPin[Annotated[EncodedVideo, Description("Encoded units")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Replaces its port's stream facts on its first tick, after trying another type's.

    Reports `MARKER:STREAM_FACTS_REFUSED <message>` and then
    `MARKER:STREAM_FACTS_REPLACED`, so a reader of `graph` knows the
    replacement has landed before it looks.
    """

    replaced = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal replaced
        if not replaced:
            replaced = True
            try:
                encoded.set_stream_facts(
                    ImageStreamFacts(frame_rate=ImageStreamFrameRate(30, 1))
                )
            except Exception as refusal:
                log.info("MARKER:STREAM_FACTS_REFUSED " + json.dumps(str(refusal)))
            encoded.set_stream_facts(REPLACEMENT_STREAM_FACTS)
            log.info("MARKER:STREAM_FACTS_REPLACED")
        encoded.write(b"\x00\x00\x00\x01\x65")

    return each_frame


@node(execution="reactive")
def ReportsItsUpstreamsStreamFacts(
    *,
    encoded_from_upstream: InputPin[Annotated[EncodedVideo, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the stream facts its one upstream port carries, every message, as
    `MARKER:UPSTREAM_STREAM_FACTS <json>`."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        if encoded_from_upstream.read() is None:
            return
        upstream_stream_facts = encoded_from_upstream.upstream_stream_facts()
        log.info(
            "MARKER:UPSTREAM_STREAM_FACTS "
            + json.dumps(None if upstream_stream_facts is None else upstream_stream_facts.to_wire())
        )

    return each_frame
