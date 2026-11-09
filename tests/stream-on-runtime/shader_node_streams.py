# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One stream per shader-node scenario: a Python source into a shader node into a Python sink."""

from tatolab.stream import StreamBuilder, graph_generator

from shader_node_probes import KnownPixelsVideoSource, ShadedFramePixelReadingSink
from shader_node_shaders import tint


def _source_into_tint_into_sink(stream_builder: StreamBuilder, texture_format: str) -> None:
    source = stream_builder.add(
        KnownPixelsVideoSource, config={"texture_format": texture_format}
    )
    shaded = stream_builder.add(tint)
    sink = stream_builder.add(ShadedFramePixelReadingSink)
    stream_builder.connect(source.output("video"), shaded.input("image"))
    stream_builder.connect(shaded.output("result"), sink.input("shaded"))


@graph_generator
def rgba_source_into_tint_into_sink(stream_builder: StreamBuilder) -> None:
    _source_into_tint_into_sink(stream_builder, "rgba8_unorm")


@graph_generator
def bgra_source_into_tint_into_sink(stream_builder: StreamBuilder) -> None:
    _source_into_tint_into_sink(stream_builder, "bgra8_unorm")
