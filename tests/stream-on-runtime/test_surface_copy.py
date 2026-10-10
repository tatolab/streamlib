# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Python processor lands a frame in a kernel's input texture through the
engine's surface-to-surface copy, with no array library.

Every probe runs in a processor interpreter fed by a native `TestPatternSource`,
and reports one `MARKER:PROBE_RESULT` JSON line; each test starts the
scenario's stream on `tatolabd` and asserts on that line.
"""

from collections.abc import Callable

import pytest

import surface_copy_streams
from runtime_process_under_test import RuntimeProcessUnderTest

pytestmark = pytest.mark.requires_gpu


def run_scenario(
    start_tatolabd_running_stream: "Callable[..., RuntimeProcessUnderTest]", scenario: str
) -> dict:
    tatolabd = start_tatolabd_running_stream(surface_copy_streams.STREAM_BY_SCENARIO[scenario])
    observation = tatolabd.await_marker("PROBE_RESULT")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert isinstance(observation, dict), f"no parseable probe result:\n{tatolabd.stderr_text}"
    if "failure" in observation:
        pytest.fail(f"the probe raised in its processor interpreter:\n{observation['failure']}")
    return observation


def test_a_frame_lands_in_a_kernel_input_texture_with_no_array_library(
    start_tatolabd_running_stream,
):
    """Copy the pattern's `rgba` frame into an `rgba8_unorm` texture, invert
    it with a kernel, and compare every pixel with the inverted frame."""
    observed = run_scenario(start_tatolabd_running_stream, "frame_landing")

    assert observed["array_libraries_imported"] == []
    assert observed["source_is_not_uniform"], "a uniform frame would match by accident"
    assert observed["mismatched_pixels"] == 0


def test_the_landing_check_fails_when_the_copy_is_skipped(start_tatolabd_running_stream):
    """The negative control: a kernel reading a texture nothing was copied
    into must not pass the check above."""
    observed = run_scenario(start_tatolabd_running_stream, "frame_landing_negative_control")

    assert observed["mismatched_pixels"] > 0


def test_a_copy_between_mismatched_surfaces_is_refused_naming_why(start_tatolabd_running_stream):
    observed = run_scenario(start_tatolabd_running_stream, "CopyRefusalProbe")

    assert "format mismatch" in observed["format_mismatch"]
    assert "extent mismatch" in observed["extent_mismatch"]
