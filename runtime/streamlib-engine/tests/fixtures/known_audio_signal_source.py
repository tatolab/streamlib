# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Publishes the known signal as `AudioBlock` bags, for a speaker to play.

The signal itself is `known_audio_signal.generate_signal()` — the same samples
`e2e_audio_loopback.sh` plays through `pw-play`, so the two fixtures measure
one reference and a difference between them is StreamLib's, not the signal's.
Generated rather than read back from a WAV so no quantisation sits between the
reference and what is played.

The format is the fixture sink's: 48 kHz stereo `f32`. It is stated rather than
discovered because there is no resampler on this rung and `SpeakerSink` refuses
what its device cannot play — so a mismatch has to be a named failure in the
run's log, not something this quietly adapts to.

Paced to stay a bounded lead ahead of the monotonic clock rather than published
as fast as the loop runs. A lead is what absorbs a scheduling hiccup, and the
bound is what keeps the producer from racing: a burst larger than the consumer's
mailbox is lost there — `PortMailbox::push` drops its oldest to make room, which
a port's `lossless` profile does not prevent — and the lost audio is a hole the
analysis would then report as this fixture's failure rather than the transport's.

The lead is monotonic by construction: it compares the duration of what has been
published against elapsed monotonic time, so it never reads a wall clock and
never sleeps.
"""

import numpy

import known_audio_signal
from streamlib import RuntimeContextLimitedAccess, monotonic_now_ns, output, processor

# The fixture sink is created with `audio.position=[FL FR]`, and the PipeWire
# arm asks for `F32_LE`.
SAMPLE_RATE = known_audio_signal.SAMPLE_RATE
CHANNELS = 2
DTYPE = "f32"

# 10 ms at 48 kHz. Independent of the device's own quantum: the speaker's ring
# re-slices whatever arrives into device periods, so a block is only a unit of
# publishing.
SAMPLES_PER_BLOCK = SAMPLE_RATE // 100

# How far ahead of real time the publishing runs. Enough that a late `process()`
# call costs the speaker nothing, and well inside the sixteen-block mailbox the
# consumer's port carries, so no block is dropped between the two.
PUBLISHING_LEAD_NS = 100_000_000

# Silence after the signal, so the capture has an unambiguous tail to end on
# rather than the loop's next lead-in.
TRAILING_SILENCE_SECONDS = 1.0


def _interleaved_stereo_f32_bytes(mono_samples):
    """The same mono signal in both channels, interleaved little-endian."""
    stereo = numpy.repeat(mono_samples.astype("<f4"), CHANNELS)
    return stereo.tobytes()


@processor(execution="continuous", interval_ms=1)
class KnownAudioSignalSource:
    """Plays the known signal once, then silence."""

    @output()
    def audio(self) -> None: ...

    def __init__(self) -> None:
        signal = known_audio_signal.generate_signal()
        trailing_silence = numpy.zeros(
            int(TRAILING_SILENCE_SECONDS * SAMPLE_RATE), dtype="<f8"
        )
        self._signal = numpy.concatenate([signal, trailing_silence])
        self._samples_published = 0
        self._first_sample_timestamp_ns = None

    def _is_far_enough_ahead(self) -> bool:
        published_ns = self._samples_published * 1_000_000_000 // SAMPLE_RATE
        elapsed_ns = monotonic_now_ns() - self._first_sample_timestamp_ns
        return published_ns - elapsed_ns > PUBLISHING_LEAD_NS

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self._samples_published >= len(self._signal):
            return
        if self._first_sample_timestamp_ns is None:
            self._first_sample_timestamp_ns = monotonic_now_ns()
        elif self._is_far_enough_ahead():
            return

        at = self._samples_published
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
                    + at * 1_000_000_000 // SAMPLE_RATE
                ),
            },
        )
        self._samples_published += len(block)
