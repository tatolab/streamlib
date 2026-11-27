# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`tatolab.compositor:AlphaOverCompositor` layers frames on the GPU beneath `tatolabd`.

Each scenario wires solid-colour sources into the compositor's layers and a
probe after it; the probe reports the composite's pixels once every source has
been composited, and the test holds them to a reference composite built with
numpy from the scenario's own description.
"""

import time
from collections.abc import Callable
from typing import Any

import numpy
import pytest

import alpha_over_compositor_probes
import alpha_over_compositor_streams
from alpha_over_compositor_streams import (
    BLUE_LAYER_WIRED_WHILE_PLAYING,
    OPAQUE_BLUE,
    OPAQUE_RED,
    CompositedLayerUnderTest,
)
from conftest import TatolabdUnderTest
from helper_process_observation import helper_process_ids_started_in
from runtime_process_under_test import RuntimeProcessUnderTest

pytestmark = pytest.mark.requires_gpu

#: An 8-bit store rounds once per composite; a reference in floats can sit one step away.
CHANNEL_TOLERANCE = 1

#: How long a layer wired or unwired while playing may take to reach the probe's report.
A_LAYER_EDIT_REACHES_THE_PROBE_TIMEOUT_SECONDS = 30.0

#: `tatolabd`'s line for each node it rebuilds.
NODE_REBUILT_LOG_LINE_TEXT = "rebuilt `"


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
        pytest.fail(f"the probe raised in its node interpreter:\n{observation['failure']}")
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
    assert observed[-1, 0].tolist() == [0, 0, 0, 0], "outside every layer is transparent black"


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


def test_layers_past_the_eighth_are_laid_over_the_first_eight_in_order(
    start_tatolabd_running_stream,
):
    """Fail-without-fix: drop the dispatch after the first and the ninth and tenth layers'
    squares are missing; lay them under the composite so far and the grey covers them."""
    layers = alpha_over_compositor_streams.LAYERS_BY_SCENARIO["ten_layers"]
    observed = composited_pixels(run_scenario(start_tatolabd_running_stream, "ten_layers"))

    assert channels_off_the_reference(observed, reference_composite(layers)) == 0
    assert observed[7, 5].tolist() == list(OPAQUE_RED), "the ninth layer shows"
    red, blue = observed[9, 7][0], observed[9, 7][2]
    assert abs(red - 128) <= CHANNEL_TOLERANCE and abs(blue - 128) <= CHANNEL_TOLERANCE, (
        "the tenth layer, at half opacity, lies over the ninth"
    )


def await_a_composite_after(
    tatolabd: TatolabdUnderTest, reports_before_the_edit: int, layers: "list[CompositedLayerUnderTest]"
) -> "numpy.ndarray | None":
    """The pixels of the first probe report after the first `reports_before_the_edit` that
    matches the composite of `layers`, or `None` when none does in time."""
    expected = reference_composite(layers)
    deadline = time.monotonic() + A_LAYER_EDIT_REACHES_THE_PROBE_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        for report in tatolabd.marker_payloads("PROBE_RESULT")[reports_before_the_edit:]:
            if "pixels" not in report:
                continue
            observed = composited_pixels(report)
            if channels_off_the_reference(observed, expected) == 0:
                return observed
        time.sleep(0.05)
    return None


def the_picture_slots_of(graph: "dict[str, Any]", node_id: str) -> "list[int]":
    """The slot of each `layers.<slot>.picture` input the graph shows node `node_id` holding."""
    (node,) = [node for node in graph["nodes"] if node["id"] == node_id]
    return sorted(
        port["pin_group_slot"]["slot"]
        for port in node["ports"]["inputs"]
        if (port.get("pin_group_slot") or {}).get("pin") == "picture"
    )


def test_a_layer_wired_into_a_playing_compositor_shows_and_unwired_drops_with_no_rebuild(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """Mental-revert: a helper that does not say it takes its slots live leaves the engine to
    rebuild the compositor for the third slot, which the transaction answers in `nodes_rebuilt`
    and which starts a helper of its own; one that holds only its setup's slots never lays blue
    over grey and red."""
    two_layers = alpha_over_compositor_streams.LAYERS_BY_SCENARIO[
        "red_on_layer_1_blue_on_layer_2"
    ][:2]
    tatolabd = start_tatolabd_running_stream(
        alpha_over_compositor_streams.grey_and_red_into_the_compositor_reporting_until_stopped
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    first_report = tatolabd.await_marker("PROBE_RESULT")
    assert "failure" not in first_report, first_report.get("failure")
    assert channels_off_the_reference(
        composited_pixels(first_report), reference_composite(two_layers)
    ) == 0
    local_api = tatolabd.local_api_client()
    graph = local_api.graph(stream=stream_id)
    (compositor_id,) = [
        node["id"] for node in graph["nodes"] if node["type"].endswith(":AlphaOverCompositor")
    ]
    assert the_picture_slots_of(graph, compositor_id) == [0, 1, 2], "two layers and an empty slot"
    helpers_before_the_edit = len(helper_process_ids_started_in(tatolabd.stderr_text))
    reports_before_the_edit = len(tatolabd.marker_payloads("PROBE_RESULT"))

    wired = local_api.call_tool(
        "transact",
        {
            "mode": "live",
            "operations": [
                {
                    "operation": "add_node",
                    "stream": stream_id,
                    "type": "alpha_over_compositor_probes:SolidPremultipliedColourSource",
                    "name": "blue",
                    "as": "blue",
                },
                *(
                    {
                        "operation": "set_node_input_value",
                        "stream": stream_id,
                        "node": "blue",
                        "input": input_name,
                        "value": value,
                    }
                    for input_name, value in (
                        ("width", BLUE_LAYER_WIRED_WHILE_PLAYING.width),
                        ("height", BLUE_LAYER_WIRED_WHILE_PLAYING.height),
                        ("premultiplied_rgba", list(OPAQUE_BLUE)),
                    )
                ),
                {
                    "operation": "connect",
                    "stream": stream_id,
                    "source": {"node": "blue", "port": "video"},
                    "target": {"node": compositor_id, "port": "layers.2.picture"},
                },
                *(
                    {
                        "operation": "set_node_input_value",
                        "stream": stream_id,
                        "node": compositor_id,
                        "input": f"layers.2.{member}",
                        "value": value,
                    }
                    for member, value in (
                        ("x", BLUE_LAYER_WIRED_WHILE_PLAYING.x),
                        ("y", BLUE_LAYER_WIRED_WHILE_PLAYING.y),
                    )
                ),
            ],
        },
    )

    assert wired.get("nodes_rebuilt", []) == [], wired
    three_layers = [*two_layers, BLUE_LAYER_WIRED_WHILE_PLAYING]
    with_blue = await_a_composite_after(tatolabd, reports_before_the_edit, three_layers)
    assert with_blue is not None, f"blue never joined the composite:\n{tatolabd.recent_stderr()}"
    assert with_blue[10, 12].tolist() == list(OPAQUE_BLUE)
    graph = local_api.graph(stream=stream_id)
    assert the_picture_slots_of(graph, compositor_id) == [0, 1, 2, 3], "a new empty slot shows"
    assert len(helper_process_ids_started_in(tatolabd.stderr_text)) == helpers_before_the_edit + 1, (
        "only the blue source starts a helper; the compositor keeps its own"
    )

    (blue,) = wired["added_nodes"]
    reports_before_the_unwire = len(tatolabd.marker_payloads("PROBE_RESULT"))
    unwired = local_api.call_tool(
        "transact",
        {
            "mode": "live",
            "operations": [
                {
                    "operation": "disconnect",
                    "stream": stream_id,
                    "source": {"node": blue["node"], "port": "video"},
                    "target": {"node": compositor_id, "port": "layers.2.picture"},
                }
            ],
        },
    )

    assert unwired.get("nodes_rebuilt", []) == [], unwired
    without_blue = await_a_composite_after(tatolabd, reports_before_the_unwire, two_layers)
    graph = local_api.graph(stream=stream_id)
    stderr_text = tatolabd.stderr_text
    (listed,) = [listed for listed in local_api.list_streams() if listed["stream"] == stream_id]
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert without_blue is not None, f"blue was never dropped from the composite:\n{stderr_text}"
    assert the_picture_slots_of(graph, compositor_id) == [0, 1, 2], "the slot and its link drop"
    assert [link for link in graph["links"] if link["target"]["port"] == "layers.2.picture"] == []
    assert len(helper_process_ids_started_in(stderr_text)) == helpers_before_the_edit + 1
    assert NODE_REBUILT_LOG_LINE_TEXT not in stderr_text
    assert listed["state"] == "running", listed
    assert listed["condition"]["health"] == "healthy", listed
