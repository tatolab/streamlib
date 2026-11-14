# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The test pattern module on `tatolabd`: frames its processor renders on the device,
read by another Python processor in an interpreter of its own.

The module's declaration, config and placement math are tested with no GPU in
`packages/tatolab-testpattern/tests`.
"""

from collections.abc import Callable

import pytest

from conftest import StreamRunWithNoVulkanDriverOutcome, TatolabdUnderTest
from testpattern_module_streams import (
    a_test_pattern_into_an_image_frame_probe,
    a_test_pattern_source_alone,
)
from tatolab.stream import run_graph_generator


@pytest.mark.requires_gpu
def test_the_test_pattern_source_produces_frames_a_python_processor_reads(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]",
):
    """The module end to end: the installed module's class → its processor
    rendering frames in an interpreter of its own → Image frame read by another
    Python processor in its own interpreter — no camera, no window."""
    tatolabd = start_tatolabd_running_stream(a_test_pattern_into_an_image_frame_probe)
    frames_seen = tatolabd.await_marker("FRAMES_SEEN")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert isinstance(frames_seen, list), f"no parseable frame report:\n{tatolabd.recent_stderr()}"
    first_message, second_message = frames_seen

    assert (first_message["width"], first_message["height"]) == (320, 180)
    assert first_message["surface_id"], "surface_id names the frame's pooled texture"
    assert first_message["format"] == "rgba8_unorm"
    assert first_message["colour"]["transfer"] == "srgb"

    assert second_message["surface_id"] != first_message["surface_id"], (
        "each frame renders into the next texture of the node's output pool, published under "
        "an id of its own"
    )
    assert second_message["timestamp_ns"] > first_message["timestamp_ns"], (
        "timestamps are the ordering primitive and must advance"
    )


def test_node_name_defaults_to_the_type_name(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]",
):
    graph = run_graph_generator(a_test_pattern_source_alone)
    assert [node["name"] for node in graph["nodes"]] == ["testpatternsource"]

    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)
    assert outcome.loaded and outcome.loaded_node_count == 1, outcome.tatolab_run_stderr_text
