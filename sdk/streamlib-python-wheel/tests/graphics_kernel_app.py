# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run one graphics-kernel probe in its real placement.

Run as a real `python app.py`: the probe builds and draws its kernel from a
helper process, and its observation reaches this app — and the test driving it —
over the child→parent log forwarding.

A kernel probe needs no upstream: it acquires its own input texture and colour
target and reports from `setup`, so each stream is that one probe alone.
"""

import sys

import streamlib
from streamlib import Stream, compile_stream_to_graph, stream

import graphics_kernel_probes


@stream
def one_fullscreen_triangle_draw_probe(stream: Stream) -> None:
    stream.add(graphics_kernel_probes.FullscreenTriangleDrawProbe)


@stream
def one_graphics_stage_mismatch_probe(stream: Stream) -> None:
    stream.add(graphics_kernel_probes.GraphicsStageMismatchProbe)


@stream
def one_graphics_binding_refusal_probe(stream: Stream) -> None:
    stream.add(graphics_kernel_probes.GraphicsBindingRefusalProbe)


@stream
def one_graphics_buffer_binding_refusal_probe(stream: Stream) -> None:
    stream.add(graphics_kernel_probes.GraphicsBufferBindingRefusalProbe)


@stream
def one_graphics_pass_shape_refusal_probe(stream: Stream) -> None:
    stream.add(graphics_kernel_probes.GraphicsPassShapeRefusalProbe)


STREAM_BY_SCENARIO = {
    "FullscreenTriangleDrawProbe": one_fullscreen_triangle_draw_probe,
    "GraphicsStageMismatchProbe": one_graphics_stage_mismatch_probe,
    "GraphicsBindingRefusalProbe": one_graphics_binding_refusal_probe,
    "GraphicsBufferBindingRefusalProbe": one_graphics_buffer_binding_refusal_probe,
    "GraphicsPassShapeRefusalProbe": one_graphics_pass_shape_refusal_probe,
}


if __name__ == "__main__":
    graph = compile_stream_to_graph(STREAM_BY_SCENARIO[sys.argv[1]])
    runtime = streamlib.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
