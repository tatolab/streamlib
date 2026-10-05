# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The audio built-in feeding a helper-placed consumer that declared a window.

Run as its own `python <script>.py` process: the claim is that the contract
crosses the parent→child wiring envelope and the child's own stage honours it,
which only a real child can show.
"""

import sys

import streamlib
from audio_window_probes import (
    DeclaredMonoWindowProbe,
    ExactWindowProbe,
    RollingWindowProbe,
    SourceFollowingWindowProbe,
    StereoToneSource,
)
from streamlib import Stream, compile_stream_to_graph, stream


def _microphone_into(stream: Stream, probe_class: type) -> None:
    microphone = stream.add(streamlib.MicrophoneSource)
    probe = stream.add(probe_class)
    stream.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


@stream
def microphone_into_an_exact_window_probe(stream: Stream) -> None:
    _microphone_into(stream, ExactWindowProbe)


@stream
def microphone_into_a_rolling_window_probe(stream: Stream) -> None:
    _microphone_into(stream, RollingWindowProbe)


@stream
def one_stereo_source_into_both_window_probes(stream: Stream) -> None:
    """One stated-format source into two consumers: one that declares no
    channel count and one that declares mono.

    A Python source rather than the microphone, because what is under test is
    that the count follows *the source* — which needs a source whose count the
    test knows.
    """
    source = stream.add(StereoToneSource)
    following = stream.add(SourceFollowingWindowProbe)
    declared_mono = stream.add(DeclaredMonoWindowProbe)
    stream.connect(source.output("audio"), following.input("audio_from_upstream"))
    stream.connect(source.output("audio"), declared_mono.input("audio_from_upstream"))


STREAM_BY_SCENARIO = {
    "contiguous_windows": microphone_into_an_exact_window_probe,
    "rolling_windows": microphone_into_a_rolling_window_probe,
    "source_following_windows": one_stereo_source_into_both_window_probes,
}


def main() -> None:
    scenario = sys.argv[1] if len(sys.argv) > 1 else "contiguous_windows"
    graph = compile_stream_to_graph(STREAM_BY_SCENARIO[scenario])
    runtime = streamlib.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
