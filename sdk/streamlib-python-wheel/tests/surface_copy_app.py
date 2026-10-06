# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that feed a test-pattern frame to one surface-copy probe in its
real placement, a helper process."""

import sys
from typing import Any

import tatolab.runtime
import tatolab.stream
from tatolab.stream import Stream, compile_stream_to_graph, stream

import surface_copy_probes


def _wire_a_test_pattern_into_probe(
    stream: Stream, probe_class: type, probe_config: dict[str, Any]
) -> None:
    pattern = stream.add(
        tatolab.stream.TestPatternSource,
        config={
            "width": surface_copy_probes.FRAME_WIDTH,
            "height": surface_copy_probes.FRAME_HEIGHT,
        },
    )
    probe = stream.add(probe_class, config=probe_config)
    stream.connect(pattern.output("video"), probe.input("video_from_upstream"))


@stream
def a_test_pattern_into_the_frame_landing_probe(stream: Stream) -> None:
    _wire_a_test_pattern_into_probe(stream, surface_copy_probes.FrameLandingProbe, {})


@stream
def a_test_pattern_into_a_frame_landing_probe_skipping_the_copy(stream: Stream) -> None:
    """The landing check's negative control: the kernel reads a texture nothing copied into."""
    _wire_a_test_pattern_into_probe(
        stream, surface_copy_probes.FrameLandingProbe, {"skip_copy": True}
    )


@stream
def a_test_pattern_into_the_copy_refusal_probe(stream: Stream) -> None:
    _wire_a_test_pattern_into_probe(stream, surface_copy_probes.CopyRefusalProbe, {})


STREAM_BY_SCENARIO = {
    "frame_landing": a_test_pattern_into_the_frame_landing_probe,
    "frame_landing_negative_control": (
        a_test_pattern_into_a_frame_landing_probe_skipping_the_copy
    ),
    "CopyRefusalProbe": a_test_pattern_into_the_copy_refusal_probe,
}


if __name__ == "__main__":
    graph = compile_stream_to_graph(STREAM_BY_SCENARIO[sys.argv[1]])
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
