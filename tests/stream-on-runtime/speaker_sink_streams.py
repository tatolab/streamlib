# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_speaker_sink.py` runs from the suite project."""

from tatolab.microphone import MicrophoneSource
from tatolab.speaker import SpeakerSink
from tatolab.stream import StreamBuilder, stream

from speaker_sink_probes import AudioBlockCountingProbe

SPEAKER_NODE_NAME = "speakersink"
UNOPENABLE_DEVICE_ID = "not-a-real-audio-device"


@stream
def one_speaker_sink_left_unnamed(stream_builder: StreamBuilder) -> None:
    stream_builder.add(SpeakerSink)


@stream
def microphone_into_a_speaker_and_a_block_counting_probe(stream_builder: StreamBuilder) -> None:
    """A microphone wired straight to a speaker, with no Python in the sample path.

    `stream_builder.add` with no `config` on either end records `{}`. The two
    ends need not agree on rate, channels or dtype, and on a stock machine they
    do not: the ALSA arm asks a capture device for mono. `SpeakerSink`'s input
    port declares a 48 kHz stereo `f32` window, so the engine converts every
    block into the format the speaker opened its playback stream in.

    The probe hangs off the same output the speaker reads, so the test has a
    marker saying enough blocks have really flowed rather than a sleep guessing
    that they have. It is a second consumer of the microphone's port, not a
    stage between the two nodes.
    """
    microphone = stream_builder.add(MicrophoneSource)
    speaker = stream_builder.add(SpeakerSink, name=SPEAKER_NODE_NAME)
    stream_builder.connect(microphone.output("audio"), speaker.input("audio"))

    probe = stream_builder.add(AudioBlockCountingProbe)
    stream_builder.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


@stream
def speaker_sink_naming_an_unopenable_device(stream_builder: StreamBuilder) -> None:
    stream_builder.add(SpeakerSink, config={"device_id": UNOPENABLE_DEVICE_ID})
