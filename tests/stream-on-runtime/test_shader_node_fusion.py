# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A chain of `@node.shader` nodes on `tatolabd` fuses into one kernel, and splits only where it must.

A Python source publishes frames whose every pixel is known, a chain of shader
nodes shades them, and a Python sink reports the pixels the chain wrote. The
same chain runs fused, and unfused by a second reader of each intermediate
Image; both write the pixels the chain computes, exactly. The live graph's
`fused_group` counts the submissions and dispatches each evaluation recorded.
"""

from collections.abc import Callable
from typing import Any

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

#: How far a channel of an 8-bit pixel may round away from the exact value.
ONE_STEP_OF_AN_8_BIT_CHANNEL = 1

#: The source and the sink: the only nodes of a fused chain with a process of their own.
HELPER_PROCESSES_OF_THE_PYTHON_SOURCE_AND_SINK = 2

POINT_WISE_CHAIN_NODE_NAMES = ["tint", "invert", "swap_red_and_blue"]


def point_wise_chain_pixel_at_column(column: int) -> "list[int]":
    """What tint, then invert, then swap red and blue writes at `column`: exact in 8 bits."""
    red, green, blue, alpha = known_pixel_at_column(column)
    tinted = [round(channel * TINT_AMOUNT) for channel in (red, green, blue)]
    inverted_red, inverted_green, inverted_blue = (255 - channel for channel in tinted)
    return [inverted_blue, inverted_green, inverted_red, alpha]


def blurred_chain_pixel_at_column(column: int) -> "list[int]":
    """What tint, then a one-column horizontal blur clamped at the edge, then invert writes."""

    def tinted(at_column: int) -> "list[int]":
        clamped_column = min(max(at_column, 0), FRAME_WIDTH - 1)
        red, green, blue, _ = known_pixel_at_column(clamped_column)
        return [round(channel * TINT_AMOUNT) for channel in (red, green, blue)]

    left, right = tinted(column - 1), tinted(column + 1)
    blurred = [(left_channel + right_channel) / 2 for left_channel, right_channel in zip(left, right)]
    return [round(255 - channel) for channel in blurred] + [known_pixel_at_column(column)[3]]


def run_the_stream_until_the_sink_reports(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]", stream
) -> "tuple[dict[str, Any], dict[str, Any], str]":
    """Run `stream`, take the sink's first report and the live graph, then stop."""
    tatolabd = start_tatolabd_running_stream(stream)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    report = tatolabd.await_marker("PROBE_RESULT")
    graph = tatolabd.local_api_client().graph(stream=stream_id)
    stderr_text = tatolabd.stderr_text
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert isinstance(report, dict), f"no parseable probe result:\n{stderr_text}"
    assert "failure" not in report, report.get("failure")
    assert report["extent"] == [FRAME_WIDTH, FRAME_HEIGHT]
    assert report["format"] == "rgba8_unorm"
    return report, graph, stderr_text


def fused_group_of(graph: "dict[str, Any]", name: str) -> "dict[str, Any]":
    """The `fused_group` of the one node named `name`."""
    matches = [node for node in graph["nodes"] if node["name"] == name]
    assert len(matches) == 1, [node["name"] for node in graph["nodes"]]
    return matches[0]["components"]["fused_group"]


def assert_the_sink_read(report: "dict[str, Any]", pixel_at_column) -> None:
    for column in SAMPLED_COLUMNS:
        expected = pixel_at_column(column)
        shaded = report["pixels_by_column"][str(column)]
        assert all(
            abs(shaded_channel - expected_channel) <= ONE_STEP_OF_AN_8_BIT_CHANNEL
            for shaded_channel, expected_channel in zip(shaded, expected)
        ), f"column {column}: the chain wrote {shaded}, and it computes {expected}"


def test_a_point_wise_chain_runs_as_one_kernel_in_one_submission_per_frame(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    report, graph, stderr_text = run_the_stream_until_the_sink_reports(
        start_tatolabd_running_stream, shader_node_streams.point_wise_chain_fused
    )

    for column in SAMPLED_COLUMNS:
        assert report["pixels_by_column"][str(column)] == point_wise_chain_pixel_at_column(column)
    fused_group = fused_group_of(graph, "swap_red_and_blue")
    assert fused_group["evaluated_by"] == "swap_red_and_blue"
    assert fused_group["members"] == POINT_WISE_CHAIN_NODE_NAMES
    assert fused_group["kernels"] == [POINT_WISE_CHAIN_NODE_NAMES]
    assert fused_group["submissions"] >= 1
    assert fused_group["dispatches"] == fused_group["submissions"], fused_group
    for member in POINT_WISE_CHAIN_NODE_NAMES:
        assert fused_group_of(graph, member) == fused_group
    assert len(helper_process_ids_started_in(stderr_text)) == (
        HELPER_PROCESSES_OF_THE_PYTHON_SOURCE_AND_SINK
    ), "the chain runs in the runtime, so only the Python source and sink start a helper"


def test_the_unfused_chain_writes_the_same_pixels_one_submission_per_node(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    report, graph, _ = run_the_stream_until_the_sink_reports(
        start_tatolabd_running_stream,
        shader_node_streams.point_wise_chain_unfused_by_a_second_reader_of_each_intermediate,
    )

    for column in SAMPLED_COLUMNS:
        assert report["pixels_by_column"][str(column)] == point_wise_chain_pixel_at_column(column)
    for member in POINT_WISE_CHAIN_NODE_NAMES:
        fused_group = fused_group_of(graph, member)
        assert fused_group["members"] == [member], fused_group
        assert fused_group["dispatches"] == fused_group["submissions"], fused_group


def test_a_blur_in_the_middle_splits_the_chain_into_two_kernels_of_one_submission(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    report, graph, _ = run_the_stream_until_the_sink_reports(
        start_tatolabd_running_stream, shader_node_streams.chain_with_a_blur_in_the_middle
    )

    assert_the_sink_read(report, blurred_chain_pixel_at_column)
    fused_group = fused_group_of(graph, "invert")
    assert fused_group["members"] == ["tint", "horizontal_blur", "invert"]
    assert fused_group["kernels"] == [["tint"], ["horizontal_blur", "invert"]]
    assert fused_group["submissions"] >= 1
    assert fused_group["dispatches"] == 2 * fused_group["submissions"], fused_group
