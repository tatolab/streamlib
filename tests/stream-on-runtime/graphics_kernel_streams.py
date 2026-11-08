# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One stream per graphics-kernel probe.

A kernel probe needs no upstream: it acquires its own input texture and colour
target and reports from `setup`, so its stream is one node. The rendered-frame
reader is the exception — it reads, downstream, the frame a producer rendered.
"""

from tatolab.stream import StreamBuilder, graph_generator

from graphics_kernel_probes import (
    FullscreenTriangleDrawProbe,
    GraphicsBindingRefusalProbe,
    GraphicsBufferBindingRefusalProbe,
    GraphicsPassShapeRefusalProbe,
    GraphicsStageMismatchProbe,
    RenderedFrameCpuReader,
    RenderedTextureFrameProducer,
)


@graph_generator
def fullscreen_triangle_draw_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(FullscreenTriangleDrawProbe)


@graph_generator
def graphics_binding_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(GraphicsBindingRefusalProbe)


@graph_generator
def graphics_stage_mismatch_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(GraphicsStageMismatchProbe)


@graph_generator
def graphics_buffer_binding_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(GraphicsBufferBindingRefusalProbe)


@graph_generator
def graphics_pass_shape_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(GraphicsPassShapeRefusalProbe)


@graph_generator
def rendered_texture_frame_read_through_cpu_downstream(stream_builder: StreamBuilder) -> None:
    producer = stream_builder.add(RenderedTextureFrameProducer)
    reader = stream_builder.add(RenderedFrameCpuReader)
    stream_builder.connect(
        producer.output("video_to_downstream"), reader.input("video_from_upstream")
    )
