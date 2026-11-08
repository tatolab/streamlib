# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The nodes `test_stream_facts.py` drives: three sources and a reader.

In its own module because a helper process imports each class by its import
path, and a class declared inside a pytest module would have the child import
the test suite.
"""

import json

from tatolab.stream import (
    Image,
    Input,
    Output,
    Packets,
    Records,
    RuntimeContextLimitedAccess,
    log,
    node,
)
from tatolab.stream.stream_facts import ImageStreamFacts, ImageStreamFrameRate, PacketsStreamFacts

DECLARED_FRAMES_PER_SECOND = 30
REPLACEMENT_STREAM_FACTS = PacketsStreamFacts(
    rfc6381_codec="avc1.64001f", coded_width=1280, coded_height=720
)


@node.process(execution="continuous", interval_ms=50)
class DeclaresItsFrameRateAndWritesRecords:
    """Declares an Image frame rate it never replaces, and writes a record every tick."""

    video = Output(Image, rate=DECLARED_FRAMES_PER_SECOND, description="Frames, never written")
    records = Output(Records, description="Records")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.outputs.write(self.records, {"kept": True})


@node.process(execution="continuous", interval_ms=50)
class ReplacesItsStreamFactsOnItsFirstTick:
    """Replaces its port's stream facts on its first tick, after trying another family's.

    Reports `MARKER:STREAM_FACTS_REFUSED <message>` and then
    `MARKER:STREAM_FACTS_REPLACED`, so a reader of `graph` knows the
    replacement has landed before it looks.
    """

    def __init__(self) -> None:
        self.replaced = False

    encoded = Output(Packets, codec="h264", description="Encoded units")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if not self.replaced:
            self.replaced = True
            try:
                ctx.outputs.set_stream_facts(
                    "encoded", ImageStreamFacts(frame_rate=ImageStreamFrameRate(30, 1))
                )
            except Exception as refusal:
                log.info("MARKER:STREAM_FACTS_REFUSED " + json.dumps(str(refusal)))
            ctx.outputs.set_stream_facts("encoded", REPLACEMENT_STREAM_FACTS)
            log.info("MARKER:STREAM_FACTS_REPLACED")
        ctx.outputs.write("encoded", b"\x00\x00\x00\x01\x65")


@node.process
class ReportsItsUpstreamsStreamFacts:
    """Reports the stream facts its one upstream port carries, every message, as
    `MARKER:UPSTREAM_STREAM_FACTS <json>`."""

    encoded_from_upstream = Input(Packets, delivery="newest")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if ctx.inputs.read("encoded_from_upstream") is None:
            return
        upstream_stream_facts = ctx.inputs.upstream_stream_facts("encoded_from_upstream")
        log.info(
            "MARKER:UPSTREAM_STREAM_FACTS "
            + json.dumps(None if upstream_stream_facts is None else upstream_stream_facts.to_wire())
        )
