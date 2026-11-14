# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_test_pattern_module.py` runs from the suite project."""

from tatolab.stream import StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource

from test_pattern_module_probes import ImageFrameProbe


@graph_generator
def a_test_pattern_into_an_image_frame_probe(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(TestPatternSource, config={"width": 320, "height": 180})
    probe = stream_builder.add(ImageFrameProbe)
    stream_builder.connect(pattern.output("video"), probe.input("video_from_upstream"))


@graph_generator
def a_test_pattern_source_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource)
