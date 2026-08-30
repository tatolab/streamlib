# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A Schroeder-Moorer reverb, authored as an ordinary Python processor.

The input port declares one window contract, so what `process()` receives is
always exactly 128 mono f32 samples at 48 kHz — whatever the capture device
actually opened at. That is what lets a delay-line algorithm exist in Python at
all: the filter lengths below are fixed at construction from a rate the
declaration guarantees, and there is no format negotiation, no resampler and no
rechunking in this file.

The contract also has to be `ordered`. A reverb is an accumulator whose output
depends on every sample that came before it; `newest` skips bags by design, and
a skipped block is a hole in a delay line.
"""

import numpy

from streamlib import (  # noqa: A004 — `input` is streamlib's port decorator
    AudioBlock,
    AudioWindowContract,
    RuntimeContextLimitedAccess,
    input,
    output,
    processor,
)

DRY_AUDIO_INPUT_PORT = "dry_audio_from_upstream"
REVERBERATED_AUDIO_OUTPUT_PORT = "reverberated_audio_to_downstream"

# 128 samples is 2.67 ms at 48 kHz — the framing latency this example adds to a
# live monitoring loop, and the one number here a reader should expect to tune.
# It is bounded above by the algorithm: see `ReverbDelayLine`.
REVERB_WINDOW = AudioWindowContract(
    sample_rate=48_000,
    channels=1,
    dtype="f32",
    window_size=128,
)

# Schroeder's parallel-comb / series-diffuser topology at the delay lengths
# Freeverb published. They are irregularly spaced on purpose: rounding them to
# a convenient common multiple — of the window size, say — lands their echoes
# on the same instants and rings audibly. So they are scaled to the contract's
# rate and then used wherever they land.
FREEVERB_REFERENCE_SAMPLE_RATE_HZ = 44_100
COMB_DELAYS_AT_THE_REFERENCE_RATE = (1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617)
DIFFUSER_DELAYS_AT_THE_REFERENCE_RATE = (556, 441, 341, 225)
DIFFUSER_FEEDBACK = 0.5

# Freeverb's input attenuation, kept as published. It is not a headroom
# guarantee: the eight combs each reach 1/(1 - feedback) at their own
# resonances, and the diffuser chain multiplies on top of that, so the wet
# path peaks near 2.5x around 5.77 kHz where the comb resonances align. What
# the clamp in `process()` is for.
GAIN_INTO_THE_COMB_BANK = 0.015

# `room_size` and `damping` are 0..1 dials; these map them onto the ranges the
# filters actually take. Feedback stays inside [0.7, 0.98] for every legal room
# size, so no setting of the dial can make a comb diverge.
ROOM_SIZE_TO_FEEDBACK_SCALE = 0.28
ROOM_SIZE_TO_FEEDBACK_OFFSET = 0.7
DAMPING_TO_FILTER_COEFFICIENT_SCALE = 0.4


def _delay_length_at_the_contracts_rate(delay_at_the_reference_rate: int) -> int:
    return round(
        delay_at_the_reference_rate
        * REVERB_WINDOW.sample_rate
        / FREEVERB_REFERENCE_SAMPLE_RATE_HZ
    )


class ReverbDelayLine:
    """A ring of past samples, read and written one whole window at a time.

    The delay must be at least one window, and that is a correctness condition
    rather than a preference. A window's positions are
    `(index + arange(window)) % delay`: while the delay is at least a window
    those are all distinct, so the samples read were written a delay ago and
    the ones written are read a delay from now, and the whole window computes
    in one vectorised pass. Below that the positions alias — the same slot
    appears twice — and the pass silently returns stale values where a sample
    should have fed back inside the window, with the scatter behind it going
    last-wins. It produces a plausible-looking wrong answer rather than
    failing, which is why this refuses. The shortest delay here is 245 samples,
    which is the real ceiling on `REVERB_WINDOW.window_size`.
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
        # Precomputed once: the modulo below runs on every window of every
        # filter, twelve times per `process()` at 375 windows a second.
        self.offsets_within_one_window = numpy.arange(window_size)
        self.index_of_the_next_window = 0

    def positions_of_the_next_window(self) -> "numpy.ndarray":
        """Where the next window sits in the ring, wrap included."""
        return (
            self.index_of_the_next_window + self.offsets_within_one_window
        ) % self.past_samples.size

    def advance_one_window(self) -> None:
        self.index_of_the_next_window = (
            self.index_of_the_next_window + self.offsets_within_one_window.size
        ) % self.past_samples.size


