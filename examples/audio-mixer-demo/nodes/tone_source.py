# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""One voice of the chord: a sine wave published on a Channels port.

Importable as `nodes.tone_source:ToneSource`, which is the name the engine
spawns this node's child interpreter with. A voice publishes whatever rate,
channel count and block size its inputs name, a new rate starting a new run,
and knows nothing about its consumer — converting it is the consumer's window
contract's job.
"""

import math
from typing import Annotated, Callable

import numpy
from tatolab.stream import (
    Audio,
    Description,
    Input,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
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
    description="A phase-continuous sine wave at the rate and channel count its inputs name",
)
@node(execution="continuous", interval_ms=5)
def ToneSource(
    *,
    frequency_hz: Input[
        Annotated[float, NumberRange(minimum=0.0), Description("The tone's pitch, in hertz.")]
    ] = Input(440.0),
    sample_rate: Input[
        Annotated[
            int,
            NumberRange(minimum=1),
            Description("The samples a second this voice is written at."),
        ]
    ] = Input(48_000),
    channels: Input[
        Annotated[
            int,
            NumberRange(minimum=1),
            Description("How many channels carry the tone, each the same."),
        ]
    ] = Input(1),
    block_size: Input[
        Annotated[
            int,
            NumberRange(minimum=1),
            Description("The samples in each block this voice writes."),
        ]
    ] = Input(512),
    amplitude: Input[
        Annotated[
            float, NumberRange(minimum=0.0, maximum=1.0), Description("The tone's peak level.")
        ]
    ] = Input(0.3),
    voice_to_downstream: OutputPin[Annotated[Audio, Description("This voice's sine wave")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """A phase-continuous sine wave, paced by the monotonic clock.

    The tick is shorter than any block: each frame emits however many
    whole blocks elapsed time owes, so the stream stays locked to real time at
    any rate and block size.
    """

    run_anchor_timestamp_ns: int | None = None
    # The stream-wide index of this run's first sample. A new run carries
    # the samples it skipped, so the index keeps counting real time.
    start_sample_of_this_run = 0
    samples_emitted_in_this_run = 0
    # A run is at one rate: a new `sample_rate` starts a new run.
    rate_of_this_run = 0
    next_phase_radians = 0.0

    def samples_elapsed_time_owes(now_ns: int) -> int:
        assert run_anchor_timestamp_ns is not None
        elapsed_ns = now_ns - run_anchor_timestamp_ns
        return (elapsed_ns * rate_of_this_run) // NANOSECONDS_PER_SECOND

    def publish_one_block() -> None:
        nonlocal samples_emitted_in_this_run
        first_sample_timestamp_ns = stamp_for_the_next_block()
        # The rate is an input the pin's type cannot state, so the block names it.
        voice_to_downstream.write(
            next_planar_samples(),
            timestamp_ns=first_sample_timestamp_ns,
            start_sample=start_sample_of_this_run + samples_emitted_in_this_run,
            sample_rate=rate_of_this_run,
        )
        samples_emitted_in_this_run += block_size.get()

    def stamp_for_the_next_block() -> int:
        """The run's anchor plus the emitted-sample offset, in integer arithmetic.

        Never an accumulated per-block delta, which drifts at 44.1 kHz-family
        rates where a block's duration is not a whole number of nanoseconds.
        """
        assert run_anchor_timestamp_ns is not None
        return (
            run_anchor_timestamp_ns
            + (samples_emitted_in_this_run * NANOSECONDS_PER_SECOND)
            // rate_of_this_run
        )

    def next_planar_samples() -> numpy.ndarray:
        """The next block, `[channel][sample]`, every channel the same tone."""
        nonlocal next_phase_radians
        phase_step_radians = 2.0 * math.pi * frequency_hz.get() / rate_of_this_run
        phases = next_phase_radians + phase_step_radians * numpy.arange(
            block_size.get(), dtype=numpy.float64
        )
        # Carried across blocks and wrapped, so a voice never clicks at a block
        # edge and the phase stays exact over a long run.
        next_phase_radians = math.fmod(
            next_phase_radians + phase_step_radians * block_size.get(),
            2.0 * math.pi,
        )
        one_channel = (amplitude.get() * numpy.sin(phases)).astype(numpy.float32)
        return numpy.ascontiguousarray(
            numpy.broadcast_to(one_channel, (channels.get(), block_size.get()))
        )

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal run_anchor_timestamp_ns, samples_emitted_in_this_run, start_sample_of_this_run
        nonlocal rate_of_this_run
        now_ns = ctx.time
        if run_anchor_timestamp_ns is None or sample_rate.get() != rate_of_this_run:
            start_sample_of_this_run += samples_emitted_in_this_run
            samples_emitted_in_this_run = 0
            run_anchor_timestamp_ns = now_ns
            rate_of_this_run = sample_rate.get()

        samples_owed = samples_elapsed_time_owes(now_ns)
        if samples_owed - samples_emitted_in_this_run > (
            MAXIMUM_BLOCKS_OF_CATCH_UP * block_size.get()
        ):
            log.warn(
                "fell far enough behind real time to start a new run; the gap is "
                "left in the timestamps rather than filled with invented samples",
                samples_behind=samples_owed - samples_emitted_in_this_run,
                frequency_hz=frequency_hz.get(),
            )
            start_sample_of_this_run += samples_owed
            run_anchor_timestamp_ns = now_ns
            samples_emitted_in_this_run = 0
            samples_owed = 0

        while samples_emitted_in_this_run + block_size.get() <= samples_owed:
            publish_one_block()

    return each_frame
