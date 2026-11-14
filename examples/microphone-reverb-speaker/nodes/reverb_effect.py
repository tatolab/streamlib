# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Schroeder-Moorer reverb, authored as an ordinary Python node.

Importable as `nodes.reverb_effect:ReverbEffect`, which is the name the engine
spawns this node's child interpreter with. Its Channels input declares one window
contract, so every block `process()` reads is exactly 128 mono f32 samples at
48 kHz, whatever the capture device opened at: the filter lengths are fixed at
construction, and there is no resampler or rechunker in this file.
"""

from dataclasses import dataclass
from typing import Final

import numpy
from tatolab.stream import (
    AudioWindowContract,
    Channels,
    ChannelsBlock,
    Input,
    Output,
    RuntimeContextLimitedAccess,
    node,
)

# 128 samples is 2.67 ms at 48 kHz: the framing latency this node adds to a
# live monitoring loop. Every delay line must be at least one window long, so
# the shortest one (245 samples) is its ceiling.
REVERB_WINDOW_CHANNELS: Final = 1
REVERB_WINDOW_DTYPE: Final = "f32"
REVERB_WINDOW = AudioWindowContract(
    sample_rate=48_000,
    channels=REVERB_WINDOW_CHANNELS,
    dtype=REVERB_WINDOW_DTYPE,
    window_size=128,
)

# Freeverb's published delay lengths, scaled to the contract's rate. They are
# irregular on purpose: rounding them to a common multiple lands their echoes
# on the same instants and rings audibly.
FREEVERB_REFERENCE_SAMPLE_RATE_HZ = 44_100
COMB_DELAYS_AT_THE_REFERENCE_RATE = (1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617)
DIFFUSER_DELAYS_AT_THE_REFERENCE_RATE = (556, 441, 341, 225)
DIFFUSER_FEEDBACK = 0.5

# Freeverb's input attenuation. It is not a headroom guarantee: the wet path
# peaks near 2.5x around 5.77 kHz, where the comb resonances align and the
# diffuser chain multiplies on top, which is what the clamp in `process()` is for.
GAIN_INTO_THE_COMB_BANK = 0.015

# Map the 0..1 dials onto the ranges the filters take. Feedback stays inside
# [0.7, 0.98] for every legal room size, so no setting makes a comb diverge.
ROOM_SIZE_TO_FEEDBACK_SCALE = 0.28
ROOM_SIZE_TO_FEEDBACK_OFFSET = 0.7
DAMPING_TO_FILTER_COEFFICIENT_SCALE = 0.4


def delay_length_at_the_reverb_window_rate(delay_at_the_reference_rate: int) -> int:
    """A Freeverb delay length rescaled from 44.1 kHz to the window contract's rate."""
    return round(
        delay_at_the_reference_rate
        * REVERB_WINDOW.sample_rate
        / FREEVERB_REFERENCE_SAMPLE_RATE_HZ
    )


class ReverbDelayLine:
    """A ring of past samples, read and written one whole window at a time.

    A delay shorter than one window is refused: the window's ring positions
    would alias, and the vectorised pass would silently read samples that should
    have fed back inside the window — plausible audio, wrong answer.
    """

    def __init__(self, delay_in_samples: int, window_size: int) -> None:
        if delay_in_samples < window_size:
            raise ValueError(
                f"ReverbDelayLine was given a {delay_in_samples}-sample delay for a "
                f"{window_size}-sample window — a delay must be at least one window, "
                f"or the window's ring positions alias and the vectorised pass reads "
                f"samples that should have fed back inside it"
            )
        self.past_samples = numpy.zeros(delay_in_samples, dtype=numpy.float32)
        self.offsets_within_one_window = numpy.arange(window_size)
        self.index_of_the_next_window = 0

    def positions_of_the_next_window(self) -> numpy.ndarray:
        """Where the next window sits in the ring, wrap included."""
        return (
            self.index_of_the_next_window + self.offsets_within_one_window
        ) % self.past_samples.size

    def advance_one_window(self) -> None:
        """Move the ring's write position on by one window."""
        self.index_of_the_next_window = (
            self.index_of_the_next_window + self.offsets_within_one_window.size
        ) % self.past_samples.size


class ReverbCombFilter:
    """One of the eight parallel resonators that make the tail.

    It damps its feedback with a one-zero lowpass where Freeverb uses a
    one-pole: a pole recurses on its own output sample by sample, which a
    window-at-a-time pass cannot compute, while a one-zero reads only the
    delayed signal and keeps the same unity DC gain.
    """

    def __init__(self, delay_in_samples: int, window_size: int) -> None:
        self.delay_line = ReverbDelayLine(delay_in_samples, window_size)
        # Carried across windows so the lowpass does not restart at zero on every
        # window boundary, which would click.
        self.delayed_sample_before_this_window = numpy.float32(0.0)

    def add_one_window_to_the_tail(
        self,
        window: numpy.ndarray,
        feedback: float,
        damping_coefficient: float,
    ) -> numpy.ndarray:
        """Feed one window into the resonator and return what it delayed."""
        positions = self.delay_line.positions_of_the_next_window()
        delayed = self.delay_line.past_samples[positions]

        one_sample_earlier = numpy.empty_like(delayed)
        one_sample_earlier[0] = self.delayed_sample_before_this_window
        one_sample_earlier[1:] = delayed[:-1]
        self.delayed_sample_before_this_window = delayed[-1]
        damped = (
            1.0 - damping_coefficient
        ) * delayed + damping_coefficient * one_sample_earlier

        self.delay_line.past_samples[positions] = window + damped * feedback
        self.delay_line.advance_one_window()
        return delayed


