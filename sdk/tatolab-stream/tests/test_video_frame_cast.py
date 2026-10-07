# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.VideoFrame` — the video bag's cast, built from a bag in hand.

What the cast reads and what it refuses, decided before any wire is involved.
"""

import re

import pytest

from tatolab.stream import VideoFrame


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


@pytest.mark.parametrize("axis", ["primaries", "transfer", "matrix", "range"])
def test_video_frame_refuses_a_colour_name_it_cannot_place(axis: str):
    """An unrecognised name is a producer's mistake — the engine's own
    producers narrow an unknown H.273 byte to absent — so it is refused
    naming the axis and the value rather than reaching the typed field as a
    string nobody declared."""
    valid = {"surface_id": "1", "width": 1, "height": 1, "timestamp_ns": 0}
    with pytest.raises(
        ValueError,
        match=re.escape(
            f"color_info.{axis} is 'bt_709', which is not an H.273 {axis} name"
        ),
    ):
        VideoFrame.from_bag({**valid, "color_info": {axis: "bt_709"}})


def test_video_frame_refuses_a_colour_axis_that_is_not_a_string():
    valid = {"surface_id": "1", "width": 1, "height": 1, "timestamp_ns": 0}
    with pytest.raises(
        ValueError,
        match=re.escape("color_info.primaries must be a string or absent; got 6"),
    ):
        VideoFrame.from_bag({**valid, "color_info": {"primaries": 6}})


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
