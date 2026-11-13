# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`Oscillator`: a steady tone published on a Channels port."""

from __future__ import annotations

import math
from dataclasses import dataclass
from typing import Literal

import numpy
from tatolab.stream import Channels, Output, RuntimeContextLimitedAccess, from_config, log, node

OSCILLATOR_OUTPUT_PORT = "audio"

# How far behind real time an oscillator may fall before it starts a new run
# rather than emitting the whole backlog: a burst that large outruns any mailbox.
MAXIMUM_BLOCKS_OF_CATCH_UP = 4

NANOSECONDS_PER_SECOND = 1_000_000_000


@dataclass
class OscillatorConfig:
    """An `Oscillator`'s tone and the shape of the audio it publishes."""

    frequency_hz: float = 220.0
    waveform: Literal["sine", "square", "saw", "triangle"] = "sine"
    amplitude: float = 0.2
    sample_rate: int = 48_000
    channels: int = 2
    block_size: int = 512


def one_cycle_of_the_waveform(waveform: str, phases_in_cycles: numpy.ndarray) -> numpy.ndarray:
    """`waveform` between -1 and 1 at each of `phases_in_cycles`, a cycle being 0 to 1."""
    within_the_cycle = phases_in_cycles % 1.0
    if waveform == "sine":
        return numpy.sin(2.0 * math.pi * within_the_cycle)
    if waveform == "square":
        return numpy.where(within_the_cycle < 0.5, 1.0, -1.0)
    if waveform == "saw":
        return 2.0 * within_the_cycle - 1.0
    if waveform == "triangle":
        return 1.0 - 4.0 * numpy.abs(within_the_cycle - 0.5)
    raise ValueError(
        f"Oscillator's waveform is `{waveform}`. To fix: set it to sine, square, saw or triangle"
    )


@node.process(
    execution="continuous",
    interval_ms=5,
    description="A steady tone: a sine, square, saw or triangle wave at the pitch you set. Wire it to a Speaker to hear it, or into Program to see its level.",
    category="Audio",
)
class Oscillator:
    """A phase-continuous tone, paced by the stream's clock.

    The tick is shorter than any block: each `process()` emits however many
    whole blocks elapsed time owes, so the tone stays locked to real time.
    """

    audio = Output(
        Channels,
        rate=from_config("sample_rate"),
        dtype="f32",
        description="The tone, the same on every channel",
    )

    def __init__(self, config: OscillatorConfig) -> None:
        self.frequency_hz = float(config.frequency_hz)
        self.waveform = config.waveform
        self.amplitude = float(config.amplitude)
        self.sample_rate = int(config.sample_rate)
        self.channels = int(config.channels)
        self.block_size = int(config.block_size)
        one_cycle_of_the_waveform(self.waveform, numpy.zeros(1))

        # A block_size of zero never advances the emitted-sample counter, so
        # `process()` would publish empty blocks forever.
        for field_name, value in (
            ("sample_rate", self.sample_rate),
            ("channels", self.channels),
            ("block_size", self.block_size),
        ):
            if value <= 0:
                raise ValueError(
                    f"Oscillator's {field_name} is {value}. To fix: set it to 1 or more"
                )

        self.run_anchor_timestamp_ns: int | None = None
        # The stream-wide index of this run's first sample. A new run carries
        # the samples it skipped, so the index keeps counting real time.
        self.start_sample_of_this_run = 0
        self.samples_emitted_in_this_run = 0
        self.next_phase_in_cycles = 0.0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        now_ns = ctx.time
        if self.run_anchor_timestamp_ns is None:
            self.run_anchor_timestamp_ns = now_ns

        samples_owed = (
            (now_ns - self.run_anchor_timestamp_ns) * self.sample_rate
        ) // NANOSECONDS_PER_SECOND
        if samples_owed - self.samples_emitted_in_this_run > (
            MAXIMUM_BLOCKS_OF_CATCH_UP * self.block_size
        ):
            log.warn(
                "fell far enough behind real time to start a new run; the gap is "
                "left in the timestamps rather than filled with invented samples",
                samples_behind=samples_owed - self.samples_emitted_in_this_run,
            )
            self.start_sample_of_this_run += samples_owed
            self.run_anchor_timestamp_ns = now_ns
            self.samples_emitted_in_this_run = 0
            samples_owed = 0

        while self.samples_emitted_in_this_run + self.block_size <= samples_owed:
            self._publish_one_block(ctx)

    def _publish_one_block(self, ctx: RuntimeContextLimitedAccess) -> None:
        assert self.run_anchor_timestamp_ns is not None
        # The anchor plus the emitted-sample offset in integer arithmetic, never
        # an accumulated per-block delta, which drifts at 44.1 kHz-family rates.
        first_sample_timestamp_ns = (
            self.run_anchor_timestamp_ns
            + (self.samples_emitted_in_this_run * NANOSECONDS_PER_SECOND) // self.sample_rate
        )
        ctx.outputs.write_channels_block(
            OSCILLATOR_OUTPUT_PORT,
            self._next_planar_samples(),
            start_sample=self.start_sample_of_this_run + self.samples_emitted_in_this_run,
            sample_rate=self.sample_rate,
            timestamp_ns=first_sample_timestamp_ns,
        )
        self.samples_emitted_in_this_run += self.block_size

    def _next_planar_samples(self) -> numpy.ndarray:
        """The next block, `[channel][sample]`, every channel the same tone."""
        phase_step_in_cycles = self.frequency_hz / self.sample_rate
        phases = self.next_phase_in_cycles + phase_step_in_cycles * numpy.arange(
            self.block_size, dtype=numpy.float64
        )
        # Carried across blocks and wrapped, so the tone never clicks at a block edge.
        self.next_phase_in_cycles = (
            self.next_phase_in_cycles + phase_step_in_cycles * self.block_size
        ) % 1.0
        one_channel = (self.amplitude * one_cycle_of_the_waveform(self.waveform, phases)).astype(
            numpy.float32
        )
        return numpy.ascontiguousarray(numpy.broadcast_to(one_channel, (self.channels, self.block_size)))
