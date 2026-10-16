# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.DisplayWindow` on the runtime's bundled software device.

The bundled lavapipe is built headless, so the instance it gives the engine has
no window-system surface extension. A window there refuses at the node's setup,
by name, and `tatolabd` and the stream's other nodes keep running. The arm
needs a window server reachable — with none the window never reaches a surface
call — so it is rig-only; the GPU lane has no display.
"""

import os
import time
from collections.abc import Callable
from pathlib import Path

import pytest

from conftest import TatolabdUnderTest
from display_window_streams import (
    PATTERN_NODE_NAME,
    WINDOW_NODE_NAME,
    WINDOW_TITLE,
    a_test_pattern_into_a_display_window,
)
from runtime_unit_under_test import RuntimeUnitUnderTest

READINESS_TIMEOUT_SECONDS = 30.0

#: How long the stream keeps running after the refusal before it is read again:
#: long enough for the pattern to produce frames the refused window never takes.
STILL_RUNNING_AFTER_THE_REFUSAL_SECONDS = 2.0

#: The bundled driver's ICD manifest, relative to the lend.
BUNDLED_VULKAN_DRIVER_DIRECTORY_RELATIVE_TO_THE_LEND = Path("tatolab/runtime/_vulkan_driver")

#: The loader's message when a command an unenabled extension owns is called —
#: the abort this test exists to keep away.
UNLOADED_SURFACE_COMMAND_PANIC = "could not load vkCreate"


def the_bundled_software_driver_manifest(runtime_unit: RuntimeUnitUnderTest) -> Path:
    """The lend's lavapipe ICD manifest; a Linux unit always carries exactly one."""
    manifests = sorted(
        (runtime_unit.lend_directory / BUNDLED_VULKAN_DRIVER_DIRECTORY_RELATIVE_TO_THE_LEND).glob(
            "lvp_icd.*.json"
        )
    )
    assert len(manifests) == 1, f"the lend must carry one lavapipe manifest: {manifests}"
    return manifests[0]


@pytest.mark.requires_gpu
@pytest.mark.rig_only_capability(reason="a display")
@pytest.mark.linux_only_capability(reason="DISPLAY and WAYLAND_DISPLAY are how Linux names a window server")
@pytest.mark.skipif(
    not (os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")),
    reason="display tier — the refusal is proven where a window server is reachable",
)
def test_a_window_on_the_bundled_software_device_refuses_at_setup_and_the_stream_keeps_running(
    runtime_unit: RuntimeUnitUnderTest,
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    tatolabd = start_tatolabd_running_stream(
        a_test_pattern_into_a_display_window,
        extra_environment={
            "VK_DRIVER_FILES": str(the_bundled_software_driver_manifest(runtime_unit)),
            "VK_ICD_FILENAMES": None,
            "VK_ADD_DRIVER_FILES": None,
        },
    )
    stream_name = tatolabd.await_the_latest_attached_stream_loaded()
    local_api = tatolabd.local_api_client()
    node_states = local_api.await_every_node_past_setup(
        stream=stream_name, timeout=READINESS_TIMEOUT_SECONDS
    )
    time.sleep(STILL_RUNNING_AFTER_THE_REFUSAL_SECONDS)
    node_states_after_the_refusal = {
        node["name"]: node.get("components", {}).get("state")
        for node in local_api.graph(stream_name)["nodes"]
    }
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert "llvmpipe" in tatolabd.stderr_text, (
        f"the runtime must have built on the bundled software device:\n{tatolabd.recent_stderr()}"
    )
    assert node_states[WINDOW_NODE_NAME] == "Error", (
        f"the window must refuse at setup rather than reach Running: {node_states}"
    )
    assert node_states_after_the_refusal == {
        PATTERN_NODE_NAME: "Running",
        WINDOW_NODE_NAME: "Error",
    }, node_states_after_the_refusal
    assert f'DisplayWindow "{WINDOW_TITLE}"' in tatolabd.stderr_text, tatolabd.recent_stderr()
    for named in ("GPU vendor's Vulkan driver", "by its URL", "a node that reads its frames"):
        assert named in tatolabd.stderr_text, (
            f"the refusal must name {named!r}:\n{tatolabd.recent_stderr()}"
        )
    assert UNLOADED_SURFACE_COMMAND_PANIC not in tatolabd.stderr_text, tatolabd.recent_stderr()
