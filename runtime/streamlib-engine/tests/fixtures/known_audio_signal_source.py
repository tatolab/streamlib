# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Publishes the known signal as `AudioBlock` bags, for a speaker to play.

The signal itself is `known_audio_signal.generate_signal()` — the same samples
`e2e_audio_loopback.sh` plays through `pw-play` or `afplay`, so the two
fixtures measure one reference and a difference between them is StreamLib's,
not the signal's. Generated rather than read back from a WAV so no
quantisation sits between the reference and what is played.

The format is the fixture sink's — PipeWire's null sink, or a Mac's built-in
speakers: 48 kHz stereo `f32`. It is stated rather than
discovered so the measurement stays about the transport: `SpeakerSink` now
declares `audio_window = match_device`, so a mismatch would be resampled into
the device's format instead of failing — and a comparison against the reference
would then be measuring the resampler rather than the path under test.

Paced to stay a bounded lead ahead of the monotonic clock rather than published
as fast as the loop runs. A lead is what absorbs a scheduling hiccup, and the
bound is what keeps the producer from racing: a burst larger than the consumer's
mailbox is lost there — `PortMailbox::push_frame_from_inbound_link` evicts its
oldest to make room, which no delivery profile prevents — and the lost audio is a hole the
analysis would then report as this fixture's failure rather than the transport's.

The lead is monotonic by construction: it compares the duration of what has been
published against elapsed monotonic time, so it never reads a wall clock and
never sleeps.
"""

import os

import numpy

import known_audio_signal
from streamlib import RuntimeContextLimitedAccess, monotonic_now_ns, output, processor

# PipeWire's fixture sink is created with `audio.position=[FL FR]` and its arm
# asks for `F32_LE`; a Mac's built-in speakers are two channels, and the
# CoreAudio arm opens every device as interleaved `f32`.
SAMPLE_RATE = known_audio_signal.SAMPLE_RATE
CHANNELS = 2
DTYPE = "f32"

# 10 ms at 48 kHz. Independent of the device's own quantum: the speaker's ring
# re-slices whatever arrives into device periods, so a block is only a unit of
# publishing.
SAMPLES_PER_BLOCK = SAMPLE_RATE // 100

# How far ahead of real time the publishing runs. Enough that a late `process()`
# call costs the speaker nothing, and well inside the depth the consumer's
# windowed port is sized to from its own contract, so no block is dropped
# between the two.
PUBLISHING_LEAD_NS = 100_000_000

# Silence after the signal, so the capture has an unambiguous tail to end on
# rather than the loop's next lead-in.
TRAILING_SILENCE_SECONDS = 1.0

# Whether the signal plays once or over and over. Off by default, because every
# analysis arm scores one pass of a finite waveform and a second pass would run
# past the window it records. On for an arm that has to still be publishing
# when something downstream gets round to looking — a `tap`, whose window opens
# whenever its caller asks rather than when the signal starts.
THE_SIGNAL_PLAYS_OVER_AND_OVER = os.environ.get("STREAMLIB_KNOWN_SIGNAL_REPEATS") == "1"

# One of `known_audio_signal`'s injectable faults, published in place of the
# clean signal, so a through-engine run can be seen going red.
FAULT_INJECTED_INTO_THE_SIGNAL = os.environ.get("STREAMLIB_KNOWN_SIGNAL_INJECT") or None


def _interleaved_stereo_f32_bytes(mono_samples):
    """The same mono signal in both channels, interleaved little-endian."""
    stereo = numpy.repeat(mono_samples.astype("<f4"), CHANNELS)
    return stereo.tobytes()


@processor(execution="continuous", interval_ms=1)
class KnownAudioSignalSource:
    """Plays the known signal once, then silence — or over and over."""

    @output()
    def audio(self) -> None: ...

    def __init__(self) -> None:
        signal = known_audio_signal.generate_signal()
        if FAULT_INJECTED_INTO_THE_SIGNAL:
            signal = known_audio_signal.signal_with_injected_fault(
                signal, FAULT_INJECTED_INTO_THE_SIGNAL
            )
        trailing_silence = numpy.zeros(
            int(TRAILING_SILENCE_SECONDS * SAMPLE_RATE), dtype="<f8"
        )
        self._signal = numpy.concatenate([signal, trailing_silence])
        self._plays_over_and_over = THE_SIGNAL_PLAYS_OVER_AND_OVER
        self._samples_published = 0
        self._first_sample_timestamp_ns = None

    def _is_far_enough_ahead(self) -> bool:
        published_ns = self._samples_published * 1_000_000_000 // SAMPLE_RATE
        elapsed_ns = monotonic_now_ns() - self._first_sample_timestamp_ns
        return published_ns - elapsed_ns > PUBLISHING_LEAD_NS

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if not self._plays_over_and_over and self._samples_published >= len(
            self._signal
        ):
            return
        if self._first_sample_timestamp_ns is None:
            self._first_sample_timestamp_ns = monotonic_now_ns()
        elif self._is_far_enough_ahead():
            return

        # The stamp counts every sample ever published and the waveform wraps
        # under it, so a repeat is one continuous stream rather than a new one
        # starting late: re-anchoring each pass on `now` would write the
        # publishing lead into the stamps as a gap. The signal's length is a
        # whole number of blocks, so a pass ends exactly on a block boundary
        # and no block spans the wrap.
        at = self._samples_published % len(self._signal)
        block = self._signal[at : at + SAMPLES_PER_BLOCK]
        # Derived from the samples before it rather than read fresh, so the
        # stamps describe one gapless stream even though the publishing runs
        # ahead of the device.
        ctx.outputs.write(
            "audio",
            {
                "samples": _interleaved_stereo_f32_bytes(block),
                "sample_rate": SAMPLE_RATE,
                "channels": CHANNELS,
                "sample_count": len(block),
                "dtype": DTYPE,
                "first_sample_timestamp_ns": (
                    self._first_sample_timestamp_ns
                    + self._samples_published * 1_000_000_000 // SAMPLE_RATE
                ),
            },
        )
        self._samples_published += len(block)
