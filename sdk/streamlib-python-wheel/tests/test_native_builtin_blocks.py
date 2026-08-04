# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native built-in blocks: marker classes resolved by `rt.add`, frames
produced by native code the interpreter never enters.

The graph tests boot a real engine (GPU required); the marker and
`VideoFrame` cast tests are pure Python.
"""

import queue
import threading
import time

import pytest

import streamlib
from streamlib import RuntimeContextLimitedAccess, TestPatternSource, VideoFrame, input, processor

PIPELINE_TIMEOUT_SECONDS = 30.0
ENGINE_TEARDOWN_TIMEOUT_SECONDS = 60.0


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

_received_bags: "queue.Queue[dict]" = queue.Queue()


@processor
class VideoFrameProbe:
    """Collects every video-frame bag the native source publishes."""

    @input(delivery_profile="every_sample")
    def video_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("video_from_upstream")
        if bag is not None:
            _received_bags.put(bag)


@pytest.mark.requires_gpu
def test_the_test_pattern_source_produces_frames_a_python_processor_reads():
    """The whole built-in mechanism, end to end: marker class → native
    registration → native production → bag read by an in-process Python
    processor — no camera, no window."""
    while not _received_bags.empty():
        _received_bags.get_nowait()

    runtime = streamlib.Runtime()
    pattern = runtime.add(TestPatternSource, config={"width": 320, "height": 180})
    probe = runtime.add(VideoFrameProbe)
    runtime.connect(pattern.output("video"), probe.input("video_from_upstream"))

    run_thread = threading.Thread(target=runtime.run, name="engine-run")
    run_thread.start()
    try:
        first_bag = _received_bags.get(timeout=PIPELINE_TIMEOUT_SECONDS)
        second_bag = _received_bags.get(timeout=PIPELINE_TIMEOUT_SECONDS)
    finally:
        runtime.shutdown()
        run_thread.join(timeout=ENGINE_TEARDOWN_TIMEOUT_SECONDS)
        assert not run_thread.is_alive(), "engine did not tear down in time"

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
