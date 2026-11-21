# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A probe that says when enough audio has flowed to judge a playback run by.

It hangs off the same microphone output the speaker reads, so it is a second
consumer rather than a stage between the two nodes — the samples the
speaker plays never enter an interpreter.

The count is what makes the run long enough to mean something. A speaker's
underruns are dominated by its cold start, so a run of a few blocks cannot tell
a startup transient from a stream losing a period at a time; a couple of
seconds of blocks can.
"""

from typing import Annotated

from tatolab.stream import Audio, InputPin, RuntimeContextLimitedAccess, StreamDelivery, log, node

RESULT_MARKER = "MARKER:BLOCKS_COUNTED "

# Roughly two seconds at a 21 ms PipeWire quantum, and longer at a smaller one
# — the bound below is on blocks rather than on time either way.
BLOCKS_TO_COUNT = 100


@node
class AudioBlockCountingProbe:
    """Reports once enough blocks have crossed the link to judge a run by."""

    def __init__(self) -> None:
        self._blocks_seen = 0

    audio_from_upstream: InputPin[Annotated[Audio, StreamDelivery("ordered")]]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # Drained even after the report is out: a probe that stopped reading
        # would back its own link up behind the microphone the speaker is fed
        # by, and what a full link costs is dropped blocks rather than a
        # stalled producer — `PortMailbox::push_frame_from_inbound_link`
        # evicts its oldest entry whatever a port's profile says.
        if self.audio_from_upstream.read() is None:
            return
        self._blocks_seen += 1
        if self._blocks_seen == BLOCKS_TO_COUNT:
            log.info(RESULT_MARKER + str(self._blocks_seen))
