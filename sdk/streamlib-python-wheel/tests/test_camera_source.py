# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.CameraSource` — the camera built-in over the video device seam.

The marker tests are pure Python. The graph test boots a real engine, which
initializes a GPU context, so it carries `requires_gpu` like every other graph
test here. It needs no camera: the device it names exists on no machine, and
what it asserts is that the source refuses rather than landing elsewhere —
which holds on a platform no capture backend serves, too.
"""

from pathlib import Path

import pytest

import tatolab.runtime
import tatolab.stream
from camera_source_named_device_app import UNOPENABLE_DEVICE_ID
from tatolab.stream import Stream, compile_stream_to_graph, stream

NAMED_DEVICE_APP = Path(__file__).parent / "camera_source_named_device_app.py"


# ---- marker semantics (no GPU) ---------------------------------------------


def test_the_marker_class_cannot_be_instantiated():
    with pytest.raises(TypeError):
        tatolab.stream.CameraSource()


@stream
def a_camera_source_alone(stream: Stream) -> None:
    stream.add(tatolab.stream.CameraSource)


def test_node_name_defaults_to_the_type_name():
    graph = compile_stream_to_graph(a_camera_source_alone)
    assert [node["name"] for node in graph["nodes"]] == ["camerasource"]
    runtime = tatolab.runtime.Runtime()
    try:
        runtime.load(graph)
    finally:
        runtime.shutdown()


# ---- the native block in a real graph (GPU) --------------------------------


@pytest.mark.requires_gpu
def test_a_device_that_was_named_and_cannot_be_opened_refuses_at_setup(
    start_app_under_test,
):
    """Landing on a different camera would be worse than failing, so the source
    refuses and the processor never reaches Running."""
    app = start_app_under_test(NAMED_DEVICE_APP)
    app.await_output_containing(
        "MARKER:NOT_EVERY_PROCESSOR_RUNNING", "the readiness wait to report Error"
    )
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    assert "MARKER:EVERY_PROCESSOR_RUNNING" not in app.output
    assert UNOPENABLE_DEVICE_ID in app.output, (
        f"the refusal must name the device that was asked for:\n{app.output}"
    )
