# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One stream per shader-node scenario: a Python source into shader nodes into a Python sink."""

from tatolab.stream import StreamBuilder, graph_generator

from shader_node_probes import (
    FrameDiscardingSink,
    KnownPixelsVideoSource,
    ShadedFramePixelReadingSink,
)
from shader_node_shaders import horizontal_blur, invert, swap_red_and_blue, tint


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


def _source_into_shader_chain_into_sink(
    stream_builder: StreamBuilder, shader_nodes, *, every_intermediate_read_twice: bool
) -> None:
    source = stream_builder.add(KnownPixelsVideoSource, config={"texture_format": "rgba8_unorm"})
    upstream = source.output("video")
    for position, shader_node in enumerate(shader_nodes):
        shaded = stream_builder.add(shader_node, name=shader_node.__name__)
        stream_builder.connect(upstream, shaded.input("image"))
        upstream = shaded.output("result")
        if every_intermediate_read_twice and position < len(shader_nodes) - 1:
            second_reader = stream_builder.add(
                FrameDiscardingSink, name=f"second_reader_of_{shader_node.__name__}"
            )
            stream_builder.connect(upstream, second_reader.input("discarded"))
    sink = stream_builder.add(ShadedFramePixelReadingSink)
    stream_builder.connect(upstream, sink.input("shaded"))


POINT_WISE_CHAIN = (tint, invert, swap_red_and_blue)
CHAIN_WITH_A_BLUR_IN_THE_MIDDLE = (tint, horizontal_blur, invert)


@graph_generator
def point_wise_chain_fused(stream_builder: StreamBuilder) -> None:
    _source_into_shader_chain_into_sink(
        stream_builder, POINT_WISE_CHAIN, every_intermediate_read_twice=False
    )


@graph_generator
def point_wise_chain_unfused_by_a_second_reader_of_each_intermediate(
    stream_builder: StreamBuilder,
) -> None:
    _source_into_shader_chain_into_sink(
        stream_builder, POINT_WISE_CHAIN, every_intermediate_read_twice=True
    )


@graph_generator
def chain_with_a_blur_in_the_middle(stream_builder: StreamBuilder) -> None:
    _source_into_shader_chain_into_sink(
        stream_builder, CHAIN_WITH_A_BLUR_IN_THE_MIDDLE, every_intermediate_read_twice=False
    )
