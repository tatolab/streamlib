# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`Oscillator`: a steady tone published on a Channels port."""

from __future__ import annotations

import math
from typing import Annotated, Callable, Literal

import numpy
from tatolab.stream import (
    Audio,
    Description,
    FormerlyNamed,
    Input,
    NumberRange,
    NumberUnit,
    OutputPin,
    PinMeaning,
    RuntimeContextLimitedAccess,
    log,
    node,
)

# How far behind real time an oscillator may fall before it starts a new run
# rather than emitting the whole backlog: a burst that large outruns any mailbox.
MAXIMUM_BLOCKS_OF_CATCH_UP = 4

#: The least an `Oscillator` catches up on before it starts a new run: several of its ticks,
#: so a block shorter than a tick still owes less than the limit at every tick.
LEAST_CATCH_UP_MS = 100

NANOSECONDS_PER_SECOND = 1_000_000_000


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


@node.meta(
    category="Audio",
    description="A steady tone: a sine, square, saw or triangle wave at the pitch you set. Wire it into the Audio Video Preview, or to a Speaker, to hear it.",
)
@node(execution="continuous", interval_ms=5)
def Oscillator(
    *,
    frequency: Input[
        Annotated[
            float,
            NumberUnit("Hz"),
            PinMeaning("frequency"),
            NumberRange(minimum=0.0),
            FormerlyNamed("frequency_hz"),
        ]
    ] = Input(220.0),
    waveform: Input[Literal["sine", "square", "saw", "triangle"]] = Input("sine"),
    amplitude: Input[Annotated[float, NumberRange(minimum=0.0, maximum=1.0)]] = Input(0.2),
    sample_rate: Input[Annotated[int, NumberUnit("Hz"), NumberRange(minimum=1)]] = Input(48_000),
    channels: Input[Annotated[int, NumberRange(minimum=1)]] = Input(2),
    block_size: Input[Annotated[int, NumberRange(minimum=1)]] = Input(512),
    audio: OutputPin[Annotated[Audio, Description("The tone, the same on every channel")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """A phase-continuous tone, paced by the stream's clock.

    The tick is shorter than any block: each frame emits however many whole blocks elapsed
    time owes, so the tone stays locked to real time. Its pitch, waveform and loudness follow
    their inputs from the next block on, and so do its channels and block size; a change of its
    rate starts a new run at that rate from the tick it is seen on.
    """
    run_anchor_timestamp_ns: "int | None" = None
    # The stream-wide index of this run's first sample. A new run carries the samples it
    # skipped, so the index keeps counting real time.
    start_sample_of_this_run = 0
    samples_emitted_in_this_run = 0
    next_phase_in_cycles = 0.0
    # The rate this run counts its samples at; none until the first frame reads it.
    sample_rate_of_this_run: "int | None" = None

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal run_anchor_timestamp_ns, start_sample_of_this_run, samples_emitted_in_this_run
        nonlocal sample_rate_of_this_run
        now_ns = ctx.time
        if run_anchor_timestamp_ns is None:
            run_anchor_timestamp_ns = now_ns
        if sample_rate.get() != sample_rate_of_this_run:
            # Counted at the old rate, the samples owed would leave a backlog or a silence.
            start_sample_of_this_run += samples_emitted_in_this_run
            run_anchor_timestamp_ns = now_ns
            samples_emitted_in_this_run = 0
            sample_rate_of_this_run = sample_rate.get()

        rate, block = sample_rate.get(), block_size.get()
        samples_owed = ((now_ns - run_anchor_timestamp_ns) * rate) // NANOSECONDS_PER_SECOND
        if samples_owed - samples_emitted_in_this_run > max(
            MAXIMUM_BLOCKS_OF_CATCH_UP * block, rate * LEAST_CATCH_UP_MS // 1000
        ):
            log.warn(
                "fell far enough behind real time to start a new run; the gap is "
                "left in the timestamps rather than filled with invented samples",
                samples_behind=samples_owed - samples_emitted_in_this_run,
            )
            start_sample_of_this_run += samples_owed
            run_anchor_timestamp_ns = now_ns
            samples_emitted_in_this_run = 0
            samples_owed = 0

        while samples_emitted_in_this_run + block <= samples_owed:
            publish_one_block(run_anchor_timestamp_ns, rate, block)

    def publish_one_block(anchor_ns: int, rate: int, block: int) -> None:
        nonlocal samples_emitted_in_this_run
        # The anchor plus the emitted-sample offset in integer arithmetic, never an accumulated
        # per-block delta, which drifts at 44.1 kHz-family rates.
        first_sample_timestamp_ns = anchor_ns + (
            samples_emitted_in_this_run * NANOSECONDS_PER_SECOND
        ) // rate
        audio.write(
            next_planar_samples(rate, block),
            first_sample_timestamp_ns,
            start_sample=start_sample_of_this_run + samples_emitted_in_this_run,
            sample_rate=rate,
        )
        samples_emitted_in_this_run += block

    def next_planar_samples(rate: int, block: int) -> numpy.ndarray:
        """The next block, `[channel][sample]`, every channel the same tone."""
        nonlocal next_phase_in_cycles
        phase_step_in_cycles = frequency.get() / rate
        phases = next_phase_in_cycles + phase_step_in_cycles * numpy.arange(
            block, dtype=numpy.float64
        )
        # Carried across blocks and wrapped, so the tone never clicks at a block edge.
        next_phase_in_cycles = (next_phase_in_cycles + phase_step_in_cycles * block) % 1.0
        one_channel = (
            amplitude.get() * one_cycle_of_the_waveform(waveform.get(), phases)
        ).astype(numpy.float32)
        return numpy.ascontiguousarray(numpy.broadcast_to(one_channel, (channels.get(), block)))

    return each_frame
