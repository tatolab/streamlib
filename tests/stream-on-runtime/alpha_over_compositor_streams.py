# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One stream per `AlphaOverCompositor` scenario: colour sources into the compositor, then the probe.

Each scenario's layers are data, so the test builds its reference composite
from the same description the stream was wired from.
"""

from dataclasses import dataclass

from tatolab.compositor import AlphaOverCompositor
from tatolab.stream import StreamBuilder, stream

import alpha_over_compositor_probes
from alpha_over_compositor_probes import (
    AlphaOverCompositeProbe,
    NeverPublishingVideoSource,
    SolidPremultipliedColourSource,
)


@dataclass(frozen=True)
class CompositedLayerUnderTest:
    """One layer: a solid colour source, or `None` for one that never publishes, and its placement."""

    premultiplied_rgba: "tuple[int, int, int, int] | None"
    width: int = alpha_over_compositor_probes.CANVAS_WIDTH
    height: int = alpha_over_compositor_probes.CANVAS_HEIGHT
    x: int = 0
    y: int = 0
    opacity: float = 1.0


OPAQUE_GREY = (90, 90, 90, 255)
OPAQUE_RED = (255, 0, 0, 255)
OPAQUE_BLUE = (0, 0, 255, 255)

LAYERS_BY_SCENARIO: "dict[str, list[CompositedLayerUnderTest]]" = {
    # A backmost layer smaller than the canvas, a half-transparent layer, a layer
    # at half opacity clipped at the bottom right, one clipped at the top left.
    "premultiplied_alpha": [
        CompositedLayerUnderTest((40, 80, 120, 255), width=24, height=16),
        CompositedLayerUnderTest((100, 0, 50, 128), width=12, height=8, x=4, y=4),
        CompositedLayerUnderTest((200, 200, 0, 255), width=10, height=10, x=26, y=18, opacity=0.5),
        CompositedLayerUnderTest((0, 160, 0, 160), width=6, height=6, x=-3, y=-3),
    ],
    "red_on_layer_1_blue_on_layer_2": [
        CompositedLayerUnderTest(OPAQUE_GREY),
        CompositedLayerUnderTest(OPAQUE_RED, width=16, height=16, x=0, y=0),
        CompositedLayerUnderTest(OPAQUE_BLUE, width=16, height=16, x=8, y=4),
    ],
    "blue_on_layer_1_red_on_layer_2": [
        CompositedLayerUnderTest(OPAQUE_GREY),
        CompositedLayerUnderTest(OPAQUE_BLUE, width=16, height=16, x=8, y=4),
        CompositedLayerUnderTest(OPAQUE_RED, width=16, height=16, x=0, y=0),
    ],
    # Layer 1 would cover the whole canvas if it ever sent a frame.
    "layer_1_never_sends": [
        CompositedLayerUnderTest(OPAQUE_GREY),
        CompositedLayerUnderTest(None),
        CompositedLayerUnderTest(OPAQUE_RED, width=8, height=8, x=2, y=2),
    ],
    "layer_0_never_sends": [
        CompositedLayerUnderTest(None),
        CompositedLayerUnderTest(OPAQUE_RED),
    ],
}


def _wire_the_scenario(stream_builder: StreamBuilder, scenario: str) -> None:
    layers = LAYERS_BY_SCENARIO[scenario]
    compositor = stream_builder.add(
        AlphaOverCompositor,
        config={
            "width": alpha_over_compositor_probes.CANVAS_WIDTH,
            "height": alpha_over_compositor_probes.CANVAS_HEIGHT,
            "layers": [
                {"x": layer.x, "y": layer.y, "opacity": layer.opacity} for layer in layers
            ],
        },
    )
    for layer_index, layer in enumerate(layers):
        if layer.premultiplied_rgba is None:
            source = stream_builder.add(NeverPublishingVideoSource)
        else:
            source = stream_builder.add(
                SolidPremultipliedColourSource,
                config={
                    "width": layer.width,
                    "height": layer.height,
                    "premultiplied_rgba": list(layer.premultiplied_rgba),
                },
            )
        stream_builder.connect(source.output("video"), compositor.input(f"layer_{layer_index}"))
    probe = stream_builder.add(AlphaOverCompositeProbe)
    stream_builder.connect(compositor.output("video"), probe.input("composite_from_upstream"))


@stream
def premultiplied_layers_into_the_compositor(stream_builder: StreamBuilder) -> None:
    _wire_the_scenario(stream_builder, "premultiplied_alpha")


@stream
def red_on_layer_1_and_blue_on_layer_2_into_the_compositor(stream_builder: StreamBuilder) -> None:
    _wire_the_scenario(stream_builder, "red_on_layer_1_blue_on_layer_2")


@stream
def blue_on_layer_1_and_red_on_layer_2_into_the_compositor(stream_builder: StreamBuilder) -> None:
    _wire_the_scenario(stream_builder, "blue_on_layer_1_red_on_layer_2")


@stream
def a_layer_1_that_never_sends_into_the_compositor(stream_builder: StreamBuilder) -> None:
    _wire_the_scenario(stream_builder, "layer_1_never_sends")


@stream
def a_layer_0_that_never_sends_into_the_compositor(stream_builder: StreamBuilder) -> None:
    _wire_the_scenario(stream_builder, "layer_0_never_sends")


STREAM_BY_SCENARIO = {
    "premultiplied_alpha": premultiplied_layers_into_the_compositor,
    "red_on_layer_1_blue_on_layer_2": red_on_layer_1_and_blue_on_layer_2_into_the_compositor,
    "blue_on_layer_1_red_on_layer_2": blue_on_layer_1_and_red_on_layer_2_into_the_compositor,
    "layer_1_never_sends": a_layer_1_that_never_sends_into_the_compositor,
    "layer_0_never_sends": a_layer_0_that_never_sends_into_the_compositor,
}
