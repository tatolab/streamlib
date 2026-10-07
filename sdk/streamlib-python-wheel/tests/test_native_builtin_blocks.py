# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native built-in blocks: built-in classes resolved by `stream_builder.add`, frames
produced by native code the interpreter never enters.

The graph tests boot a real engine. The built-in classes and the `VideoFrame`
cast are pure Python, tested in the stream suite.
"""

import json
import re
import sys
from pathlib import Path

import pytest

import tatolab.runtime
from tatolab.stream import (
    StreamBuilder,
    TestPatternSource,
    VideoFrame,
    compile_stream_to_graph,
    stream,
)

PIPELINE_TIMEOUT_SECONDS = 30.0
ENGINE_TEARDOWN_TIMEOUT_SECONDS = 60.0

NATIVE_BUILTIN_APP = Path(__file__).parent / "native_builtin_app.py"

FRAMES_SEEN = re.compile(r"MARKER:FRAMES_SEEN (\[.*\])")


# ---- the native block in a real graph (GPU) --------------------------------


@pytest.mark.requires_gpu
def test_the_test_pattern_source_produces_frames_a_python_processor_reads(
    start_app_under_test,
):
    """The whole built-in mechanism, end to end: built-in class → native
    registration → native production in the app process → bag read by a
    Python processor in its own helper process — no camera, no window."""
    app = start_app_under_test(NATIVE_BUILTIN_APP)
    app.await_output_containing("MARKER:FRAMES_SEEN", "the probe's first two frames")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    match = FRAMES_SEEN.search(app.output)
    assert match is not None, f"no parseable frame report:\n{app.output}"
    first_bag, second_bag = json.loads(match.group(1))

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


@stream
def a_test_pattern_source_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(TestPatternSource)


def test_node_name_defaults_to_the_type_name():
    graph = compile_stream_to_graph(a_test_pattern_source_alone)
    assert [node["name"] for node in graph["nodes"]] == ["testpatternsource"]
    runtime = tatolab.runtime.Runtime()
    try:
        runtime.load(
            graph,
            project_directory=Path(__file__).resolve().parent,
            interpreter=sys.executable,
        )
    finally:
        runtime.shutdown()
