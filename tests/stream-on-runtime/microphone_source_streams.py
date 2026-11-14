# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_microphone_source.py` runs from the suite project."""

from tatolab.microphone import MicrophoneSource
from tatolab.stream import ScaledStreamClock, StreamBuilder, graph_generator

from microphone_source_probes import ChannelsBlockProbe

UNOPENABLE_DEVICE_ID = "not-a-real-audio-device"
SLOW_MOTION_SCALE = 0.5


@graph_generator
def one_microphone_source_left_unnamed(stream_builder: StreamBuilder) -> None:
    stream_builder.add(MicrophoneSource)


@graph_generator
def microphone_into_an_audio_block_probe(stream_builder: StreamBuilder) -> None:
    """Added with no `config` at all — the spelling for a block that needs no
    configuration, and the one that reaches the backend's default device."""
    microphone = stream_builder.add(MicrophoneSource)
    probe = stream_builder.add(ChannelsBlockProbe)
    stream_builder.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


@graph_generator
def microphone_into_an_audio_block_probe_in_slow_motion(stream_builder: StreamBuilder) -> None:
    stream_builder.run_on_clock(ScaledStreamClock(scale=SLOW_MOTION_SCALE))
    microphone = stream_builder.add(MicrophoneSource)
    probe = stream_builder.add(ChannelsBlockProbe)
    stream_builder.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


@graph_generator
def microphone_source_naming_an_unopenable_device(stream_builder: StreamBuilder) -> None:
    stream_builder.add(MicrophoneSource, config={"device_id": UNOPENABLE_DEVICE_ID})
