# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probe that reads the microphone module's blocks from a processor interpreter.

Reports what each Channels block read for the first few blocks as one JSON
marker line on `tatolabd`'s standard error — the boundary under test is native
capture in the microphone's helper process reaching a Python processor in its
own interpreter, as numpy over the borrowed sample.
"""

import json

import numpy

from tatolab.stream import Channels, Input, log, node

RESULT_MARKER = "MARKER:BLOCKS_SEEN "

BLOCKS_REPORTED = 8


@node
class ChannelsBlockProbe:
    """Reports what each read saw for the first few blocks the source publishes."""

    def __init__(self) -> None:
        self.readings = []

    # The plan's profile for audio: order carries meaning, so the probe reads
    # blocks in the order they were published rather than skipping to the
    # freshest. It promises nothing about how many arrive — loss at the device
    # edge is counted there, loss on this link is counted at this port.
    audio_from_upstream = Input(Channels, delivery="ordered")

    def process(self, ctx) -> None:
        # Drained even after the report is out: a probe that stopped reading
        # would be measuring its own backlog draining away as counted drops
        # rather than the capture.
        block = ctx.inputs.read(self.audio_from_upstream)
        if block is None or len(self.readings) >= BLOCKS_REPORTED:
            return

        samples = numpy.from_dlpack(block)
        self.readings.append(
            {
                "sample_rate": block.sample_rate,
                "channels": block.channel_count,
                "sample_count": block.samples_per_channel,
                "dtype": block.dtype,
                "start_sample": block.start_sample,
                "first_sample_timestamp_ns": block.timestamp_ns,
                "shape": list(samples.shape),
                "numpy_type": samples.dtype.str,
                "loudest_sample": float(numpy.max(numpy.abs(samples))),
                "samples_borrow_the_sample_with_no_copy": not samples.flags.owndata,
            }
        )
        if len(self.readings) == BLOCKS_REPORTED:
            log.info(RESULT_MARKER + json.dumps(self.readings))
