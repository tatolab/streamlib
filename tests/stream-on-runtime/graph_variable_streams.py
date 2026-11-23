# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The streams `test_graph_variables.py` starts on `tatolabd`."""

from tatolab.stream import GetVariable, SetVariable, StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource

from graph_variable_probes import ImageVariableReader

PATTERN_WIDTH = 320


@graph_generator
def a_test_pattern_held_in_a_variable_and_read_twice(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(
        TestPatternSource, input_values={"width": PATTERN_WIDTH, "height": 180}
    )
    setter = stream_builder.add(SetVariable, input_values={"variable": "pattern"})
    stream_builder.connect(pattern.output("video"), setter.input("value"))
    for reader in ("front", "back"):
        getter = stream_builder.add(GetVariable, input_values={"variable": "pattern"})
        image_reader = stream_builder.add(
            ImageVariableReader, name=reader, input_values={"reader": reader}
        )
        stream_builder.connect(
            getter.output("value"), image_reader.input("image_from_the_variable")
        )
