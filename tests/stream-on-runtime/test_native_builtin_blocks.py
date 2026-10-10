# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native built-in blocks: built-in classes resolved by `stream_builder.add`, frames
produced by native code the interpreter never enters.

The graph test starts the engine on `tatolabd`. The built-in classes and the
`VideoFrame` cast are pure Python, tested in the stream suite.
"""

from collections.abc import Callable

import pytest

import tatolab.stream
from conftest import StreamRunWithNoVulkanDriverOutcome
from native_builtin_probes import VideoFrameProbe
from runtime_process_under_test import RuntimeProcessUnderTest
from tatolab.stream import (
    StreamBuilder,
    TestPatternSource,
    VideoFrame,
    compile_stream_to_graph,
    stream,
)


@stream
def a_test_pattern_into_a_video_frame_probe(stream_builder: StreamBuilder) -> None:
    pattern = stream_builder.add(
        tatolab.stream.TestPatternSource, config={"width": 320, "height": 180}
    )
    probe = stream_builder.add(VideoFrameProbe)
    stream_builder.connect(pattern.output("video"), probe.input("video_from_upstream"))


@stream
def a_test_pattern_source_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource)


# ---- the native block in a real graph (GPU) --------------------------------


@pytest.mark.requires_gpu
def test_the_test_pattern_source_produces_frames_a_python_processor_reads(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
):
    """The whole built-in mechanism, end to end: built-in class → native
    registration → native production in `tatolabd` → bag read by a Python
    processor in its own processor interpreter — no camera, no window."""
    tatolabd = start_tatolabd(a_test_pattern_into_a_video_frame_probe)
    frames_seen = tatolabd.await_marker("FRAMES_SEEN")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert isinstance(frames_seen, list), f"no parseable frame report:\n{tatolabd.recent_stderr()}"
    first_bag, second_bag = frames_seen

    frame = VideoFrame.from_bag(first_bag)
    assert (frame.width, frame.height) == (320, 180)
    assert frame.surface_id, "surface_id names the pattern surface"
    assert frame.fps == 30
    assert frame.color_info is not None and frame.color_info.transfer == "srgb"

    later_frame = VideoFrame.from_bag(second_bag)
    assert later_frame.surface_id == frame.surface_id, (
        "the pattern surface is acquired once and republished"
    )
    assert later_frame.timestamp_ns > frame.timestamp_ns, (
        "timestamps are the ordering primitive and must advance"
    )


def test_node_name_defaults_to_the_type_name(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]",
):
    graph = compile_stream_to_graph(a_test_pattern_source_alone)
    assert [node["name"] for node in graph["nodes"]] == ["testpatternsource"]

    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)
    assert outcome.loaded and outcome.loaded_node_count == 1, outcome.tatolab_run_stderr_text
