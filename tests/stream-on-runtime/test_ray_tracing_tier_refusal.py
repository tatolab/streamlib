# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Ray tracing is a typed absent tier where the device has none.

MoltenVK advertises no `VK_KHR_ray_tracing_pipeline`, so on macOS every
ray-tracing constructor refuses at `setup()` naming the tier — and so does a
Linux device without the extension. A device that has the tier is out of this
test's scope; `test_ray_tracing_kernel.py` covers it.
"""

import pytest

import ray_tracing_kernel_streams
from ray_tracing_kernel_probes import RAY_TRACING_TIER_ABSENT

pytestmark = pytest.mark.requires_gpu


def test_every_ray_tracing_constructor_refuses_at_setup_naming_the_absent_tier(
    start_tatolabd_running_stream,
):
    tatolabd = start_tatolabd_running_stream(
        ray_tracing_kernel_streams.ray_tracing_tier_absent_refusal_probe_alone
    )
    observation = tatolabd.await_marker("PROBE_RESULT")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()
    assert isinstance(observation, dict), f"no parseable probe result:\n{tatolabd.stderr_text}"
    if "failure" in observation:
        pytest.fail(f"the probe raised in its processor interpreter:\n{observation['failure']}")
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
