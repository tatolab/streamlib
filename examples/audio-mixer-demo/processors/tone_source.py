# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""One voice of the chord: a sine wave published as `AudioBlock` bags.

Each voice is added three times with a different frequency, rate, channel
count and block size, so what reaches the mixer is three genuinely different
audio streams. Normalising them is the window contract's job, not this
processor's — a voice publishes whatever shape it was configured with and
knows nothing about its consumer.
"""

import math

import numpy

from streamlib import RuntimeContextLimitedAccess, log, output, processor

VOICE_OUTPUT_PORT = "voice_to_downstream"

# How far behind real time a voice may fall before it starts a new run rather
# than emitting the whole backlog. A burst that large outruns any mailbox, and
# carrying the debt forever would drift; re-anchoring puts the stamps back on
# real time, and the windowing stage downstream flushes on the discontinuity
# instead of blending audio across it.
MAXIMUM_BLOCKS_OF_CATCH_UP = 4

NANOSECONDS_PER_SECOND = 1_000_000_000


@processor(execution="continuous", interval_ms=5, description="A sine-wave voice")
class ToneSource:
    """A phase-continuous sine wave, paced by the monotonic clock.

    The tick interval is deliberately shorter than one block's duration: each
    `process()` emits however many whole blocks elapsed time owes, which keeps
    the stream locked to real time at any rate and block size rather than to a
    tick the engine cannot express as an integer millisecond.
    """

    def __init__(
        self,
        frequency_hz: float = 440.0,
        sample_rate: int = 48_000,
        channels: int = 1,
        block_size: int = 512,
        amplitude: float = 0.3,
    ) -> None:
        self.frequency_hz = float(frequency_hz)
        self.sample_rate = int(sample_rate)
        self.channels = int(channels)
        self.block_size = int(block_size)
        self.amplitude = float(amplitude)

        # Refused here rather than left to surface downstream: a block_size of
        # zero never advances the emitted-sample counter, so `process()` would
        # publish empty blocks forever without ever catching up to the clock.
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
        ctx.outputs.write(
            VOICE_OUTPUT_PORT,
            {
                "samples": self._next_interleaved_sample_bytes(),
                "sample_rate": self.sample_rate,
                "channels": self.channels,
                "sample_count": self.block_size,
                "dtype": "f32",
                "first_sample_timestamp_ns": first_sample_timestamp_ns,
            },
            first_sample_timestamp_ns,
        )
        self.samples_emitted_in_this_run += self.block_size

    def _stamp_for_the_next_block(self) -> int:
        """The run's anchor plus the emitted-sample offset, in integer arithmetic.

        Never an accumulated per-block delta, which drifts at 44.1 kHz-family
        rates where a block's duration is not a whole number of nanoseconds.
        """
        assert self.run_anchor_timestamp_ns is not None
        return self.run_anchor_timestamp_ns + (
            self.samples_emitted_in_this_run * NANOSECONDS_PER_SECOND
        ) // self.sample_rate

    def _next_interleaved_sample_bytes(self) -> bytes:
        phase_step_radians = 2.0 * math.pi * self.frequency_hz / self.sample_rate
        phases = self.next_phase_radians + phase_step_radians * numpy.arange(
            self.block_size, dtype=numpy.float64
        )
        # Carried across blocks and wrapped, so a voice never clicks at a block
        # edge and the phase stays exact over a long run.
        self.next_phase_radians = math.fmod(
            self.next_phase_radians + phase_step_radians * self.block_size, 2.0 * math.pi
        )
        # Spelled little-endian at the source: the payload is little-endian by
        # contract, not by the platform's luck.
        one_channel = (self.amplitude * numpy.sin(phases)).astype("<f4")
        if self.channels == 1:
            return one_channel.tobytes()
        return numpy.repeat(one_channel, self.channels).tobytes()

    @output(description="This voice's sine wave, as AudioBlock bags")
    def voice_to_downstream(self) -> None: ...
