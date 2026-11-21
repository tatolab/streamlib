# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""One voice of the chord: a sine wave published on a Channels port.

Importable as `nodes.tone_source:ToneSource`, which is the name the engine
spawns this node's child interpreter with. A voice publishes whatever rate,
channel count and block size its settings name and knows nothing about its
consumer — converting it is the consumer's window contract's job.
"""

import math
from typing import Annotated

import numpy
from tatolab.stream import (
    Audio,
    Description,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    SettingOnly,
    log,
    node,
)

# How far behind real time a voice may fall before it starts a new run rather
# than emitting the whole backlog: a burst that large outruns any mailbox, and
# the windowing stage downstream flushes on the discontinuity instead of
# blending audio across it.
MAXIMUM_BLOCKS_OF_CATCH_UP = 4

NANOSECONDS_PER_SECOND = 1_000_000_000


@node.meta(
    description="A phase-continuous sine wave at the rate and channel count its settings name",
)
@node(execution="continuous", interval_ms=5)
class ToneSource:
    """A phase-continuous sine wave, paced by the monotonic clock.

    The tick is shorter than any block: each `process()` emits however many
    whole blocks elapsed time owes, so the stream stays locked to real time at
    any rate and block size.
    """

    frequency_hz: Annotated[
        float, NumberRange(minimum=0.0), Description("The tone's pitch, in hertz.")
    ] = 440.0
    sample_rate: Annotated[
        int,
        SettingOnly(),
        NumberRange(minimum=1),
        Description("The samples a second this voice is written at."),
    ] = 48_000
    channels: Annotated[
        int,
        SettingOnly(),
        NumberRange(minimum=1),
        Description("How many channels carry the tone, each the same."),
    ] = 1
    block_size: Annotated[
        int,
        SettingOnly(),
        NumberRange(minimum=1),
        Description("The samples in each block this voice writes."),
    ] = 512
    amplitude: Annotated[
        float, NumberRange(minimum=0.0, maximum=1.0), Description("The tone's peak level.")
    ] = 0.3
    voice_to_downstream: OutputPin[Annotated[Audio, Description("This voice's sine wave")]]

    def __init__(self) -> None:
        self._run_anchor_timestamp_ns: int | None = None
        # The stream-wide index of this run's first sample. A new run carries
        # the samples it skipped, so the index keeps counting real time.
        self._start_sample_of_this_run = 0
        self._samples_emitted_in_this_run = 0
        self._next_phase_radians = 0.0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        now_ns = ctx.time
        if self._run_anchor_timestamp_ns is None:
            self._run_anchor_timestamp_ns = now_ns

        samples_owed = self._samples_elapsed_time_owes(now_ns)
        if samples_owed - self._samples_emitted_in_this_run > (
            MAXIMUM_BLOCKS_OF_CATCH_UP * self.block_size
        ):
            log.warn(
                "fell far enough behind real time to start a new run; the gap is "
                "left in the timestamps rather than filled with invented samples",
                samples_behind=samples_owed - self._samples_emitted_in_this_run,
                frequency_hz=self.frequency_hz,
            )
            self._start_sample_of_this_run += samples_owed
            self._run_anchor_timestamp_ns = now_ns
            self._samples_emitted_in_this_run = 0
            samples_owed = 0

        while self._samples_emitted_in_this_run + self.block_size <= samples_owed:
            self._publish_one_block()

    def _samples_elapsed_time_owes(self, now_ns: int) -> int:
        assert self._run_anchor_timestamp_ns is not None
        elapsed_ns = now_ns - self._run_anchor_timestamp_ns
        return (elapsed_ns * self.sample_rate) // NANOSECONDS_PER_SECOND

    def _publish_one_block(self) -> None:
        first_sample_timestamp_ns = self._stamp_for_the_next_block()
        # The rate is a setting the class cannot state, so the block names it.
        self.voice_to_downstream.write(
            self._next_planar_samples(),
            timestamp_ns=first_sample_timestamp_ns,
            start_sample=self._start_sample_of_this_run + self._samples_emitted_in_this_run,
            sample_rate=self.sample_rate,
        )
        self._samples_emitted_in_this_run += self.block_size

    def _stamp_for_the_next_block(self) -> int:
        """The run's anchor plus the emitted-sample offset, in integer arithmetic.

        Never an accumulated per-block delta, which drifts at 44.1 kHz-family
        rates where a block's duration is not a whole number of nanoseconds.
        """
        assert self._run_anchor_timestamp_ns is not None
        return (
            self._run_anchor_timestamp_ns
            + (self._samples_emitted_in_this_run * NANOSECONDS_PER_SECOND)
            // self.sample_rate
        )

    def _next_planar_samples(self) -> numpy.ndarray:
        """The next block, `[channel][sample]`, every channel the same tone."""
        phase_step_radians = 2.0 * math.pi * self.frequency_hz / self.sample_rate
        phases = self._next_phase_radians + phase_step_radians * numpy.arange(
            self.block_size, dtype=numpy.float64
        )
        # Carried across blocks and wrapped, so a voice never clicks at a block
        # edge and the phase stays exact over a long run.
        self._next_phase_radians = math.fmod(
            self._next_phase_radians + phase_step_radians * self.block_size,
            2.0 * math.pi,
        )
        one_channel = (self.amplitude * numpy.sin(phases)).astype(numpy.float32)
        return numpy.ascontiguousarray(
            numpy.broadcast_to(one_channel, (self.channels, self.block_size))
        )
