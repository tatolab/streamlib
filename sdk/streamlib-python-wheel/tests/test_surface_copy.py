# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Python processor lands a frame in a kernel's input texture through the
engine's surface-to-surface copy, with no array library.

Every probe runs in a helper process fed by a native `TestPatternSource`, and
reports one `MARKER:PROBE_RESULT` JSON line.
"""

import json
import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "surface_copy_app.py"

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


def test_a_frame_lands_in_a_kernel_input_texture_with_no_array_library(
    start_app_under_test,
):
    """Copy the pattern's `rgba` frame into an `rgba8_unorm` texture, invert
    it with a kernel, and compare every pixel with the inverted frame."""
    observed = run_scenario(start_app_under_test, "frame_landing")

    assert observed["array_libraries_imported"] == []
    assert observed["source_is_not_uniform"], "a uniform frame would match by accident"
    assert observed["mismatched_pixels"] == 0


def test_the_landing_check_fails_when_the_copy_is_skipped(start_app_under_test):
    """The negative control: a kernel reading a texture nothing was copied
    into must not pass the check above."""
    observed = run_scenario(start_app_under_test, "frame_landing_negative_control")

    assert observed["mismatched_pixels"] > 0


def test_a_copy_between_mismatched_surfaces_is_refused_naming_why(start_app_under_test):
    observed = run_scenario(start_app_under_test, "CopyRefusalProbe")

    assert "format mismatch" in observed["format_mismatch"]
    assert "extent mismatch" in observed["extent_mismatch"]
