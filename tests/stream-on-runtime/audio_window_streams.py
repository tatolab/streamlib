# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_audio_window_stage.py` runs from the suite project."""

from tatolab.microphone import MicrophoneSource
from tatolab.stream import StreamBuilder, graph_generator

from audio_window_probes import (
    DeclaredMonoWindowProbe,
    ExactWindowProbe,
    RollingWindowProbe,
    SourceFollowingWindowProbe,
    StereoToneSource,
)


def _microphone_into(stream_builder: StreamBuilder, probe_class: type) -> None:
    microphone = stream_builder.add(MicrophoneSource)
    probe = stream_builder.add(probe_class)
    stream_builder.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


@graph_generator
def microphone_into_an_exact_window_probe(stream_builder: StreamBuilder) -> None:
    _microphone_into(stream_builder, ExactWindowProbe)


@graph_generator
def microphone_into_a_rolling_window_probe(stream_builder: StreamBuilder) -> None:
    _microphone_into(stream_builder, RollingWindowProbe)


@graph_generator
def one_stereo_source_into_both_window_probes(stream_builder: StreamBuilder) -> None:
    """One stated-format source into two consumers: one that declares no
    channel count and one that declares mono.

    A Python source rather than the microphone, because what is under test is
    that the count follows *the source* — which needs a source whose count the
    test knows.
    """
    source = stream_builder.add(StereoToneSource)
    following = stream_builder.add(SourceFollowingWindowProbe)
    declared_mono = stream_builder.add(DeclaredMonoWindowProbe)
    stream_builder.connect(source.output("audio"), following.input("audio_from_upstream"))
    stream_builder.connect(source.output("audio"), declared_mono.input("audio_from_upstream"))
