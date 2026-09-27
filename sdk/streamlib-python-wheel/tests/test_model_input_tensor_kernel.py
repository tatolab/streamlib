# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A model's input tensor is prepared on the GPU from a helper process, and
matches a torch reference of the same parameters for every fit, layout and
dtype.

Every probe runs in a helper process fed by a native `TestPatternSource`, and
reports one `MARKER:PROBE_RESULT` JSON line; a rig whose torch sees no device
to read the tensor on skips.
"""

import json
import re
from pathlib import Path

import pytest

from model_input_tensor_kernel_probes import (
    FIT_CASES,
    NATURAL_TORCH_DEVICE_TYPE,
    TOLERATED_ERROR_IN_PIXEL_LEVELS,
    matrix_case_name,
    matrix_cases_for_fit,
)

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "model_input_tensor_kernel_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_scenario(start_app_under_test, scenario: str, *arguments: str) -> dict:
    app = start_app_under_test(APP, scenario, *arguments)
    app.await_output_containing("MARKER:PROBE_RESULT", f"the {scenario} result")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
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
    start_app_under_test, fit_case_name: str
):
    observed = run_scenario(start_app_under_test, "matrix", fit_case_name)

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
    start_app_under_test,
):
    """The negative control: a mean a tenth off must not pass the check above."""
    observed = run_scenario(start_app_under_test, "matrix_with_the_wrong_mean", "letterbox")

    for name, case in observed["cases"].items():
        assert case["max_error_in_pixel_levels"] > TOLERATED_ERROR_IN_PIXEL_LEVELS, name


def test_a_bgra_frame_and_a_tensor_surface_are_refused_by_name(start_app_under_test):
    observed = run_scenario(start_app_under_test, "NonRgbaSourceRefusalProbe")

    assert "ModelInputTensorKernel.apply_to_surface" in observed["bgra_refusal"]
    assert observed["bgra_surface_id"] in observed["bgra_refusal"]
    assert "'bgra32'" in observed["bgra_refusal"]
    assert "RGBA" in observed["bgra_refusal"]
    assert observed["tensor_surface_id"] in observed["tensor_refusal"]
    assert "tensor" in observed["tensor_refusal"]
