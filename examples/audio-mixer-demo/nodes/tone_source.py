# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""One voice of the chord: a sine wave published on a Channels port.

Importable as `nodes.tone_source:ToneSource`, which is the name the engine
spawns this node's child interpreter with. A voice publishes whatever rate,
channel count and block size it is configured with and knows nothing about its
consumer — converting it is the consumer's window contract's job.
"""

import math
from dataclasses import dataclass

import numpy
from tatolab.stream import (
    Channels,
    Output,
    RuntimeContextLimitedAccess,
    from_config,
    log,
    node,
)

VOICE_OUTPUT_PORT = "voice_to_downstream"

# How far behind real time a voice may fall before it starts a new run rather
# than emitting the whole backlog: a burst that large outruns any mailbox, and
# the windowing stage downstream flushes on the discontinuity instead of
# blending audio across it.
MAXIMUM_BLOCKS_OF_CATCH_UP = 4

NANOSECONDS_PER_SECOND = 1_000_000_000


@dataclass
class ToneSourceConfig:
    """ToneSource's settings, as its node's config in the graph document spells them."""

    frequency_hz: float = 440.0
    sample_rate: int = 48_000
    channels: int = 1
    block_size: int = 512
    amplitude: float = 0.3


@node.meta(
    description="A phase-continuous sine wave at the rate and channel count its config names",
)
@node.process(execution="continuous", interval_ms=5)
class ToneSource:
    """A phase-continuous sine wave, paced by the monotonic clock.

    The tick is shorter than any block: each `process()` emits however many
    whole blocks elapsed time owes, so the stream stays locked to real time at
    any rate and block size.
    """

    voice_to_downstream = Output(
        Channels,
        rate=from_config("sample_rate"),
        dtype="f32",
        description="This voice's sine wave",
    )

    def __init__(self, config: ToneSourceConfig) -> None:
        self.frequency_hz = float(config.frequency_hz)
        self.sample_rate = int(config.sample_rate)
        self.channels = int(config.channels)
        self.block_size = int(config.block_size)
        self.amplitude = float(config.amplitude)

        # A block_size of zero never advances the emitted-sample counter, so
        # `process()` would publish empty blocks forever.
        for field_name, value in (
            ("sample_rate", self.sample_rate),
            ("channels", self.channels),
            ("block_size", self.block_size),
        ):
            if value <= 0:
                raise ValueError(
                    f"ToneSource was configured with {field_name}={value} — the rate, "
                    f"channel count and block size are each strictly positive"
                )

        self.run_anchor_timestamp_ns: int | None = None
        # The stream-wide index of this run's first sample. A new run carries
        # the samples it skipped, so the index keeps counting real time.
        self.start_sample_of_this_run = 0
        self.samples_emitted_in_this_run = 0
        self.next_phase_radians = 0.0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        now_ns = ctx.time
        if self.run_anchor_timestamp_ns is None:
            self.run_anchor_timestamp_ns = now_ns

        samples_owed = self._samples_elapsed_time_owes(now_ns)
        if samples_owed - self.samples_emitted_in_this_run > (
            MAXIMUM_BLOCKS_OF_CATCH_UP * self.block_size
        ):
            log.warn(
                "fell far enough behind real time to start a new run; the gap is "
                "left in the timestamps rather than filled with invented samples",
                samples_behind=samples_owed - self.samples_emitted_in_this_run,
                frequency_hz=self.frequency_hz,
            )
            self.start_sample_of_this_run += samples_owed
            self.run_anchor_timestamp_ns = now_ns
            self.samples_emitted_in_this_run = 0
            samples_owed = 0

        while self.samples_emitted_in_this_run + self.block_size <= samples_owed:
            self._publish_one_block(ctx)

    def _samples_elapsed_time_owes(self, now_ns: int) -> int:
        assert self.run_anchor_timestamp_ns is not None
        elapsed_ns = now_ns - self.run_anchor_timestamp_ns
        return (elapsed_ns * self.sample_rate) // NANOSECONDS_PER_SECOND

    def _publish_one_block(self, ctx: RuntimeContextLimitedAccess) -> None:
        first_sample_timestamp_ns = self._stamp_for_the_next_block()
        # The output's rate is the config's, which the class cannot state, so
        # the block names it.
        ctx.outputs.write_channels_block(
            VOICE_OUTPUT_PORT,
            self._next_planar_samples(),
            start_sample=self.start_sample_of_this_run + self.samples_emitted_in_this_run,
            sample_rate=self.sample_rate,
            timestamp_ns=first_sample_timestamp_ns,
        )
        self.samples_emitted_in_this_run += self.block_size

    def _stamp_for_the_next_block(self) -> int:
        """The run's anchor plus the emitted-sample offset, in integer arithmetic.

        Never an accumulated per-block delta, which drifts at 44.1 kHz-family
        rates where a block's duration is not a whole number of nanoseconds.
        """
        assert self.run_anchor_timestamp_ns is not None
        return (
            self.run_anchor_timestamp_ns
            + (self.samples_emitted_in_this_run * NANOSECONDS_PER_SECOND)
            // self.sample_rate
        )

    def _next_planar_samples(self) -> numpy.ndarray:
        """The next block, `[channel][sample]`, every channel the same tone."""
        phase_step_radians = 2.0 * math.pi * self.frequency_hz / self.sample_rate
        phases = self.next_phase_radians + phase_step_radians * numpy.arange(
            self.block_size, dtype=numpy.float64
        )
        # Carried across blocks and wrapped, so a voice never clicks at a block
        # edge and the phase stays exact over a long run.
        self.next_phase_radians = math.fmod(
            self.next_phase_radians + phase_step_radians * self.block_size,
            2.0 * math.pi,
        )
        one_channel = (self.amplitude * numpy.sin(phases)).astype(numpy.float32)
        return numpy.ascontiguousarray(
            numpy.broadcast_to(one_channel, (self.channels, self.block_size))
        )
