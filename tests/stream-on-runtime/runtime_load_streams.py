# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_stream_graph_load_on_tatolabd.py` runs,
each run as a one-stream project whose `stream.py` delegates to it."""

from runtime_load_served_graph_nodes import LoadedFrameSink
from tatolab.h264 import H264Encoder
from tatolab.stream import NodeOutputPortReference, StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource

@graph_generator
def pattern_linked_from_a_port_it_lacks_into_an_encoder(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(TestPatternSource)
    encoder = stream_builder.add(H264Encoder)
    # Named past the typed node's own check, which refuses the port, as a saved graph can.
    stream_builder.connect(
        NodeOutputPortReference(pattern.name, "no_such_port"), encoder.input("video")
    )


@graph_generator
def named_pattern_into_a_named_sink(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(
        TestPatternSource, name="Loaded Pattern", input_values={"width": 320, "height": 180}
    )
    sink = stream_builder.add(LoadedFrameSink, name="Loaded Sink")
    stream_builder.connect(pattern.output("video"), sink.input("video_from_upstream"))
