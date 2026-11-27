# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Probe that reads the microphone module's blocks from a node interpreter.

Reports what each audio block read for the first few blocks as one JSON
marker line on `tatolabd`'s standard error — the boundary under test is native
capture in the microphone's helper process reaching a Python node in its
own interpreter, as numpy over the borrowed sample.
"""

import json
from typing import Annotated, Callable

import numpy

from tatolab.stream import Audio, InputPin, RuntimeContextLimitedAccess, StreamDelivery, log, node

RESULT_MARKER = "MARKER:BLOCKS_SEEN "

BLOCKS_REPORTED = 8


@node(execution="reactive")
def ChannelsBlockProbe(
    *,
    audio_from_upstream: InputPin[Annotated[Audio, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports what each read saw for the first few blocks the source publishes."""

    readings = []

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        # Drained even after the report is out: a probe that stopped reading
        # would be measuring its own backlog draining away as counted drops
        # rather than the capture.
        block = audio_from_upstream.read()
        if block is None or len(readings) >= BLOCKS_REPORTED:
            return

        samples = numpy.from_dlpack(block)
        readings.append(
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
        if len(readings) == BLOCKS_REPORTED:
            log.info(RESULT_MARKER + json.dumps(readings))

    return each_frame
