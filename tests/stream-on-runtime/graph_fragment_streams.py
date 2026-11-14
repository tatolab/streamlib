# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_stream_graph_load_on_tatolabd.py` runs to load a
fragment inside a fragment."""

from runtime_load_served_graph_nodes import LoadedFrameSink
from tatolab.stream import GraphInput, Image, StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource


@graph_generator
def sink_behind_an_input(stream_builder: StreamBuilder) -> None:
    source = stream_builder.add(GraphInput, config={"port": "video", "family": Image})
    sink = stream_builder.add(LoadedFrameSink, name="sink")
    stream_builder.connect(source.output("value"), sink.input("video_from_upstream"))


@graph_generator
def look(stream_builder: StreamBuilder) -> None:
    source = stream_builder.add(GraphInput, config={"port": "video", "family": Image})
    soft = stream_builder.add(sink_behind_an_input, name="soft")
    stream_builder.connect(source.output("value"), soft.input("video"))


@graph_generator
def pattern_into_a_look_holding_a_fragment(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(
        TestPatternSource, name="pattern", config={"width": 320, "height": 180}
    )
    placed_look = stream_builder.add(look, name="look")
    stream_builder.connect(pattern.output("video"), placed_look.input("video"))
