# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`tatolab.camera.CameraSource` — the camera module, run from the suite project.

The load test needs no device: `tatolabd` loads the graph and its start is
then refused at the GPU. The graph test starts the engine, which initializes a GPU context,
so it carries `requires_gpu` like every other graph test here. It needs no
camera: the device it names exists on no machine, and what it asserts is that
the source refuses rather than landing elsewhere — which holds on a platform no
capture backend serves, too.
"""

from collections.abc import Callable

import pytest

from camera_source_streams import (
    UNOPENABLE_DEVICE_ID,
    a_camera_naming_a_device_no_backend_can_open,
    a_camera_source_alone,
)
from conftest import StreamRunWithNoVulkanDriverOutcome, TatolabdUnderTest
from tatolab.stream import run_graph_generator

READINESS_TIMEOUT_SECONDS = 10.0
CAMERA_NODE_NAME = "camerasource"


# ---- module class semantics (no GPU) ---------------------------------------


def test_node_name_defaults_to_the_type_name(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]",
):
    graph = run_graph_generator(a_camera_source_alone)
    assert [node["name"] for node in graph["nodes"]] == [CAMERA_NODE_NAME]

    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)
    assert outcome.loaded and outcome.loaded_node_count == 1, outcome.tatolab_run_stderr_text


# ---- the module in a real graph (GPU) --------------------------------------


@pytest.mark.requires_gpu
def test_a_device_that_was_named_and_cannot_be_opened_fails_its_build(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """Landing on a different device than the one named would be worse than failing, so the
    source's open device, a computed value, fails its build: the node runs no frame, and the
    stream's condition names the device that was asked for."""
    tatolabd = start_tatolabd_running_stream(a_camera_naming_a_device_no_backend_can_open)
    stream_id = tatolabd.await_the_latest_stream_loaded()
    diagnostic = tatolabd.local_api_client().await_the_diagnostic_of_a_node(
        stream=stream_id,
        node_name=CAMERA_NODE_NAME,
        kind="computed_value_build_failed",
        timeout=READINESS_TIMEOUT_SECONDS,
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert UNOPENABLE_DEVICE_ID in diagnostic["words"], (
        f"the failed build must name the device that was asked for: {diagnostic}"
    )
    assert diagnostic["recovery"] == "change_an_input_the_build_read", diagnostic
