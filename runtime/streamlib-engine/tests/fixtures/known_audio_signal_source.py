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

Published faster than real time on purpose. The speaker's ring fills, the drain
thread waits for room, its `lossless` input stops being read, and this
processor's own write blocks — so the device paces the whole chain and the
signal reaches it gapless. Publishing at wall-clock cadence instead would leave
every scheduling hiccup as a hole in the audio, which is exactly what the
analysis is built to catch.
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

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if self._samples_published >= len(self._signal):
            return
        if self._first_sample_timestamp_ns is None:
            self._first_sample_timestamp_ns = monotonic_now_ns()

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
