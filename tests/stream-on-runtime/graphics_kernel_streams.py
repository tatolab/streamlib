# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One stream per graphics-kernel probe, each the probe alone.

A kernel probe needs no upstream: it acquires its own input texture and colour
target and reports from `setup`, so its stream is one node.
"""

from tatolab.stream import StreamBuilder, stream

from graphics_kernel_probes import (
    FullscreenTriangleDrawProbe,
    GraphicsBindingRefusalProbe,
    GraphicsBufferBindingRefusalProbe,
    GraphicsPassShapeRefusalProbe,
    GraphicsStageMismatchProbe,
)


@stream
def fullscreen_triangle_draw_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(FullscreenTriangleDrawProbe)


@stream
def graphics_binding_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(GraphicsBindingRefusalProbe)


@stream
def graphics_stage_mismatch_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(GraphicsStageMismatchProbe)


@stream
def graphics_buffer_binding_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(GraphicsBufferBindingRefusalProbe)


@stream
def graphics_pass_shape_refusal_probe_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(GraphicsPassShapeRefusalProbe)
