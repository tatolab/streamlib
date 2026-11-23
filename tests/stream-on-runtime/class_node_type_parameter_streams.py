# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_class_node_type_parameter.py` runs from the suite project."""

from tatolab.stream import StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource

from class_node_type_parameter_probes import HandsOnEachPicture, ReportsThePicturesItReads


@graph_generator
def a_test_pattern_handed_on_by_a_class_typed_by_its_parameter(
    stream_builder: StreamBuilder,
) -> None:
    pattern = stream_builder.add(TestPatternSource, input_values={"width": 320, "height": 180})
    hand_on = stream_builder.add(HandsOnEachPicture, name="hand-on")
    reader = stream_builder.add(ReportsThePicturesItReads, name="reader")
    stream_builder.connect(pattern.output("video"), hand_on.input("picture_in"))
    stream_builder.connect(hand_on.output("picture_out"), reader.input("pictures"))
