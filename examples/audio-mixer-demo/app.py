# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: three tone voices → chord mixer → speaker.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest
and no `main()`. Each voice publishes at a deliberately different sample rate,
channel count and block size; the mixer's ports declare one window contract and
the engine converts all three to it, so the mixer itself only adds numbers.

Processors live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from processors.chord_mixer import ChordMixer
from processors.tone_source import ToneSource

from streamlib import Runtime, SpeakerSink

# A C major chord in equal temperament.
C4_FREQUENCY_HZ = 261.63
E4_FREQUENCY_HZ = 329.63
G4_FREQUENCY_HZ = 392.00

# Three voices at 0.3 sum to 0.9 — below full scale, so the mix never clips.
VOICE_AMPLITUDE = 0.3


def setup(rt: Runtime) -> None:
    root_voice = rt.add(
        ToneSource,
        config={
            "frequency_hz": C4_FREQUENCY_HZ,
            "amplitude": VOICE_AMPLITUDE,
            "sample_rate": 48_000,
            "channels": 1,
            "block_size": 512,
        },
        display_name="C4Voice",
    )
    # 44.1 kHz in 441-sample blocks: the rate the mixer's contract resamples.
    third_voice = rt.add(
        ToneSource,
        config={
            "frequency_hz": E4_FREQUENCY_HZ,
            "amplitude": VOICE_AMPLITUDE,
            "sample_rate": 44_100,
            "channels": 1,
            "block_size": 441,
        },
        display_name="E4Voice",
    )
    # 16 kHz stereo: resampled up and mixed down to mono by the same contract.
    fifth_voice = rt.add(
        ToneSource,
        config={
            "frequency_hz": G4_FREQUENCY_HZ,
            "amplitude": VOICE_AMPLITUDE,
            "sample_rate": 16_000,
            "channels": 2,
            "block_size": 256,
        },
        display_name="G4Voice",
    )

    mixer = rt.add(ChordMixer)
    speaker = rt.add(SpeakerSink)

    rt.connect(root_voice.output("voice_to_downstream"), mixer.input("root_voice_from_upstream"))
    rt.connect(third_voice.output("voice_to_downstream"), mixer.input("third_voice_from_upstream"))
    rt.connect(fifth_voice.output("voice_to_downstream"), mixer.input("fifth_voice_from_upstream"))
    rt.connect(mixer.output("mixed_chord_to_downstream"), speaker.input("audio"))
