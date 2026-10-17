# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.EncodedVideoFrame` — the encoded-frame message's cast, read from a payload in hand.

What the cast refuses and how it reads its fields, decided before any wire is
involved.
"""

import re
from typing import Any

import pytest

from tatolab.stream import EncodedVideoFrame
from tatolab.stream.msgpack import pack, unpack

# A one-NAL access unit: the Annex-B start code, an IDR NAL header and a byte.
ANNEX_B_ACCESS_UNIT = b"\x00\x00\x00\x01\x65\x88"


BT709_COLOR_ON_THE_WIRE = {
    "primaries": "bt709",
    "transfer": "bt709",
    "matrix": "bt709",
    "range": "limited",
}

REQUIRED_MESSAGE_KEYS = (
    "codec",
    "bitstream",
    "is_sync_point",
    "group_index",
    "sequence_index",
    "width",
    "height",
)


def encoded_frame_entries(**overrides: Any) -> "dict[str, Any]":
    """A sync-point H.264 access unit at the coded extent of a 320×180 source."""
    entries: "dict[str, Any]" = {
        "codec": "h264",
        "bitstream": ANNEX_B_ACCESS_UNIT,
        "is_sync_point": True,
        "group_index": 0,
        "sequence_index": 0,
        "width": 320,
        "height": 192,
        "color": dict(BT709_COLOR_ON_THE_WIRE),
    }
    entries.update(overrides)
    return entries


def encoded_frame_payload(**overrides: Any) -> bytes:
    return pack(encoded_frame_entries(**overrides))


def test_from_bytes_reads_the_access_unit_as_bytes():
    frame = EncodedVideoFrame.from_bytes(encoded_frame_payload())

    assert frame.annex_b_access_unit_bytes == ANNEX_B_ACCESS_UNIT
    assert frame.codec == "h264"
    assert frame.color is not None
    assert frame.color.transfer == "bt709"


def test_to_bytes_writes_the_bitstream_under_the_wire_key_and_round_trips():
    frame = EncodedVideoFrame.from_bytes(encoded_frame_payload())

    assert unpack(frame.to_bytes()) == encoded_frame_entries()
    assert EncodedVideoFrame.from_bytes(frame.to_bytes()) == frame


def test_a_bitstream_that_is_not_bytes_is_refused_by_name():
    with pytest.raises(ValueError, match="'bitstream' must be bytes"):
        EncodedVideoFrame.from_bytes(encoded_frame_payload(bitstream=[0, 0, 0, 1]))


def test_a_codec_naming_neither_elementary_stream_is_refused_by_name():
    with pytest.raises(ValueError, match="codec 'av1' names no elementary stream"):
        EncodedVideoFrame.from_bytes(encoded_frame_payload(codec="av1"))


def test_a_sync_point_flag_that_is_not_a_bool_is_refused_by_name():
    with pytest.raises(ValueError, match="'is_sync_point' must be bool"):
        EncodedVideoFrame.from_bytes(encoded_frame_payload(is_sync_point=1))


@pytest.mark.parametrize("key", ["group_index", "sequence_index", "width", "height"])
def test_a_bool_is_refused_for_every_integer_field(key: str):
    with pytest.raises(ValueError, match=f"{key!r} must be int"):
        EncodedVideoFrame.from_bytes(encoded_frame_payload(**{key: True}))


@pytest.mark.parametrize("key", REQUIRED_MESSAGE_KEYS)
def test_a_missing_key_is_named(key: str):
    entries = encoded_frame_entries()
    del entries[key]

    with pytest.raises(TypeError, match=key):
        EncodedVideoFrame.from_bytes(pack(entries))


def test_a_malformed_color_names_this_message_rather_than_a_video_frame():
    with pytest.raises(
        ValueError,
        match="message is not an encoded video frame: 'color' must be a mapping",
    ):
        EncodedVideoFrame.from_bytes(encoded_frame_payload(color="bt709"))


def test_an_unplaceable_colour_name_is_refused_naming_this_message_and_the_axis():
    with pytest.raises(
        ValueError,
        match=re.escape(
            "message is not an encoded video frame: color.transfer is 'bt_709', "
            "which is not an H.273 transfer name"
        ),
    ):
        EncodedVideoFrame.from_bytes(encoded_frame_payload(color={"transfer": "bt_709"}))


def test_an_encoded_video_frame_takes_no_surface_and_holds_no_claim():
    frame = EncodedVideoFrame.from_bytes(encoded_frame_payload())

    assert not hasattr(frame, "surface_id")
    assert not hasattr(frame, "writable")
    assert not hasattr(frame, "__dlpack__")


def test_the_access_unit_stays_off_the_repr():
    rendered = repr(EncodedVideoFrame.from_bytes(encoded_frame_payload()))

    assert "sequence_index=0" in rendered
    assert "annex_b_access_unit_bytes" not in rendered
