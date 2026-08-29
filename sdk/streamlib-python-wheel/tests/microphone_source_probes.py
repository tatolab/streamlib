# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probe that reads the audio built-in's blocks from a helper process.

Reports what the `AudioBlock` cast saw for the first few blocks as one JSON
marker line, riding the child→parent log forwarding — the boundary under test
is native capture in the app process reaching a Python processor in its own
child, as numpy.
"""

import json

import numpy

from streamlib import AudioBlock, input, log, processor

RESULT_MARKER = "MARKER:BLOCKS_SEEN "

BLOCKS_REPORTED = 8


@processor
class AudioBlockProbe:
    """Reports what the cast saw for the first few blocks the source publishes."""

    def __init__(self) -> None:
        self.readings = []

    # The plan's profile for audio: order carries meaning, so the probe reads
    # blocks in the order they were published rather than skipping to the
    # freshest. It promises nothing about how many arrive — loss at the device
    # edge is counted there, loss on this link is counted at this port.
    @input(delivery_profile="ordered")
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        # Drained even after the report is out: a probe that stopped reading
        # would be measuring its own backlog draining away as counted drops
        # rather than the capture.
        block = ctx.inputs.read("audio_from_upstream", into=AudioBlock)
        if block is None or len(self.readings) >= BLOCKS_REPORTED:
            return

        samples = block.samples
        # `reshape` hands back a view of the `frombuffer` view, so the bag's
        # own bytes sit one link further down the chain than `samples.base`.
        viewed = samples
        while viewed.base is not None and isinstance(viewed.base, numpy.ndarray):
            viewed = viewed.base
        self.readings.append(
            {
                "sample_rate": block.sample_rate,
                "channels": block.channels,
                "sample_count": block.sample_count,
                "dtype": block.dtype,
                "first_sample_timestamp_ns": block.first_sample_timestamp_ns,
                "shape": list(samples.shape),
                "numpy_type": samples.dtype.str,
                "loudest_sample": float(numpy.max(numpy.abs(samples))),
                "samples_are_a_view_over_the_bag_bytes": (
                    viewed.base is block.interleaved_sample_bytes
                ),
            }
        )
        if len(self.readings) == BLOCKS_REPORTED:
            log.info(RESULT_MARKER + json.dumps(self.readings))
