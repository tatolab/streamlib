# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A pixel effect written as one GLSL function runs on the GPU from a helper
process, and the compiler and the engine copy refuse at the user's own line.

Every probe runs in a helper process fed by a native `TestPatternSource`, and
reports one `MARKER:PROBE_RESULT` JSON line.
"""

import json
import re
from pathlib import Path

import pytest

import glsl_pixel_effect_probes

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "glsl_pixel_effect_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def run_scenario(start_app_under_test, scenario: str) -> dict:
    app = start_app_under_test(APP, scenario)
    app.await_output_containing("MARKER:PROBE_RESULT", f"the {scenario} result")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
    return observation


def test_an_invert_effect_with_a_strength_dial_outputs_255_minus_the_source(
    start_app_under_test,
):
    observed = run_scenario(start_app_under_test, "invert")

    assert observed["source_is_not_uniform"], "a uniform frame would match by accident"
    assert observed["output_extent"] == [
        glsl_pixel_effect_probes.FRAME_WIDTH,
        glsl_pixel_effect_probes.FRAME_HEIGHT,
    ]
    assert observed["timestamp_carried"]
    assert observed["mismatched_pixels"] == 0


def test_the_invert_check_fails_for_an_identity_effect(start_app_under_test):
    """The negative control: an effect that returns its source must not pass
    the check above."""
    observed = run_scenario(start_app_under_test, "invert_negative_control")

    assert observed["mismatched_pixels"] > 0


def test_the_pre_declared_extent_and_sampling_helpers_read_the_source(
    start_app_under_test,
):
    observed = run_scenario(start_app_under_test, "PreDeclaredHelpersProbe")

    assert observed["mirror_through_texel_helper"] == 0
    assert observed["clamped_past_the_right_edge"] == 0
    assert observed["texel_centres_through_uv_helper"] == 0


def test_a_compiler_diagnostic_names_the_line_of_the_users_body(start_app_under_test):
    observed = run_scenario(start_app_under_test, "CompilerDiagnosticLineProbe")

    assert "no_such_function" in observed["diagnostic"]
    assert observed["reported_lines"] == [
        glsl_pixel_effect_probes.UNDEFINED_FUNCTION_LINE
    ], observed["diagnostic"]


def test_a_frame_the_copy_refuses_is_refused_naming_the_frame(start_app_under_test):
    observed = run_scenario(start_app_under_test, "CopyRefusedFrameProbe")

    assert "GlslPixelEffect.apply_to_frame" in observed["refusal"]
    assert observed["bgra_surface_id"] in observed["refusal"]
    assert "format mismatch" in observed["refusal"]
