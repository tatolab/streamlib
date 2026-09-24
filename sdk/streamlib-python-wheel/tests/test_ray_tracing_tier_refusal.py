# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Ray tracing is a typed absent tier where the device has none.

MoltenVK advertises no `VK_KHR_ray_tracing_pipeline`, so on macOS every
ray-tracing constructor refuses at `setup()` naming the tier — and so does a
Linux device without the extension. A device that has the tier is out of this
test's scope; `test_ray_tracing_kernel.py` covers it.
"""

import json
import re
from pathlib import Path

import pytest

from ray_tracing_kernel_probes import RAY_TRACING_TIER_ABSENT

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "ray_tracing_kernel_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")


def test_every_ray_tracing_constructor_refuses_at_setup_naming_the_absent_tier(
    start_app_under_test,
):
    app = start_app_under_test(APP, "RayTracingTierAbsentRefusalProbe")
    app.await_output_containing(
        "MARKER:PROBE_RESULT", "the RayTracingTierAbsentRefusalProbe result"
    )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
    if observation.get("tier_present"):
        pytest.skip("this device has the ray-tracing tier; test_ray_tracing_kernel covers it")

    refusals = observation["refusals"]
    assert set(refusals) == {"build_triangles_blas", "create_ray_tracing_kernel"}, (
        f"a device without the tier must refuse every constructor: {refusals!r}"
    )
    for constructor, refusal in refusals.items():
        assert RAY_TRACING_TIER_ABSENT in refusal, (
            f"{constructor} must refuse naming the absent tier: {refusal!r}"
        )
        assert "VK_KHR_ray_tracing_pipeline" in refusal, (
            f"{constructor} must name the extension the tier is: {refusal!r}"
        )
