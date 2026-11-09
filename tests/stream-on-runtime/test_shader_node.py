# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

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
    SAMPLED_COLUMNS,
    known_pixel_at_column,
)
from shader_node_shaders import TINT_AMOUNT

pytestmark = pytest.mark.requires_gpu

#: How far a channel of an 8-bit pixel may round away from the exact product.
ONE_STEP_OF_AN_8_BIT_CHANNEL = 1

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
