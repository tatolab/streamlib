# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.compositor:AlphaOverCompositor` layers frames on the GPU beneath `tatolabd`.

Each scenario wires solid-colour sources into the compositor's layers and a
probe after it; the probe reports the composite's pixels once every source has
been composited, and the test holds them to a reference composite built with
numpy from the scenario's own description.
"""

from collections.abc import Callable

import numpy
import pytest

import alpha_over_compositor_probes
import alpha_over_compositor_streams
from alpha_over_compositor_streams import OPAQUE_BLUE, OPAQUE_RED, CompositedLayerUnderTest
from runtime_process_under_test import RuntimeProcessUnderTest

pytestmark = pytest.mark.requires_gpu

#: An 8-bit store rounds once per composite; a reference in floats can sit one step away.
CHANNEL_TOLERANCE = 1


def run_scenario(
    start_tatolabd_running_stream: "Callable[..., RuntimeProcessUnderTest]", scenario: str
) -> dict:
    tatolabd = start_tatolabd_running_stream(
        alpha_over_compositor_streams.STREAM_BY_SCENARIO[scenario]
    )
    observation = tatolabd.await_marker("PROBE_RESULT")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert isinstance(observation, dict), f"no parseable probe result:\n{tatolabd.stderr_text}"
    if "failure" in observation:
        pytest.fail(f"the probe raised in its processor interpreter:\n{observation['failure']}")
    return observation


def reference_composite(layers: "list[CompositedLayerUnderTest]") -> numpy.ndarray:
    """Premultiplied source-over, back to front, onto transparent black, in floats."""
    canvas = numpy.zeros(
        (alpha_over_compositor_probes.CANVAS_HEIGHT, alpha_over_compositor_probes.CANVAS_WIDTH, 4)
    )
    for layer in layers:
        if layer.premultiplied_rgba is None:
            continue
        above = numpy.array(layer.premultiplied_rgba) / 255 * layer.opacity
        top, bottom = max(layer.y, 0), min(layer.y + layer.height, canvas.shape[0])
        left, right = max(layer.x, 0), min(layer.x + layer.width, canvas.shape[1])
        below = canvas[top:bottom, left:right]
        canvas[top:bottom, left:right] = above + below * (1 - above[3])
    return numpy.rint(canvas * 255)


def composited_pixels(observation: dict) -> numpy.ndarray:
    assert observation["composites_received"] > 0, "the compositor published nothing"
    assert observation["extent"] == [
        alpha_over_compositor_probes.CANVAS_WIDTH,
        alpha_over_compositor_probes.CANVAS_HEIGHT,
    ]
    return numpy.array(observation["pixels"], dtype=numpy.int64)


def channels_off_the_reference(observed: numpy.ndarray, expected: numpy.ndarray) -> int:
    return int((numpy.abs(observed - expected) > CHANNEL_TOLERANCE).sum())


def test_premultiplied_layers_blend_over_transparent_black_at_their_placements(
    start_tatolabd_running_stream,
):
    layers = alpha_over_compositor_streams.LAYERS_BY_SCENARIO["premultiplied_alpha"]
    observed = composited_pixels(run_scenario(start_tatolabd_running_stream, "premultiplied_alpha"))
    # The negative control: the half-transparent layer blended as straight alpha
    # must miss the observed pixels, so the check discriminates.
    layers_read_as_straight_alpha = [
        layers[0],
        CompositedLayerUnderTest(
            (200, 0, 100, 128),
            width=layers[1].width,
            height=layers[1].height,
            x=layers[1].x,
            y=layers[1].y,
        ),
        *layers[2:],
    ]

    assert channels_off_the_reference(observed, reference_composite(layers)) == 0, observed
    assert channels_off_the_reference(
        observed, reference_composite(layers_read_as_straight_alpha)
    ) > 0
    assert observed[-1, -1].tolist() == [0, 0, 0, 0], "outside every layer is transparent black"


@pytest.mark.parametrize(
    "scenario, colour_on_top_where_they_overlap",
    [
        ("red_on_layer_1_blue_on_layer_2", OPAQUE_BLUE),
        ("blue_on_layer_1_red_on_layer_2", OPAQUE_RED),
    ],
)
def test_a_higher_layer_is_drawn_over_a_lower_one(
    start_tatolabd_running_stream, scenario, colour_on_top_where_they_overlap
):
    layers = alpha_over_compositor_streams.LAYERS_BY_SCENARIO[scenario]
    observed = composited_pixels(run_scenario(start_tatolabd_running_stream, scenario))

    assert channels_off_the_reference(observed, reference_composite(layers)) == 0
    assert observed[10, 12].tolist() == list(colour_on_top_where_they_overlap)


def test_a_layer_that_has_never_sent_a_frame_is_left_out(start_tatolabd_running_stream):
    layers = alpha_over_compositor_streams.LAYERS_BY_SCENARIO["layer_1_never_sends"]
    observed = composited_pixels(run_scenario(start_tatolabd_running_stream, "layer_1_never_sends"))

    assert channels_off_the_reference(observed, reference_composite(layers)) == 0


def test_nothing_is_published_until_layer_0_sends_a_frame(start_tatolabd_running_stream):
    observation = run_scenario(start_tatolabd_running_stream, "layer_0_never_sends")

    assert observation["composites_received"] == 0
