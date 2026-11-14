# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""A `@node.shader` on `tatolabd`: its Slang runs on the GPU in the runtime, with no process of its own.

A Python source publishes frames whose every pixel is known from its own
texture ring, the `tint` shader node scales their colour, and a Python sink
reports the pixels it wrote. The shader's input arrives from another process
and its output leaves for another, so the frame crosses both ways through the
GPU pools' claim tables.
"""

from collections.abc import Callable

import pytest

import shader_node_streams
from conftest import TatolabdUnderTest
from helper_process_observation import helper_process_ids_started_in
from shader_node_probes import (
    FRAME_HEIGHT,
    FRAME_WIDTH,
    QUARTER_AMOUNT,
    SAMPLED_COLUMNS,
    known_pixel_at_column,
)
from shader_node_shaders import TINT_AMOUNT

pytestmark = pytest.mark.requires_gpu

#: How far a channel of an 8-bit pixel may round away from the exact product.
ONE_STEP_OF_AN_8_BIT_CHANNEL = 1

#: Reports the sink may make before the function node pool's first value reaches the shader.
REPORTS_AWAITED_FOR_THE_LINKED_AMOUNT = 600

#: The source and the sink: the only nodes of the stream with a process of their own.
HELPER_PROCESSES_OF_THE_PYTHON_SOURCE_AND_SINK = 2


def test_a_shader_node_tints_each_frame_on_the_gpu_with_no_process_of_its_own(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(
        shader_node_streams.rgba_source_into_tint_into_sink
    )
    report = tatolabd.await_marker("PROBE_RESULT")
    stderr_text = tatolabd.stderr_text
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert isinstance(report, dict), f"no parseable probe result:\n{stderr_text}"
    assert "failure" not in report, report.get("failure")
    assert report["extent"] == [FRAME_WIDTH, FRAME_HEIGHT]
    assert report["format"] == "rgba8_unorm"
    for column in SAMPLED_COLUMNS:
        red, green, blue, alpha = known_pixel_at_column(column)
        expected = [red * TINT_AMOUNT, green * TINT_AMOUNT, blue * TINT_AMOUNT, alpha]
        shaded = report["pixels_by_column"][str(column)]
        assert all(
            abs(shaded_channel - expected_channel) <= ONE_STEP_OF_AN_8_BIT_CHANNEL
            for shaded_channel, expected_channel in zip(shaded, expected)
        ), f"column {column}: the shader wrote {shaded}, and the tint of the source is {expected}"

    assert len(helper_process_ids_started_in(stderr_text)) == (
        HELPER_PROCESSES_OF_THE_PYTHON_SOURCE_AND_SINK
    ), "the shader node runs in the runtime, so only the Python source and sink start a helper"


def test_a_shader_node_refuses_a_bgra_image_naming_its_format_and_the_conversion(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(
        shader_node_streams.bgra_source_into_tint_into_sink
    )
    refusal_line = tatolabd.await_stderr_containing("shader node wrote no frame")
    stderr_text = tatolabd.stderr_text
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert "bgra8_unorm" in refusal_line, refusal_line
    assert "input `image`" in refusal_line, refusal_line
    assert "converts its Image to rgba8_unorm" in refusal_line, refusal_line
    assert "MARKER:PROBE_RESULT" not in stderr_text, (
        "a refused frame writes nothing, so the sink reads nothing"
    )


def _is_within_one_step(shaded: "list[int]", expected: "list[float]") -> bool:
    return all(
        abs(shaded_channel - expected_channel) <= ONE_STEP_OF_AN_8_BIT_CHANNEL
        for shaded_channel, expected_channel in zip(shaded, expected)
    )


@pytest.mark.parametrize(
    ("stream_name", "shaded_pixel_of"),
    [
        (
            "function_node_into_the_amount_of_a_tint",
            lambda red, green, blue, alpha: [
                red * QUARTER_AMOUNT,
                green * QUARTER_AMOUNT,
                blue * QUARTER_AMOUNT,
                alpha,
            ],
        ),
        (
            "function_node_into_the_amount_of_a_tint_fused_into_invert",
            lambda red, green, blue, alpha: [
                255 - red * QUARTER_AMOUNT,
                255 - green * QUARTER_AMOUNT,
                255 - blue * QUARTER_AMOUNT,
                alpha,
            ],
        ),
    ],
)
def test_a_function_node_linked_into_a_float_input_of_a_shader_node_sets_it(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
    stream_name: str,
    shaded_pixel_of,
):
    tatolabd = start_tatolabd_running_stream(getattr(shader_node_streams, stream_name))
    reports = []
    for occurrence in range(1, REPORTS_AWAITED_FOR_THE_LINKED_AMOUNT + 1):
        report = tatolabd.await_marker("PROBE_RESULT", occurrence=occurrence)
        reports.append(report)
        if isinstance(report, dict) and "pixels_by_column" in report and all(
            _is_within_one_step(
                report["pixels_by_column"][str(column)],
                shaded_pixel_of(*known_pixel_at_column(column)),
            )
            for column in SAMPLED_COLUMNS
        ):
            break
    else:
        pytest.fail(
            f"no frame was shaded with the amount the function node hands out; the last "
            f"report was {reports[-1]}\n{tatolabd.stderr_text}"
        )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()


def test_a_shader_node_reading_only_the_extent_of_its_image_writes_frames(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(
        shader_node_streams.source_into_a_shader_reading_only_the_extent_of_its_image
    )
    report = tatolabd.await_marker("PROBE_RESULT")
    stderr_text = tatolabd.stderr_text
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert isinstance(report, dict), f"no parseable probe result:\n{stderr_text}"
    assert "failure" not in report, report.get("failure")
    assert report["extent"] == [FRAME_WIDTH, FRAME_HEIGHT]
    for column in SAMPLED_COLUMNS:
        expected = [(column + 0.5) / FRAME_WIDTH * 255, 0, 0, 255]
        shaded = report["pixels_by_column"][str(column)]
        assert _is_within_one_step(shaded, expected), (
            f"column {column}: the shader wrote {shaded}, and the ramp there is {expected}"
        )
    assert "shader node wrote no frame" not in stderr_text, stderr_text
