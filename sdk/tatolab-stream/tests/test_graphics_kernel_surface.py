# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""What a graphics kernel cannot be asked for, read off the surface a node is handed.

The runtime's own classes are held to these Protocols member for member, so a
parameter that reappears on either one fails here or at the conformance gate.
"""

import inspect

from tatolab.stream import GpuContextFullAccess, GraphicsKernel


def test_a_draw_takes_no_vertex_buffer_no_index_buffer_and_no_depth_target():
    """The recon constraints, stated where a caller meets them.

    No escalate op mints a `VertexBuffer` or an `IndexBuffer`, and the
    offscreen pass a draw runs attaches colour targets only — so the honest
    surface is one that cannot ask for them at all. Asserted against the
    signature rather than a refusal message, because a parameter that quietly
    reappears is exactly what this forbids.
    """
    parameters = inspect.signature(GraphicsKernel.draw).parameters
    unsupported = [
        name
        for name in parameters
        if "vertex_buffer" in name or "index_buffer" in name or "depth" in name
    ]
    assert unsupported == [], (
        f"a draw cannot honour {unsupported}: no escalate op mints a vertex or "
        "index buffer, and the pass attaches colour targets only"
    )
    assert "vertex_count" in parameters, (
        "the vertices are the shaders' own — a draw still says how many of them"
    )


def test_a_graphics_kernel_carries_no_depth_or_vertex_input_state():
    """The pipeline the wire builds has no depth attachment and no vertex
    input, so neither is a knob `create_graphics_kernel` offers."""
    parameters = inspect.signature(GpuContextFullAccess.create_graphics_kernel).parameters
    unsupported = [
        name
        for name in parameters
        if "depth" in name or "vertex_input" in name or "multisample" in name
    ]
    assert unsupported == [], (
        f"the graphics kernel builds single-sampled colour-only pipelines: {unsupported}"
    )
