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
import struct
from typing import Optional

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    AudioBlock,
    AudioWindowContract,
    RuntimeContextLimitedAccess,
    input,
    log,
    monotonic_now_ns,
    output,
    processor,
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

# How far ahead of real time the publishing runs. Enough that a late
# `process()` costs the consumer nothing, and well inside the depth its
# windowed port is sized to, so no block is evicted between the two.
PUBLISHING_LEAD_NS = 100_000_000


def _reading(block) -> dict:
    return {
        "sample_rate": block.sample_rate,
        "channels": block.channels,
        "sample_count": block.sample_count,
        "dtype": block.dtype,
        "first_sample_timestamp_ns": block.first_sample_timestamp_ns,
        "shape": list(block.samples.shape),
    }


@processor
class ExactWindowProbe:
    """Reports the shape and stamp of the first few windows it is handed."""

    def __init__(self) -> None:
        self.readings = []

    @input(
        delivery_profile="ordered",
        audio_window=AudioWindowContract(
            sample_rate=16_000,
            channels=1,
            dtype="f32",
            window_size=512,
            hop=512,
        ),
    )
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        # Drained past the report so the probe measures the stage rather than
        # its own backlog draining away as counted drops.
        block = ctx.inputs.read("audio_from_upstream", into=AudioBlock)
        if block is None or len(self.readings) >= WINDOWS_REPORTED:
            return
        self.readings.append(_reading(block))
        if len(self.readings) == WINDOWS_REPORTED:
            log.info(CONTIGUOUS_RESULT_MARKER + json.dumps(self.readings))


@processor
class RollingWindowProbe:
    """A hop below the window: consecutive windows overlap by the difference."""

    def __init__(self) -> None:
        self.readings = []

    @input(
        delivery_profile="ordered",
        audio_window=AudioWindowContract(
            sample_rate=16_000,
            channels=1,
            dtype="f32",
            window_size=512,
            hop=160,
        ),
    )
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        block = ctx.inputs.read("audio_from_upstream", into=AudioBlock)
        if block is None or len(self.readings) >= WINDOWS_REPORTED:
            return
        self.readings.append(_reading(block))
        if len(self.readings) == WINDOWS_REPORTED:
            log.info(ROLLING_RESULT_MARKER + json.dumps(self.readings))


@processor(execution="continuous", interval_ms=1)
class StereoToneSource:
    """Publishes a stereo tone at a stated rate, so a consumer's channel count
    is the test's own fact rather than the machine's.

    Paced to stay a bounded lead ahead of the monotonic clock: a burst larger
    than the consumer's mailbox is evicted there, and the lost blocks would
    read as the stage's failure rather than this fixture's.
    """

    @output()
    def audio(self) -> None: ...

    def __init__(self) -> None:
        self._frames_published = 0
        self._first_sample_timestamp_ns: "Optional[int]" = None

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
        scalars = []
        for offset in range(SOURCE_FRAMES_PER_BLOCK):
            instant = (at + offset) / SOURCE_SAMPLE_RATE
            scalars.extend([math.sin(math.tau * 440.0 * instant)] * SOURCE_CHANNELS)

        ctx.outputs.write(
            "audio",
            {
                "samples": struct.pack(f"<{len(scalars)}f", *scalars),
                "sample_rate": SOURCE_SAMPLE_RATE,
                "channels": SOURCE_CHANNELS,
                "sample_count": SOURCE_FRAMES_PER_BLOCK,
                "dtype": "f32",
                # Derived from the samples before it rather than read fresh, so
                # the stamps describe one gapless stream even though the
                # publishing runs ahead of real time.
                "first_sample_timestamp_ns": (
                    anchor_ns + at * 1_000_000_000 // SOURCE_SAMPLE_RATE
                ),
            },
        )
        self._frames_published += SOURCE_FRAMES_PER_BLOCK


@processor
class SourceFollowingWindowProbe:
    """Declares no channel count, so its windows carry the source's own."""

    def __init__(self) -> None:
        self.readings = []

    @input(
        delivery_profile="ordered",
        audio_window=AudioWindowContract(
            sample_rate=SOURCE_SAMPLE_RATE,
            dtype="f32",
            window_size=WINDOW_SIZE,
        ),
    )
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        block = ctx.inputs.read("audio_from_upstream", into=AudioBlock)
        if block is None or len(self.readings) >= WINDOWS_REPORTED:
            return
        self.readings.append(_reading(block))
        if len(self.readings) == WINDOWS_REPORTED:
            log.info(SOURCE_FOLLOWING_RESULT_MARKER + json.dumps(self.readings))


@processor
class DeclaredMonoWindowProbe:
    """The same source through a contract that does state a count: proof the
    declared path still converts while the one beside it follows."""

    def __init__(self) -> None:
        self.readings = []

    @input(
        delivery_profile="ordered",
        audio_window=AudioWindowContract(
            sample_rate=SOURCE_SAMPLE_RATE,
            dtype="f32",
            window_size=WINDOW_SIZE,
            channels=1,
        ),
    )
    def audio_from_upstream(self) -> None: ...

    def process(self, ctx) -> None:
        block = ctx.inputs.read("audio_from_upstream", into=AudioBlock)
        if block is None or len(self.readings) >= WINDOWS_REPORTED:
            return
        self.readings.append(_reading(block))
        if len(self.readings) == WINDOWS_REPORTED:
            log.info(DECLARED_MONO_RESULT_MARKER + json.dumps(self.readings))