class ReverbCombFilter:
    """One of the eight parallel resonators that make the tail.

    What it hands back is the delayed signal; what it feeds back is that signal
    through a one-zero lowpass. The lowpass is the damping: each pass around
    the loop loses a little more of the top end, so a bright room and a dull
    one differ by one coefficient rather than by a second filter bank.

    Freeverb damps with a one-*pole* filter instead. This is a deliberate
    departure: a pole is a per-sample recursion on the filter's own output, and
    a window of those cannot be computed without stepping sample by sample —
    which is the whole vectorisation gone. A one-zero lowpass reads only the
    delayed signal, which is already known for the entire window, and has the
    same unity DC gain, so the room still gets duller as the tail decays.
    """

    def __init__(self, delay_in_samples: int, window_size: int) -> None:
        self.delay_line = ReverbDelayLine(delay_in_samples, window_size)
        # The sample immediately before this window, carried so the lowpass is
        # continuous across window boundaries rather than restarting at zero
        # 375 times a second — which would click.
        self.delayed_sample_before_this_window = numpy.float32(0.0)

    def add_one_window_to_the_tail(
        self,
        window: "numpy.ndarray",
        feedback: float,
        damping_coefficient: float,
    ) -> "numpy.ndarray":
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
    """One of the four series stages that smear the comb bank's echoes.

    Freeverb calls this its allpass and it is not one. A true allpass would be
    `(z^-N - g) / (1 - g·z^-N)`; this structure is
    `((1+g)·z^-N - 1) / (1 - g·z^-N)`, which is unity only at DC and rises to
    `(2+g)/(1+g)` — 1.67 at the feedback used here — toward Nyquist. Four of
    them in series peak near 7.7. That is worth knowing rather than inheriting
    the name's promise: it is where the wet path's headroom actually goes, and
    why `process()` clamps.

    What the stage is for is real regardless: dispersing the comb bank's
    echoes in time, which is the difference between a reverb and four flutter
    echoes.
    """

    def __init__(self, delay_in_samples: int, window_size: int) -> None:
        self.delay_line = ReverbDelayLine(delay_in_samples, window_size)

    def diffuse_one_window(self, window: "numpy.ndarray") -> "numpy.ndarray":
        positions = self.delay_line.positions_of_the_next_window()
        delayed = self.delay_line.past_samples[positions]
        self.delay_line.past_samples[positions] = window + delayed * DIFFUSER_FEEDBACK
        self.delay_line.advance_one_window()
        return delayed - window


@processor(description="Adds a reverb tail to the audio it is given")
class ReverbEffect:
    """Mixes a decaying tail under the audio that produced it."""

    def __init__(
        self,
        room_size: float = 0.7,
        damping: float = 0.5,
        wet_level: float = 0.25,
        dry_level: float = 0.7,
    ) -> None:
        for dial_name, value in (
            ("room_size", room_size),
            ("damping", damping),
            ("wet_level", wet_level),
            ("dry_level", dry_level),
        ):
            if not 0.0 <= float(value) <= 1.0:
                raise ValueError(
                    f"ReverbEffect was configured with {dial_name}={value} — every dial "
                    f"runs from 0.0 to 1.0"
                )

        self.comb_feedback = (
            float(room_size) * ROOM_SIZE_TO_FEEDBACK_SCALE + ROOM_SIZE_TO_FEEDBACK_OFFSET
        )
        self.damping_coefficient = float(damping) * DAMPING_TO_FILTER_COEFFICIENT_SCALE
        self.wet_level = float(wet_level)
        self.dry_level = float(dry_level)

        self.comb_filters = [
            ReverbCombFilter(
                _delay_length_at_the_contracts_rate(delay), REVERB_WINDOW.window_size
            )
            for delay in COMB_DELAYS_AT_THE_REFERENCE_RATE
        ]
        self.diffuser_filters = [
            ReverbDiffuserFilter(
                _delay_length_at_the_contracts_rate(delay), REVERB_WINDOW.window_size
            )
            for delay in DIFFUSER_DELAYS_AT_THE_REFERENCE_RATE
        ]

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        # Drained rather than read once: a window that is already whole should
        # not wait behind the next device quantum on a monitoring path, and
        # falling a window behind should cost latency once, not forever.
        while True:
            block = ctx.inputs.read(DRY_AUDIO_INPUT_PORT, into=AudioBlock)
            if block is None:
                return
            self._reverberate_one_window(ctx, block)

    def _reverberate_one_window(
        self, ctx: RuntimeContextLimitedAccess, block: AudioBlock
    ) -> None:
        # The contract guarantees mono, so the single channel is the signal.
        # `samples` is a read-only view over the bag's own bytes; the copy is
        # what makes the arithmetic below legal.
        dry = block.samples[:, 0].astype(numpy.float32)

        into_the_comb_bank = dry * GAIN_INTO_THE_COMB_BANK
        wet = numpy.zeros_like(dry)
        for comb_filter in self.comb_filters:
            wet += comb_filter.add_one_window_to_the_tail(
                into_the_comb_bank, self.comb_feedback, self.damping_coefficient
            )
        for diffuser_filter in self.diffuser_filters:
            wet = diffuser_filter.diffuse_one_window(wet)

        mixed = dry * self.dry_level + wet * self.wet_level
        # Not a formality. The wet path peaks near 2.5x at 5766 Hz, where the
        # comb resonances align and the diffuser chain multiplies on top, so a
        # full-scale sine sitting on that peak clamps 41% of its samples. Most
        # input stays well under it — 0.78 for a 1 kHz full-scale sine — which
        # is exactly why the peak is worth clamping rather than trusting.
        numpy.clip(mixed, -1.0, 1.0, out=mixed)

        # The tail rides later windows; these samples still cover the instants
        # the incoming ones did, so the block keeps the stamp it arrived with
        # and A/V sync downstream stays subtraction.
        ctx.outputs.write(
            REVERBERATED_AUDIO_OUTPUT_PORT,
            {
                # Little-endian at the wire, where the contract states it —
                # never the platform's native spelling.
                "samples": mixed.astype("<f4").tobytes(),
                "sample_rate": REVERB_WINDOW.sample_rate,
                "channels": REVERB_WINDOW.channels,
                "sample_count": REVERB_WINDOW.window_size,
                "dtype": REVERB_WINDOW.dtype,
                "first_sample_timestamp_ns": block.first_sample_timestamp_ns,
            },
            block.first_sample_timestamp_ns,
        )

    @input(
        delivery_profile="ordered",
        audio_window=REVERB_WINDOW,
        description="Whatever the microphone captured, converted to the reverb's window",
    )
    def dry_audio_from_upstream(self) -> None: ...

    @output(description="The input with its own tail mixed under it, as AudioBlock bags")
    def reverberated_audio_to_downstream(self) -> None: ...
