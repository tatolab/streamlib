# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes whose audio input port declares a window contract.

The consumer the contract exists for: a model that wants exact-size blocks at
its own rate gets them as ordinary user code, with no private buffering. Every
one of these runs in its own child process, so what they report is the child's
own stage — the same Rust code the parent's mailboxes run — honouring a contract
that crossed the wiring envelope.

The source-following probes take their samples from a Python source in this
file rather than from a device, so the count under test is one the test states
rather than one the machine happens to have.
"""

import json
import math
from typing import Optional

import numpy
from tatolab.stream import (
    AudioWindowContract,
    Channels,
    ChannelsBlock,
    Input,
    Output,
    RuntimeContextLimitedAccess,
    log,
    monotonic_now_ns,
    node,
)

CONTIGUOUS_RESULT_MARKER = "MARKER:WINDOWS_SEEN "
ROLLING_RESULT_MARKER = "MARKER:ROLLING_WINDOWS_SEEN "
SOURCE_FOLLOWING_RESULT_MARKER = "MARKER:SOURCE_FOLLOWING_WINDOWS_SEEN "
DECLARED_MONO_RESULT_MARKER = "MARKER:DECLARED_MONO_WINDOWS_SEEN "

WINDOWS_REPORTED = 6

# What the Python source below publishes, and what a source-following consumer
# must therefore see: stereo at the rate its contract declares, so nothing is
# resampled and the count is the only thing under test.
SOURCE_SAMPLE_RATE = 48_000
SOURCE_CHANNELS = 2
SOURCE_FRAMES_PER_BLOCK = 480
WINDOW_SIZE = 960

# How far ahead of real time the publishing runs, so a late `process()` costs
# the consumer nothing. It does not make the stream gapless: this source starts
# publishing the moment its own `process()` first runs, which can precede the
# consumer child's subscriber going live, and those opening blocks are lost on
# the producer's ring before any mailbox sees them.
PUBLISHING_LEAD_NS = 100_000_000


def _reading(block: ChannelsBlock) -> dict:
    return {
        "sample_rate": block.sample_rate,
        "channels": block.channel_count,
        "sample_count": block.samples_per_channel,
        "dtype": block.dtype,
        "start_sample": block.start_sample,
        "first_sample_timestamp_ns": block.timestamp_ns,
        "shape": list(numpy.from_dlpack(block).shape),
    }


@node
class ExactWindowProbe:
    """Reports the shape and stamp of the first few windows it is handed."""

    def __init__(self) -> None:
        self.readings = []

    audio_from_upstream = Input(
        Channels,
        delivery="ordered",
        window=AudioWindowContract(
            sample_rate=16_000,
            channels=1,
            dtype="f32",
            window_size=512,
            hop=512,
        ),
    )

    def process(self, ctx) -> None:
        # Drained past the report so the probe measures the stage rather than
        # its own backlog draining away as counted drops.
        block = ctx.inputs.read(self.audio_from_upstream)
        if block is None or len(self.readings) >= WINDOWS_REPORTED:
            return
        self.readings.append(_reading(block))
        if len(self.readings) == WINDOWS_REPORTED:
            log.info(CONTIGUOUS_RESULT_MARKER + json.dumps(self.readings))


@node
class RollingWindowProbe:
    """A hop below the window: consecutive windows overlap by the difference."""

    def __init__(self) -> None:
        self.readings = []

    audio_from_upstream = Input(
        Channels,
        delivery="ordered",
        window=AudioWindowContract(
            sample_rate=16_000,
            channels=1,
            dtype="f32",
            window_size=512,
            hop=160,
        ),
    )

    def process(self, ctx) -> None:
        block = ctx.inputs.read(self.audio_from_upstream)
        if block is None or len(self.readings) >= WINDOWS_REPORTED:
            return
        self.readings.append(_reading(block))
        if len(self.readings) == WINDOWS_REPORTED:
            log.info(ROLLING_RESULT_MARKER + json.dumps(self.readings))


@node(execution="continuous", interval_ms=1)
class StereoToneSource:
    """Publishes a stereo tone at a stated rate, so a consumer's channel count
    is the test's own fact rather than the machine's.

    Paced to stay a bounded lead ahead of the monotonic clock: a burst larger
    than the consumer's mailbox is evicted there, and the lost blocks would
    read as the stage's failure rather than this fixture's.
    """

    audio = Output(Channels, rate=SOURCE_SAMPLE_RATE, dtype="f32")

    def __init__(self) -> None:
        self._frames_published = 0
        self._first_sample_timestamp_ns: Optional[int] = None

    def _is_far_enough_ahead(self, anchor_ns: int) -> bool:
        published_ns = self._frames_published * 1_000_000_000 // SOURCE_SAMPLE_RATE
        elapsed_ns = monotonic_now_ns() - anchor_ns
        return published_ns - elapsed_ns > PUBLISHING_LEAD_NS

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        anchor_ns = self._first_sample_timestamp_ns
        if anchor_ns is None:
            anchor_ns = monotonic_now_ns()
            self._first_sample_timestamp_ns = anchor_ns
        elif self._is_far_enough_ahead(anchor_ns):
            return

        at = self._frames_published
        instants = (at + numpy.arange(SOURCE_FRAMES_PER_BLOCK)) / SOURCE_SAMPLE_RATE
        tone = numpy.sin(math.tau * 440.0 * instants).astype(numpy.float32)
        ctx.outputs.write(
            self.audio,
            numpy.ascontiguousarray(numpy.stack([tone] * SOURCE_CHANNELS)),
            # Derived from the samples before it rather than read fresh, so the
            # stamps describe one gapless stream even though the publishing
            # runs ahead of real time.
            timestamp_ns=anchor_ns + at * 1_000_000_000 // SOURCE_SAMPLE_RATE,
            start_sample=at,
        )
        self._frames_published += SOURCE_FRAMES_PER_BLOCK


@node
class SourceFollowingWindowProbe:
    """Declares no channel count, so its windows carry the source's own."""

    def __init__(self) -> None:
        self.readings = []

    audio_from_upstream = Input(
        Channels,
        delivery="ordered",
        window=AudioWindowContract(
            sample_rate=SOURCE_SAMPLE_RATE,
            dtype="f32",
            window_size=WINDOW_SIZE,
        ),
    )

    def process(self, ctx) -> None:
        block = ctx.inputs.read(self.audio_from_upstream)
        if block is None or len(self.readings) >= WINDOWS_REPORTED:
            return
        self.readings.append(_reading(block))
        if len(self.readings) == WINDOWS_REPORTED:
            log.info(SOURCE_FOLLOWING_RESULT_MARKER + json.dumps(self.readings))


@node
class DeclaredMonoWindowProbe:
    """The same source through a contract that does state a count: proof the
    declared path still converts while the one beside it follows."""

    def __init__(self) -> None:
        self.readings = []

    audio_from_upstream = Input(
        Channels,
        delivery="ordered",
        window=AudioWindowContract(
            sample_rate=SOURCE_SAMPLE_RATE,
            dtype="f32",
            window_size=WINDOW_SIZE,
            channels=1,
        ),
    )

    def process(self, ctx) -> None:
        block = ctx.inputs.read(self.audio_from_upstream)
        if block is None or len(self.readings) >= WINDOWS_REPORTED:
            return
        self.readings.append(_reading(block))
        if len(self.readings) == WINDOWS_REPORTED:
            log.info(DECLARED_MONO_RESULT_MARKER + json.dumps(self.readings))