class ReverbDiffuserFilter:
    """One of the four series stages that disperse the comb bank's echoes in time.

    Freeverb calls this an allpass; it is `((1+g)·z^-N - 1) / (1 - g·z^-N)`,
    unity at DC and rising to `(2+g)/(1+g)` toward Nyquist, so four in series
    are where the wet path's headroom goes.
    """

    def __init__(self, delay_in_samples: int, window_size: int) -> None:
        self.delay_line = ReverbDelayLine(delay_in_samples, window_size)

    def diffuse_one_window(self, window: numpy.ndarray) -> numpy.ndarray:
        """Pass one window through the stage."""
        positions = self.delay_line.positions_of_the_next_window()
        delayed = self.delay_line.past_samples[positions]
        self.delay_line.past_samples[positions] = window + delayed * DIFFUSER_FEEDBACK
        self.delay_line.advance_one_window()
        return delayed - window


@dataclass
class ReverbEffectConfig:
    """`ReverbEffect`'s four dials, each from 0.0 to 1.0."""

    room_size: float = 0.7
    damping: float = 0.5
    wet_level: float = 0.25
    dry_level: float = 0.7


@node.meta(description="Adds a reverb tail to the audio it is given, mixed under the original")
@node.process
class ReverbEffect:
    """Mono audio in, the same audio with its own reverb tail mixed under it out."""

    dry_audio_from_upstream = Input(
        Channels,
        delivery="ordered",
        window=REVERB_WINDOW,
        description="Whatever the microphone captured, converted to the reverb's window",
    )
    reverberated_audio_to_downstream = Output(
        Channels,
        rate=REVERB_WINDOW.sample_rate,
        dtype=REVERB_WINDOW_DTYPE,
        description="The input with its own tail mixed under it",
    )

    def __init__(self, config: ReverbEffectConfig) -> None:
        for dial_name, value in (
            ("room_size", config.room_size),
            ("damping", config.damping),
            ("wet_level", config.wet_level),
            ("dry_level", config.dry_level),
        ):
            if not 0.0 <= float(value) <= 1.0:
                raise ValueError(
                    f"ReverbEffect was configured with {dial_name}={value} — every dial "
                    f"runs from 0.0 to 1.0"
                )

        self.comb_feedback = (
            float(config.room_size) * ROOM_SIZE_TO_FEEDBACK_SCALE
            + ROOM_SIZE_TO_FEEDBACK_OFFSET
        )
        self.damping_coefficient = (
            float(config.damping) * DAMPING_TO_FILTER_COEFFICIENT_SCALE
        )
        self.wet_level = float(config.wet_level)
        self.dry_level = float(config.dry_level)

        self.comb_filters = [
            ReverbCombFilter(
                delay_length_at_the_reverb_window_rate(delay), REVERB_WINDOW.window_size
            )
            for delay in COMB_DELAYS_AT_THE_REFERENCE_RATE
        ]
        self.diffuser_filters = [
            ReverbDiffuserFilter(
                delay_length_at_the_reverb_window_rate(delay), REVERB_WINDOW.window_size
            )
            for delay in DIFFUSER_DELAYS_AT_THE_REFERENCE_RATE
        ]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # Drained rather than read once, so a window that is already whole never
        # waits behind the next device quantum, and falling behind costs latency
        # once rather than forever.
        while True:
            dry_block = ctx.inputs.read(self.dry_audio_from_upstream)
            if dry_block is None:
                return
            self.reverberate_one_window(ctx, dry_block)

    def reverberate_one_window(
        self, ctx: RuntimeContextLimitedAccess, dry_block: ChannelsBlock
    ) -> None:
        """Mix one window's tail under it and write the result downstream."""
        # The contract guarantees mono f32, `[channel][sample]`. The view borrows
        # the message's samples, which the arithmetic below only ever reads.
        dry = numpy.from_dlpack(dry_block)[0]

        into_the_comb_bank = dry * GAIN_INTO_THE_COMB_BANK
        wet = numpy.zeros_like(dry)
        for comb_filter in self.comb_filters:
            wet += comb_filter.add_one_window_to_the_tail(
                into_the_comb_bank, self.comb_feedback, self.damping_coefficient
            )
        for diffuser_filter in self.diffuser_filters:
            wet = diffuser_filter.diffuse_one_window(wet)

        mixed = dry * self.dry_level + wet * self.wet_level
        numpy.clip(mixed, -1.0, 1.0, out=mixed)

        # The tail rides later windows; these samples cover the instants the
        # incoming ones did, so the block keeps the stamp and index it arrived with.
        ctx.outputs.write(
            self.reverberated_audio_to_downstream,
            numpy.ascontiguousarray(mixed.astype(numpy.float32, copy=False)[numpy.newaxis, :]),
            timestamp_ns=dry_block.timestamp_ns,
            start_sample=dry_block.start_sample,
        )
