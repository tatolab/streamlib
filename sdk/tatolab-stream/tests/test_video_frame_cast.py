# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.VideoFrame` — the video-frame message's cast, read from a payload in hand.

What the cast reads and what it refuses, decided before any wire is involved.
"""

import re
from typing import Any

import pytest

from tatolab.stream import ColorInfo, VideoFrame
from tatolab.stream.msgpack import pack, unpack

# What `rmp_serde::to_vec_named` emits for the camera's frame — 1280×720,
# SMPTE 170M primaries and matrix, BT.709 transfer, limited range, 30 fps —
# with `content_light`, `mastering_display` and `texture_layout` absent.
CAMERA_VIDEO_FRAME_PAYLOAD = bytes.fromhex(
    "86aa737572666163655f6964ad706f6f6c65642d736c6f742337a57769647468cd0500"
    "a6686569676874cd02d0ac74696d657374616d705f6e73ce0012d687aa636f6c6f725f"
    "696e666f84a66d6174726978a9736d7074653137306da97072696d6172696573a9736d"
    "7074653137306da572616e6765a76c696d69746564a87472616e73666572a562743730"
    "39a36670731e"
)

MINIMAL_FRAME_ENTRIES: "dict[str, Any]" = {
    "surface_id": "1",
    "width": 1,
    "height": 1,
    "timestamp_ns": 0,
}


def video_frame_payload(**entries: Any) -> bytes:
    return pack({**MINIMAL_FRAME_ENTRIES, **entries})


def test_from_bytes_reads_the_cameras_record():
    frame = VideoFrame.from_bytes(CAMERA_VIDEO_FRAME_PAYLOAD)

    assert frame.surface_id == "pooled-slot#7"
    assert (frame.width, frame.height) == (1280, 720)
    assert frame.timestamp_ns == 1_234_567
    assert frame.fps == 30
    assert frame.color_info == ColorInfo(
        primaries="smpte170m", transfer="bt709", matrix="smpte170m", range="limited"
    )
    assert frame.content_light is None
    assert frame.mastering_display is None
    assert frame.texture_layout is None


def test_to_bytes_round_trips_the_cameras_record():
    frame = VideoFrame.from_bytes(CAMERA_VIDEO_FRAME_PAYLOAD)

    assert VideoFrame.from_bytes(frame.to_bytes()) == frame


def test_to_bytes_writes_the_keys_the_camera_writes():
    frame = VideoFrame.from_bytes(CAMERA_VIDEO_FRAME_PAYLOAD)

    written = unpack(frame.to_bytes())

    assert {key: value for key, value in written.items() if value is not None} == unpack(
        CAMERA_VIDEO_FRAME_PAYLOAD
    )


def test_from_bytes_names_a_missing_key():
    with pytest.raises(TypeError, match="surface_id"):
        VideoFrame.from_bytes(pack({"width": 1, "height": 1, "timestamp_ns": 0}))


def test_from_bytes_rejects_mistyped_fields():
    with pytest.raises(ValueError, match="must be int"):
        VideoFrame.from_bytes(video_frame_payload(timestamp_ns="not-an-int"))


def test_from_bytes_rejects_mistyped_optional_fields():
    with pytest.raises(ValueError, match="fps"):
        VideoFrame.from_bytes(video_frame_payload(fps="30"))
    with pytest.raises(ValueError, match="texture_layout"):
        VideoFrame.from_bytes(video_frame_payload(texture_layout="GENERAL"))
    with pytest.raises(ValueError, match="color_info"):
        VideoFrame.from_bytes(video_frame_payload(color_info="srgb"))


@pytest.mark.parametrize("axis", ["primaries", "transfer", "matrix", "range"])
def test_from_bytes_refuses_a_colour_name_it_cannot_place(axis: str):
    with pytest.raises(
        ValueError,
        match=re.escape(
            f"color_info.{axis} is 'bt_709', which is not an H.273 {axis} name"
        ),
    ):
        VideoFrame.from_bytes(video_frame_payload(color_info={axis: "bt_709"}))


def test_from_bytes_refuses_a_colour_axis_that_is_not_a_string():
    with pytest.raises(
        ValueError,
        match=re.escape("color_info.primaries must be a string or absent; got 6"),
    ):
        VideoFrame.from_bytes(video_frame_payload(color_info={"primaries": 6}))


def test_from_bytes_rejects_bool_dimensions():
    with pytest.raises(ValueError, match="must be int"):
        VideoFrame.from_bytes(video_frame_payload(width=True))


def test_from_bytes_wraps_malformed_nested_metadata_in_the_same_error():
    with pytest.raises(ValueError, match="content_light"):
        VideoFrame.from_bytes(
            video_frame_payload(content_light={"max_cll": 1000, "unexpected_key": 1})
        )


def test_from_bytes_ignores_keys_the_cast_does_not_read():
    frame = VideoFrame.from_bytes(video_frame_payload(a_key_a_newer_producer_adds="ignored"))

    assert frame == VideoFrame(**MINIMAL_FRAME_ENTRIES)
