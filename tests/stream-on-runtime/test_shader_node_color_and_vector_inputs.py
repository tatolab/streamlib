# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A `@node.shader` taking a Color and a Vector on `tatolabd`: a value set live, or a Color a
function node hands out over a link, changes the picture the shader writes.

The `tint_by_a_color_and_lift` shader scales each pixel of a known ramp by its `tint_color`
and adds its `lift`, a Vector3, to red, green and blue. Each part reaches the shader as a push
constant, so a picture that changes part by part shows every part arrived where it belongs.
"""

from collections.abc import Callable

import pytest

import shader_node_streams
from conftest import TatolabdUnderTest
from shader_node_probes import (
    HALF_RED_QUARTER_GREEN,
    SAMPLED_COLUMNS,
    known_pixel_at_column,
)

pytestmark = pytest.mark.requires_gpu

#: How far a channel of an 8-bit pixel may round away from the exact value.
ONE_STEP_OF_AN_8_BIT_CHANNEL = 1

#: Reports the sink may make before a value set or linked reaches the shader.
REPORTS_AWAITED_FOR_A_VALUE = 600

#: The Color set live on the shader's `tint_color`, as the local API takes it.
COLOR_SET_LIVE = {"red": 0.25, "green": 0.5, "blue": 1.0, "alpha": 1.0}

#: The Vector3 set live on the shader's `lift`, as the local API takes it.
LIFT_SET_LIVE = {"x": 0.0, "y": 0.125, "z": 0.25}

PixelOf = Callable[[int, int, int, int], "list[float]"]


def _shaded_as(color: "dict[str, float]", lift: "dict[str, float]") -> PixelOf:
    def shaded_pixel(red: int, green: int, blue: int, alpha: int) -> "list[float]":
        return [
            min(255.0, red * color["red"] + 255 * lift["x"]),
            min(255.0, green * color["green"] + 255 * lift["y"]),
            min(255.0, blue * color["blue"] + 255 * lift["z"]),
            alpha * color["alpha"],
        ]

    return shaded_pixel


def _await_a_frame_shaded_as(
    tatolabd: TatolabdUnderTest, shaded_pixel_of: PixelOf, *, from_occurrence: int, what: str
) -> int:
    """The occurrence of the first report from `from_occurrence` on whose sampled pixels are all
    `shaded_pixel_of` the source's, failing with the last report when none is."""
    report = None
    for occurrence in range(from_occurrence, from_occurrence + REPORTS_AWAITED_FOR_A_VALUE):
        report = tatolabd.await_marker("PROBE_RESULT", occurrence=occurrence)
        if isinstance(report, dict) and "pixels_by_column" in report and all(
            all(
                abs(shaded_channel - expected_channel) <= ONE_STEP_OF_AN_8_BIT_CHANNEL
                for shaded_channel, expected_channel in zip(
                    report["pixels_by_column"][str(column)],
                    shaded_pixel_of(*known_pixel_at_column(column)),
                )
            )
            for column in SAMPLED_COLUMNS
        ):
            return occurrence
    pytest.fail(f"no frame was shaded with {what}; the last report was {report}\n{tatolabd.stderr_text}")


def test_a_color_and_a_vector_set_live_on_a_shader_node_change_the_picture_it_writes(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(
        shader_node_streams.source_into_a_color_tint_into_sink
    )
    stream_id = tatolabd.await_the_latest_stream_loaded()
    local_api = tatolabd.local_api_client()
    occurrence = _await_a_frame_shaded_as(
        tatolabd,
        _shaded_as({"red": 1.0, "green": 1.0, "blue": 1.0, "alpha": 1.0}, {"x": 0, "y": 0, "z": 0}),
        from_occurrence=1,
        what="the white Color and zero lift the shader declares as its defaults",
    )
    (tint,) = [
        node
        for node in local_api.call_tool("graph", {"stream": stream_id})["nodes"]
        if node["name"] == "tint"
    ]
    input_pin_types = {port["name"]: port.get("pin_type") for port in tint["ports"]["inputs"]}
    assert input_pin_types["tint_color"] == {"type": "color"}, tint
    assert input_pin_types["lift"] == {"type": "vector", "component_count": "three"}, tint

    def set_live(input_name: str, value: "dict[str, float]") -> None:
        local_api.call_tool(
            "set_node_input_value",
            {"stream": stream_id, "node": tint["id"], "input": input_name, "value": value, "mode": "live"},
        )

    set_live("tint_color", COLOR_SET_LIVE)
    occurrence = _await_a_frame_shaded_as(
        tatolabd,
        _shaded_as(COLOR_SET_LIVE, {"x": 0, "y": 0, "z": 0}),
        from_occurrence=occurrence + 1,
        what=f"the Color set live, {COLOR_SET_LIVE}",
    )
    set_live("lift", LIFT_SET_LIVE)
    _await_a_frame_shaded_as(
        tatolabd,
        _shaded_as(COLOR_SET_LIVE, LIFT_SET_LIVE),
        from_occurrence=occurrence + 1,
        what=f"the Color set live and the lift set live, {LIFT_SET_LIVE}",
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()


def test_a_function_node_linked_into_a_color_input_of_a_shader_node_sets_it(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(
        shader_node_streams.function_node_into_the_color_of_a_tint
    )
    linked_color = HALF_RED_QUARTER_GREEN.stored_value()
    _await_a_frame_shaded_as(
        tatolabd,
        _shaded_as(linked_color, {"x": 0, "y": 0, "z": 0}),
        from_occurrence=1,
        what=f"the Color the function node hands out, {linked_color}",
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
