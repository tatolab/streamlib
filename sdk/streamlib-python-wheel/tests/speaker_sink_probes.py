# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A probe that says when enough audio has flowed to judge a playback run by.

It hangs off the same microphone output the speaker reads, so it is a second
consumer rather than a stage between the two built-ins — the samples the
speaker plays never enter an interpreter.

The count is what makes the run long enough to mean something. A speaker's
underruns are dominated by its cold start, so a run of a few blocks cannot tell
a startup transient from a stream losing a period at a time; a couple of
seconds of blocks can.
"""

from streamlib import RuntimeContextLimitedAccess, input, log, processor

RESULT_MARKER = "MARKER:BLOCKS_COUNTED "

# Roughly two seconds at a 21 ms PipeWire quantum, and longer at a smaller one
# — the bound below is on blocks rather than on time either way.
BLOCKS_TO_COUNT = 100


@processor
class AudioBlockCountingProbe:
    """Reports once enough blocks have crossed the link to judge a run by."""

    def __init__(self) -> None:
        self.blocks_seen = 0

    # The plan's profile for audio: order matters and no sample may be dropped
    # on the consumer side.
    @input(delivery_profile="lossless")
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # Drained even after the report is out: a `lossless` port makes the
        # producer wait, so a probe that stopped reading would stall the
        # microphone the speaker is being fed by.
        if ctx.inputs.read("audio_from_upstream") is None:
            return
        self.blocks_seen += 1
        if self.blocks_seen == BLOCKS_TO_COUNT:
            log.info(RESULT_MARKER + str(self.blocks_seen))
