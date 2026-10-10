# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_audio_window_stage.py` runs from the suite project."""

import tatolab.stream
from tatolab.stream import StreamBuilder, stream

from audio_window_probes import (
    DeclaredMonoWindowProbe,
    ExactWindowProbe,
    RollingWindowProbe,
    SourceFollowingWindowProbe,
    StereoToneSource,
)


def _microphone_into(stream_builder: StreamBuilder, probe_class: type) -> None:
    microphone = stream_builder.add(tatolab.stream.MicrophoneSource)
    probe = stream_builder.add(probe_class)
    stream_builder.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


@stream
def microphone_into_an_exact_window_probe(stream_builder: StreamBuilder) -> None:
    _microphone_into(stream_builder, ExactWindowProbe)


@stream
def microphone_into_a_rolling_window_probe(stream_builder: StreamBuilder) -> None:
    _microphone_into(stream_builder, RollingWindowProbe)


@stream
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
