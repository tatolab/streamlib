# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stream that encodes the known signal to Opus and captures what decodes back.

`KnownAudioSignalSource -> OpusEncoder -> OpusDecoder ->
CapturedAudioWaveformRecorder`. No audio device is in the path at all: where
`audio_loopback_stream.py` measures the transport by playing into a sink and
capturing its monitor, this measures the codec by keeping the whole loop
inside the graph. So a failure here with the loopback arm green is the codec's,
and a failure of both is the engine's or the rig's.

The source publishes 48 kHz stereo `f32`, which is exactly what the encoder's
window contract asks the stage to resample to — stated rather than discovered
so nothing between the reference and the measurement is a resampler and the
comparison stays about the codec. The stage still does the framing: the source
publishes 480-sample blocks and the encoder's port declares 960/960.

What is being scored is lossy by design, so the analysis is asked for tone
identity and the DTMF timing grid — what Opus preserves — rather than a
sample-exact match, which no codec would give.

The recorder reads `STREAMLIB_CAPTURED_WAVEFORM` (where to write the WAV) and
`STREAMLIB_CAPTURED_WAVEFORM_SECONDS` (how much decoded audio to record) in its
own helper process, which inherits the environment of the `tatolabd` the
fixture starts.
`verify_opus_roundtrip.sh` records 3.0 s: the signal is 2.78 s and the source
stops at 3.78 s, so that sits between them.
"""

import os

import known_audio_signal
import known_audio_signal_source
import tatolab.stream
from captured_audio_waveform_recorder import SECONDS_TO_RECORD, CapturedAudioWaveformRecorder
from known_audio_signal_source import KnownAudioSignalSource
from tatolab.stream import StreamBuilder, stream

# What the source will ever publish, derived rather than named so it cannot
# drift when the signal changes.
PUBLISHED_SECONDS = (
    len(known_audio_signal.generate_signal())
    + int(
        known_audio_signal_source.TRAILING_SILENCE_SECONDS
        * known_audio_signal.SAMPLE_RATE
    )
) / known_audio_signal.SAMPLE_RATE

# The decoder emits less than the source published, because it discards the
# encoder's lookahead at entry. The lookahead is whatever libopus reports —
# 312 samples at 48 kHz, 120 at `lowdelay` — so the bound gives back one whole
# 20 ms packet rather than a number that would rot if it changed.
LONGEST_RECORDABLE_SECONDS = PUBLISHED_SECONDS - 0.02


@stream
def known_signal_through_opus_and_back(stream_builder: StreamBuilder) -> None:
    if not os.environ.get("STREAMLIB_CAPTURED_WAVEFORM"):
        raise ValueError("STREAMLIB_CAPTURED_WAVEFORM names no file to write the decoded audio to")
    # Refused here rather than left to the recorder, which would simply never
    # reach its window and never write — costing the caller a timeout and a log
    # tail instead of a sentence naming the mistake.
    if SECONDS_TO_RECORD > LONGEST_RECORDABLE_SECONDS:
        raise ValueError(
            f"the recorder's {SECONDS_TO_RECORD} s (STREAMLIB_CAPTURED_WAVEFORM_SECONDS) is past the "
            f"{LONGEST_RECORDABLE_SECONDS:.4f} s this graph can ever record: the "
            f"source publishes {PUBLISHED_SECONDS:.4f} s once and then stops, and "
            "the decoder discards the encoder's lookahead on top of that"
        )

    signal = stream_builder.add(KnownAudioSignalSource)
    encoder = stream_builder.add(tatolab.stream.OpusEncoder)
    decoder = stream_builder.add(tatolab.stream.OpusDecoder)
    recorder = stream_builder.add(CapturedAudioWaveformRecorder)

    stream_builder.connect(signal.output("audio"), encoder.input("audio"))
    stream_builder.connect(encoder.output("encoded_audio"), decoder.input("encoded_audio"))
    stream_builder.connect(decoder.output("audio"), recorder.input("audio_from_upstream"))
