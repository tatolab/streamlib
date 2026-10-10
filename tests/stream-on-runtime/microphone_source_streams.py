# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_microphone_source.py` runs from the suite project."""

import tatolab.stream
from tatolab.stream import StreamBuilder, stream

from microphone_source_probes import AudioBlockProbe

UNOPENABLE_DEVICE_ID = "not-a-real-audio-device"


@stream
def one_microphone_source_left_unnamed(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.MicrophoneSource)


@stream
def microphone_into_an_audio_block_probe(stream_builder: StreamBuilder) -> None:
    """Added with no `config` at all — the spelling for a block that needs no
    configuration, and the one that reaches the backend's default device."""
    microphone = stream_builder.add(tatolab.stream.MicrophoneSource)
    probe = stream_builder.add(AudioBlockProbe)
    stream_builder.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


@stream
def microphone_source_naming_an_unopenable_device(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.MicrophoneSource, config={"device_id": UNOPENABLE_DEVICE_ID})
