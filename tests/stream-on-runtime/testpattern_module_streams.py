# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_test_pattern_module.py` runs from the suite project."""

from tatolab.stream import StreamBuilder, stream
from tatolab.testpattern import TestPatternSource

from test_pattern_module_probes import VideoFrameProbe


@stream
def a_test_pattern_into_a_video_frame_probe(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(TestPatternSource, config={"width": 320, "height": 180})
    probe = stream_builder.add(VideoFrameProbe)
    stream_builder.connect(pattern.output("video"), probe.input("video_from_upstream"))


@stream
def a_test_pattern_source_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource)
