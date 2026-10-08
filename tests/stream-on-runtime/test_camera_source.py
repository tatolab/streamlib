# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.CameraSource` — the camera built-in over the video device seam.

The load test needs no device: `tatolabd` loads the graph and is then refused
at the GPU. The graph test starts the engine, which initializes a GPU context,
so it carries `requires_gpu` like every other graph test here. It needs no
camera: the device it names exists on no machine, and what it asserts is that
the source refuses rather than landing elsewhere — which holds on a platform no
capture backend serves, too.
"""

from collections.abc import Callable

import pytest

import tatolab.stream
from conftest import StreamGraphLoadOutcome
from runtime_process_under_test import RuntimeProcessUnderTest
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream

UNOPENABLE_DEVICE_ID = "/dev/video-not-a-real-camera"
READINESS_TIMEOUT_SECONDS = 10.0
CAMERA_NODE_NAME = "camerasource"


@stream
def a_camera_source_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.CameraSource)


@stream
def a_camera_naming_a_device_no_backend_can_open(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.CameraSource, config={"device_id": UNOPENABLE_DEVICE_ID})


# ---- built-in class semantics (no GPU) -------------------------------------


def test_node_name_defaults_to_the_type_name(
    load_stream_graph_on_tatolabd: "Callable[..., StreamGraphLoadOutcome]",
):
    graph = compile_stream_to_graph(a_camera_source_alone)
    assert [node["name"] for node in graph["nodes"]] == [CAMERA_NODE_NAME]

    outcome = load_stream_graph_on_tatolabd(graph)
    assert outcome.loaded and outcome.loaded_node_count == 1, outcome.stderr_text


# ---- the native block in a real graph (GPU) --------------------------------


@pytest.mark.requires_gpu
def test_a_device_that_was_named_and_cannot_be_opened_refuses_at_setup(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
):
    """Landing on a different camera would be worse than failing, so the source
    refuses and the processor never reaches Running."""
    tatolabd = start_tatolabd(a_camera_naming_a_device_no_backend_can_open)
    node_states = tatolabd.local_api_client().await_every_node_past_setup(
        timeout=READINESS_TIMEOUT_SECONDS
    )
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert node_states[CAMERA_NODE_NAME] == "Error", (
        f"the camera must refuse at setup rather than reach Running: {node_states}"
    )
    assert UNOPENABLE_DEVICE_ID in tatolabd.stderr_text, (
        f"the refusal must name the device that was asked for:\n{tatolabd.recent_stderr()}"
    )
