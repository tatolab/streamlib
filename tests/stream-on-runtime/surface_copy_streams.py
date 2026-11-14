# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""One stream per surface-copy scenario: a test-pattern frame fed to one probe,
which runs in a processor interpreter of its own."""

from typing import Any

from tatolab.stream import StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource

import surface_copy_probes


def _wire_a_test_pattern_into_probe(
    stream_builder: StreamBuilder, probe_class: type, probe_config: dict[str, Any]
) -> None:
    pattern = stream_builder.add(
        TestPatternSource,
        config={
            "width": surface_copy_probes.FRAME_WIDTH,
            "height": surface_copy_probes.FRAME_HEIGHT,
        },
    )
    probe = stream_builder.add(probe_class, config=probe_config)
    stream_builder.connect(pattern.output("video"), probe.input("video_from_upstream"))


@graph_generator
def a_test_pattern_into_the_frame_landing_probe(stream_builder: StreamBuilder) -> None:
    _wire_a_test_pattern_into_probe(stream_builder, surface_copy_probes.FrameLandingProbe, {})


@graph_generator
def a_test_pattern_into_a_frame_landing_probe_skipping_the_copy(stream_builder: StreamBuilder) -> None:
    """The landing check's negative control: the kernel reads a texture nothing copied into."""
    _wire_a_test_pattern_into_probe(
        stream_builder, surface_copy_probes.FrameLandingProbe, {"skip_copy": True}
    )


@graph_generator
def a_test_pattern_into_the_copy_refusal_probe(stream_builder: StreamBuilder) -> None:
    _wire_a_test_pattern_into_probe(stream_builder, surface_copy_probes.CopyRefusalProbe, {})


STREAM_BY_SCENARIO = {
    "frame_landing": a_test_pattern_into_the_frame_landing_probe,
    "frame_landing_negative_control": (
        a_test_pattern_into_a_frame_landing_probe_skipping_the_copy
    ),
    "CopyRefusalProbe": a_test_pattern_into_the_copy_refusal_probe,
}

