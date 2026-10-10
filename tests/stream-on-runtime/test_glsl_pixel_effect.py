# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A pixel effect written as one GLSL function runs on the GPU from a processor
interpreter beneath `tatolabd`, and the compiler and the engine copy refuse at the user's own line.

Every probe runs in a processor interpreter fed by a native `TestPatternSource`,
and reports one `MARKER:PROBE_RESULT` JSON line; each test starts the
scenario's stream on `tatolabd` and asserts on that line.
"""

from collections.abc import Callable

import pytest

import glsl_pixel_effect_probes
import glsl_pixel_effect_streams
from runtime_process_under_test import RuntimeProcessUnderTest

pytestmark = pytest.mark.requires_gpu


def run_scenario(
    start_tatolabd_running_stream: "Callable[..., RuntimeProcessUnderTest]", scenario: str
) -> dict:
    tatolabd = start_tatolabd_running_stream(glsl_pixel_effect_streams.STREAM_BY_SCENARIO[scenario])
    observation = tatolabd.await_marker("PROBE_RESULT")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert isinstance(observation, dict), f"no parseable probe result:\n{tatolabd.stderr_text}"
    if "failure" in observation:
        pytest.fail(f"the probe raised in its processor interpreter:\n{observation['failure']}")
    return observation


def test_an_invert_effect_with_a_strength_dial_outputs_255_minus_the_source(
    start_tatolabd_running_stream,
):
    observed = run_scenario(start_tatolabd_running_stream, "invert")

    assert observed["source_is_not_uniform"], "a uniform frame would match by accident"
    assert observed["output_extent"] == [
        glsl_pixel_effect_probes.FRAME_WIDTH,
        glsl_pixel_effect_probes.FRAME_HEIGHT,
    ]
    assert observed["timestamp_carried"]
    assert observed["mismatched_pixels"] == 0


def test_the_invert_check_fails_for_an_identity_effect(start_tatolabd_running_stream):
    """The negative control: an effect that returns its source must not pass
    the check above."""
    observed = run_scenario(start_tatolabd_running_stream, "invert_negative_control")

    assert observed["mismatched_pixels"] > 0


def test_the_pre_declared_extent_and_sampling_helpers_read_the_source(
    start_tatolabd_running_stream,
):
    observed = run_scenario(start_tatolabd_running_stream, "PreDeclaredHelpersProbe")

    assert observed["mirror_through_texel_helper"] == 0
    assert observed["clamped_past_the_right_edge"] == 0
    assert observed["texel_centres_through_uv_helper"] == 0


def test_every_dial_type_reaches_the_shader_at_its_std430_offset(start_tatolabd_running_stream):
    observed = run_scenario(start_tatolabd_running_stream, "EveryDialTypeProbe")

    assert observed["distinct_pixels"] == [
        glsl_pixel_effect_probes.EVERY_DIAL_TYPE_EXPECTED_PIXEL
    ]


def test_a_compiler_diagnostic_names_the_line_of_the_users_body(start_tatolabd_running_stream):
    observed = run_scenario(start_tatolabd_running_stream, "CompilerDiagnosticLineProbe")

    assert "no_such_function" in observed["diagnostic"]
    assert observed["reported_lines"] == [
        glsl_pixel_effect_probes.UNDEFINED_FUNCTION_LINE
    ], observed["diagnostic"]


def test_a_frame_the_copy_refuses_is_refused_naming_the_frame(start_tatolabd_running_stream):
    observed = run_scenario(start_tatolabd_running_stream, "CopyRefusedFrameProbe")

    assert "GlslPixelEffect.apply_to_frame" in observed["refusal"]
    assert observed["bgra_surface_id"] in observed["refusal"]
    assert "format mismatch" in observed["refusal"]
