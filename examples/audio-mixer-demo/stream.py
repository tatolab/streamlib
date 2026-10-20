# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: three tone voices → chord mixer → speaker.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest. Each voice publishes at a deliberately different sample rate, channel
count and block size; the mixer's ports declare one window contract and the
engine converts all three to it, so the mixer itself only adds numbers.

Nodes live in their own modules, never in this file: each one runs in its own
child interpreter, which imports the class by name.
"""

from tatolab.stream import SpeakerSink, SpeakerSinkConfig, StreamBuilder, stream

from nodes.chord_mixer import ChordMixer
from nodes.tone_source import ToneSource

# A C major chord in equal temperament.
C4_FREQUENCY_HZ = 261.63
E4_FREQUENCY_HZ = 329.63
G4_FREQUENCY_HZ = 392.00

# Three voices at 0.3 sum to 0.9 — below full scale, so the mix never clips.
VOICE_AMPLITUDE = 0.3

# The playback device as the audio backend names it. `None` opens the
# backend's default; a named device it cannot open raises rather than landing
# on another.
SPEAKER_DEVICE_ID: str | None = None


@stream
def main(stream_builder: StreamBuilder) -> None:
    """Three mismatched sine voices, converted by one window contract, mixed and played."""
    root_voice = stream_builder.add(
        ToneSource,
        name="c4voice",
        config={
            "frequency_hz": C4_FREQUENCY_HZ,
            "amplitude": VOICE_AMPLITUDE,
            "sample_rate": 48_000,
            "channels": 1,
            "block_size": 512,
        },
    )
    # 44.1 kHz in 441-sample blocks: the rate the mixer's contract resamples.
    third_voice = stream_builder.add(
        ToneSource,
        name="e4voice",
        config={
            "frequency_hz": E4_FREQUENCY_HZ,
            "amplitude": VOICE_AMPLITUDE,
            "sample_rate": 44_100,
            "channels": 1,
            "block_size": 441,
        },
    )
    # 16 kHz stereo: resampled up and mixed down to mono by the same contract.
    fifth_voice = stream_builder.add(
        ToneSource,
        name="g4voice",
        config={
            "frequency_hz": G4_FREQUENCY_HZ,
            "amplitude": VOICE_AMPLITUDE,
            "sample_rate": 16_000,
            "channels": 2,
            "block_size": 256,
        },
    )

    speaker_config: SpeakerSinkConfig = {}
    if SPEAKER_DEVICE_ID is not None:
        speaker_config["device_id"] = SPEAKER_DEVICE_ID
    mixer = stream_builder.add(ChordMixer)
    speaker = stream_builder.add(SpeakerSink, config=speaker_config)

    stream_builder.connect(
        root_voice.output("voice_to_downstream"),
        mixer.input("root_voice_from_upstream"),
    )
    stream_builder.connect(
        third_voice.output("voice_to_downstream"),
        mixer.input("third_voice_from_upstream"),
    )
    stream_builder.connect(
        fifth_voice.output("voice_to_downstream"),
        mixer.input("fifth_voice_from_upstream"),
    )
    stream_builder.connect(
        mixer.output("mixed_chord_to_downstream"), speaker.input("audio")
    )
