# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A model's input tensor is prepared on the GPU from a processor interpreter
beneath `tatolabd`, and matches a torch reference of the same parameters for every fit, layout and
dtype.

Every probe runs in a processor interpreter fed by a native `TestPatternSource`,
and reports one `MARKER:PROBE_RESULT` JSON line; each test starts the
scenario's stream on `tatolabd` and asserts on that line. A rig whose torch
sees no device to read the tensor on skips.
"""

from collections.abc import Callable
from typing import Any

import pytest

import model_input_tensor_kernel_streams
from model_input_tensor_kernel_probes import (
    FIT_CASES,
    SECOND_FIT_CASE,
    NATURAL_TORCH_DEVICE_TYPE,
    TOLERATED_ERROR_IN_PIXEL_LEVELS,
    matrix_case_name,
    matrix_cases_for_fit,
)
from runtime_process_under_test import RuntimeProcessUnderTest

pytestmark = pytest.mark.requires_gpu


def run_scenario(
    start_tatolabd_running_stream: "Callable[..., RuntimeProcessUnderTest]",
    scenario_stream: "Callable[..., Any]",
) -> dict:
    tatolabd = start_tatolabd_running_stream(scenario_stream)
    observation = tatolabd.await_marker("PROBE_RESULT")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert isinstance(observation, dict), f"no parseable probe result:\n{tatolabd.stderr_text}"
    if "failure" in observation:
        pytest.fail(f"the probe raised in its processor interpreter:\n{observation['failure']}")
    if "torch_device_unavailable" in observation:
        pytest.skip(observation["torch_device_unavailable"])
    return observation


def expected_shape(fit_case_name: str, layout: str) -> "list[int]":
    tensor_width, tensor_height = FIT_CASES[fit_case_name]["expected_tensor_extent"]
    if layout == "nchw":
        return [1, 3, tensor_height, tensor_width]
    return [1, tensor_height, tensor_width, 3]


@pytest.mark.parametrize("fit_case_name", list(FIT_CASES))
def test_every_layout_and_dtype_of_a_fit_matches_the_torch_reference(
    start_tatolabd_running_stream, fit_case_name: str
):
    observed = run_scenario(
        start_tatolabd_running_stream,
        model_input_tensor_kernel_streams.MATRIX_STREAM_BY_FIT_CASE_NAME[fit_case_name],
    )

    assert observed["source_is_not_uniform"], "a uniform frame would match by accident"
    cases = matrix_cases_for_fit(fit_case_name)
    assert sorted(observed["cases"]) == sorted(matrix_case_name(*case) for case in cases)
    for _, layout, dtype, channel_order in cases:
        name = matrix_case_name(fit_case_name, layout, dtype, channel_order)
        case = observed["cases"][name]
        assert case["geometry"] == FIT_CASES[fit_case_name]["expected_geometry"], name
        assert case["stated_shape"] == expected_shape(fit_case_name, layout), name
        assert case["tensor_shape"] == expected_shape(fit_case_name, layout), name
        assert case["stated_dtype"] == dtype, name
        assert case["tensor_dtype"] == f"torch.{dtype}", name
        assert case["tensor_device"].startswith(NATURAL_TORCH_DEVICE_TYPE), name
        assert case["max_error_in_pixel_levels"] <= TOLERATED_ERROR_IN_PIXEL_LEVELS, (
            name,
            case["max_error_in_pixel_levels"],
        )


def test_the_comparison_fails_for_a_kernel_compiled_with_the_wrong_mean(
    start_tatolabd_running_stream,
):
    """The negative control: a mean a tenth off must not pass the check above."""
    observed = run_scenario(
        start_tatolabd_running_stream,
        model_input_tensor_kernel_streams.a_test_pattern_into_the_letterbox_matrix_probe_with_the_wrong_mean,
    )

    for name, case in observed["cases"].items():
        assert case["max_error_in_pixel_levels"] > TOLERATED_ERROR_IN_PIXEL_LEVELS, name


def test_a_bgra_frame_and_a_tensor_surface_are_refused_by_name(start_tatolabd_running_stream):
    observed = run_scenario(
        start_tatolabd_running_stream,
        model_input_tensor_kernel_streams.a_test_pattern_into_the_non_rgba_source_refusal_probe,
    )

    assert "ModelInputTensorKernel.apply_to_surface" in observed["bgra_refusal"]
    assert observed["bgra_surface_id"] in observed["bgra_refusal"]
    assert "'bgra32'" in observed["bgra_refusal"]
    assert "RGBA" in observed["bgra_refusal"]
    assert observed["tensor_surface_id"] in observed["tensor_refusal"]
    assert "tensor" in observed["tensor_refusal"]


def test_a_pad_bottom_right_tensor_follows_its_frame_across_an_extent_change(
    start_tatolabd_running_stream,
):
    observed = run_scenario(
        start_tatolabd_running_stream,
        model_input_tensor_kernel_streams.a_test_pattern_into_the_pad_bottom_right_extent_change_probe,
    )

    applies = observed["applies"]
    assert [apply["tensor_shape"] for apply in applies] == [
        [1, 3, 48, 64],
        [1, 3, *reversed(SECOND_FIT_CASE["expected_tensor_extent"])],
        [1, 3, 48, 64],
    ]
    assert [apply["geometry"] for apply in applies] == [
        FIT_CASES["pad_bottom_right_to_multiple_of_16"]["expected_geometry"],
        SECOND_FIT_CASE["expected_geometry"],
        FIT_CASES["pad_bottom_right_to_multiple_of_16"]["expected_geometry"],
    ]
    for apply in applies:
        assert apply["max_error_in_pixel_levels"] <= TOLERATED_ERROR_IN_PIXEL_LEVELS, apply
