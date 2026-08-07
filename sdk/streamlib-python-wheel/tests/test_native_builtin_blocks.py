# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native built-in blocks: marker classes resolved by `rt.add`, frames
produced by native code the interpreter never enters.

The graph tests boot a real engine (GPU required); the marker and
`VideoFrame` cast tests are pure Python.
"""

import json
import re
from pathlib import Path

import pytest

import streamlib
from streamlib import TestPatternSource, VideoFrame

PIPELINE_TIMEOUT_SECONDS = 30.0
ENGINE_TEARDOWN_TIMEOUT_SECONDS = 60.0

NATIVE_BUILTIN_APP = Path(__file__).parent / "native_builtin_app.py"

FRAMES_SEEN = re.compile(r"MARKER:FRAMES_SEEN (\[.*\])")


# ---- marker semantics (no GPU) ---------------------------------------------


def test_the_marker_class_cannot_be_instantiated():
    with pytest.raises(TypeError):
        TestPatternSource()


def test_an_undecorated_class_is_still_rejected_by_add():
    class NotAProcessor:
        pass

    runtime = streamlib.Runtime()
    try:
        with pytest.raises(RuntimeError, match="not a processor"):
            runtime.add(NotAProcessor)
    finally:
        runtime.shutdown()


# ---- VideoFrame cast (no GPU) ----------------------------------------------


def test_video_frame_casts_a_bag_with_full_metadata():
    bag = {
        "surface_id": "42",
        "width": 1280,
        "height": 720,
        "timestamp_ns": 123_456_789,
        "fps": 30,
        "color_info": {"primaries": "bt709", "transfer": "srgb", "range": "full"},
    }
    frame = VideoFrame.from_bag(bag)
    assert frame.surface_id == "42"
    assert (frame.width, frame.height) == (1280, 720)
    assert frame.timestamp_ns == 123_456_789
    assert frame.fps == 30
    assert frame.color_info is not None
    assert frame.color_info.primaries == "bt709"
    assert frame.color_info.transfer == "srgb"
    assert frame.color_info.matrix is None
    assert frame.content_light is None


def test_video_frame_names_the_missing_key():
    with pytest.raises(ValueError, match="surface_id"):
        VideoFrame.from_bag({"width": 1, "height": 1, "timestamp_ns": 0})


def test_video_frame_rejects_mistyped_fields():
    with pytest.raises(ValueError, match="must be int"):
        VideoFrame.from_bag(
            {"surface_id": "1", "width": 1, "height": 1, "timestamp_ns": "not-an-int"}
        )


def test_video_frame_rejects_mistyped_optional_fields():
    valid = {"surface_id": "1", "width": 1, "height": 1, "timestamp_ns": 0}
    with pytest.raises(ValueError, match="fps"):
        VideoFrame.from_bag({**valid, "fps": "30"})
    with pytest.raises(ValueError, match="texture_layout"):
        VideoFrame.from_bag({**valid, "texture_layout": "GENERAL"})
    with pytest.raises(ValueError, match="color_info"):
        VideoFrame.from_bag({**valid, "color_info": "srgb"})


def test_video_frame_rejects_bool_dimensions():
    # bool is an int subclass; a width of True is a bug, not a width.
    with pytest.raises(ValueError, match="must be int"):
        VideoFrame.from_bag(
            {"surface_id": "1", "width": True, "height": 1, "timestamp_ns": 0}
        )


def test_video_frame_wraps_malformed_nested_metadata_in_the_same_error():
    with pytest.raises(ValueError, match="content_light"):
        VideoFrame.from_bag(
            {
                "surface_id": "1",
                "width": 1,
                "height": 1,
                "timestamp_ns": 0,
                "content_light": {"max_cll": 1000, "unexpected_key": 1},
            }
        )


# ---- the native block in a real graph (GPU) --------------------------------


@pytest.mark.requires_gpu
def test_the_test_pattern_source_produces_frames_a_python_processor_reads(
    start_app_under_test,
):
    """The whole built-in mechanism, end to end: marker class → native
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


def test_display_name_defaults_to_the_type_name():
    runtime = streamlib.Runtime()
    try:
        pattern = runtime.add(TestPatternSource)
        assert pattern.display_name == "TestPatternSource"
    finally:
        runtime.shutdown()
