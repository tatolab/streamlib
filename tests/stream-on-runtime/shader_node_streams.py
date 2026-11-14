# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""One stream per shader-node scenario: a Python source into shader nodes into a Python sink."""

from tatolab.stream import StreamBuilder, graph_generator

from shader_node_probes import (
    FrameDiscardingSink,
    KnownPixelsVideoSource,
    QuarterAmount,
    ShadedFramePixelReadingSink,
)
from shader_node_shaders import (
    horizontal_blur,
    invert,
    red_ramp_of_the_extent_of,
    swap_red_and_blue,
    tint,
)

#: Frames enough that the source outlasts the start of the stream's function node pool.
FRAMES_OUTLASTING_THE_START_OF_THE_FUNCTION_NODE_POOL = 2000


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
def tint_into_invert_fused_publishing_until_stopped(stream_builder: StreamBuilder) -> None:
    source = stream_builder.add(
        KnownPixelsVideoSource,
        name="source",
        config={
            "texture_format": "rgba8_unorm",
            "frames_to_publish": FRAMES_OUTLASTING_THE_START_OF_THE_FUNCTION_NODE_POOL,
        },
    )
    tinted = stream_builder.add(tint, name="tint")
    inverted = stream_builder.add(invert, name="invert")
    sink = stream_builder.add(ShadedFramePixelReadingSink, name="sink")
    stream_builder.connect(source.output("video"), tinted.input("image"))
    stream_builder.connect(tinted.output("result"), inverted.input("image"))
    stream_builder.connect(inverted.output("result"), sink.input("shaded"))


@graph_generator
def tint_and_invert_each_an_island_of_its_own_publishing_until_stopped(
    stream_builder: StreamBuilder,
) -> None:
    source = stream_builder.add(
        KnownPixelsVideoSource,
        name="source",
        config={
            "texture_format": "rgba8_unorm",
            "frames_to_publish": FRAMES_OUTLASTING_THE_START_OF_THE_FUNCTION_NODE_POOL,
        },
    )
    tinted = stream_builder.add(tint, name="tint")
    discard = stream_builder.add(FrameDiscardingSink, name="discard")
    inverted = stream_builder.add(invert, name="invert")
    sink = stream_builder.add(ShadedFramePixelReadingSink, name="sink")
    stream_builder.connect(source.output("video"), tinted.input("image"))
    stream_builder.connect(tinted.output("result"), discard.input("discarded"))
    stream_builder.connect(source.output("video"), inverted.input("image"))
    stream_builder.connect(inverted.output("result"), sink.input("shaded"))


@graph_generator
def chain_with_a_blur_in_the_middle(stream_builder: StreamBuilder) -> None:
    _source_into_shader_chain_into_sink(
        stream_builder, CHAIN_WITH_A_BLUR_IN_THE_MIDDLE, every_intermediate_read_twice=False
    )


def _function_node_into_the_amount_of_tint(
    stream_builder: StreamBuilder, *, tint_fused_into_invert: bool
) -> None:
    source = stream_builder.add(
        KnownPixelsVideoSource,
        config={
            "texture_format": "rgba8_unorm",
            "frames_to_publish": FRAMES_OUTLASTING_THE_START_OF_THE_FUNCTION_NODE_POOL,
        },
    )
    quarter_amount = stream_builder.add(QuarterAmount)
    tinted = stream_builder.add(tint, name="tint")
    sink = stream_builder.add(ShadedFramePixelReadingSink)
    stream_builder.connect(source.output("video"), tinted.input("image"))
    stream_builder.connect(quarter_amount.output("result"), tinted.input("amount"))
    shaded = tinted.output("result")
    if tint_fused_into_invert:
        inverted = stream_builder.add(invert, name="invert")
        stream_builder.connect(shaded, inverted.input("image"))
        shaded = inverted.output("result")
    stream_builder.connect(shaded, sink.input("shaded"))


@graph_generator
def function_node_into_the_amount_of_a_tint(stream_builder: StreamBuilder) -> None:
    _function_node_into_the_amount_of_tint(stream_builder, tint_fused_into_invert=False)


@graph_generator
def function_node_into_the_amount_of_a_tint_fused_into_invert(
    stream_builder: StreamBuilder,
) -> None:
    _function_node_into_the_amount_of_tint(stream_builder, tint_fused_into_invert=True)


@graph_generator
def source_into_a_shader_reading_only_the_extent_of_its_image(
    stream_builder: StreamBuilder,
) -> None:
    source = stream_builder.add(KnownPixelsVideoSource, config={"texture_format": "rgba8_unorm"})
    ramp = stream_builder.add(red_ramp_of_the_extent_of)
    sink = stream_builder.add(ShadedFramePixelReadingSink)
    stream_builder.connect(source.output("video"), ramp.input("extent_from"))
    stream_builder.connect(ramp.output("result"), sink.input("shaded"))
