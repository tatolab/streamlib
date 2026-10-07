# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.EncodedVideoFrame` — the encoded bag's cast, built from a bag in hand.

What the cast refuses and how it reads its fields, decided before any wire is
involved. That the payload survives a real link as `bytes` is tested against
the runtime.
"""

import re
from typing import Any

import pytest

from tatolab.stream import EncodedVideoFrame
from tatolab.stream.encoded_video_frame import _REQUIRED_BAG_KEYS


# A one-NAL access unit: the four-byte Annex-B start code, an IDR NAL header,
# and a byte of payload. Short on purpose — nothing here decodes it, and what
# is under test is that the bytes arrive as bytes.
ANNEX_B_ACCESS_UNIT = b"\x00\x00\x00\x01\x65\x88"


BT709_COLOR_ON_THE_WIRE = {
    "primaries": "bt709",
    "transfer": "bt709",
    "matrix": "bt709",
    "range": "limited",
}


def encoded_frame_bag(**overrides: Any) -> "dict[str, Any]":
    """A sync-point H.264 access unit at the coded extent of a 320×180 source."""
    bag: "dict[str, Any]" = {
        "codec": "h264",
        "bitstream": ANNEX_B_ACCESS_UNIT,
        "is_sync_point": True,
        "group_index": 0,
        "sequence_index": 0,
        "width": 320,
        "height": 192,
        "color": dict(BT709_COLOR_ON_THE_WIRE),
    }
    bag.update(overrides)
    return bag


def test_a_bitstream_that_is_not_bytes_is_refused_by_name():
    """The mistake this catches is the one that otherwise decodes silently: a
    producer whose access unit went out as a list of numbers rather than a
    buffer."""
    with pytest.raises(ValueError, match="'bitstream' must be bytes"):
        EncodedVideoFrame.from_bag(encoded_frame_bag(bitstream=[0, 0, 0, 1]))


def test_a_codec_naming_neither_elementary_stream_is_refused_by_name():
    with pytest.raises(ValueError, match="codec 'av1' names no elementary stream"):
        EncodedVideoFrame.from_bag(encoded_frame_bag(codec="av1"))


def test_a_sync_point_flag_that_is_not_a_bool_is_refused_by_name():
    """`is_sync_point` decides whether a reader may enter the stream here, so
    a truthy `1` standing in for it is a producer bug and not a convenience."""
    with pytest.raises(ValueError, match="'is_sync_point' must be bool"):
        EncodedVideoFrame.from_bag(encoded_frame_bag(is_sync_point=1))


@pytest.mark.parametrize(
    "key", ["group_index", "sequence_index", "width", "height"]
)
def test_a_bool_is_refused_for_every_integer_field(key: str):
    """`bool` is an `int` subclass, so a `sequence_index` of `True` would
    otherwise arrive as 1 and read as a plausible ordering."""
    with pytest.raises(ValueError, match=f"{key!r} must be int"):
        EncodedVideoFrame.from_bag(encoded_frame_bag(**{key: True}))


@pytest.mark.parametrize("key", _REQUIRED_BAG_KEYS)
def test_a_missing_key_is_named(key: str):
    bag = encoded_frame_bag()
    del bag[key]

    with pytest.raises(ValueError, match=f"missing key {key!r}"):
        EncodedVideoFrame.from_bag(bag)


def test_a_malformed_color_names_this_bag_rather_than_a_video_frame():
    """The H.273 reader is shared with `VideoFrame`, and a refusal that named
    `color_info` on a video frame would send a codec author looking at the
    wrong key of the wrong convention."""
    with pytest.raises(
        ValueError,
        match="bag is not an encoded video frame: 'color' must be a mapping",
    ):
        EncodedVideoFrame.from_bag(encoded_frame_bag(color="bt709"))


def test_an_unplaceable_colour_name_is_refused_naming_this_bag_and_the_axis():
    """The same reader serves both casts, so the refusal must carry this
    bag's own key — `color`, never `color_info` — beside the axis and value."""
    with pytest.raises(
        ValueError,
        match=re.escape(
            "bag is not an encoded video frame: color.transfer is 'bt_709', "
            "which is not an H.273 transfer name"
        ),
    ):
        EncodedVideoFrame.from_bag(encoded_frame_bag(color={"transfer": "bt_709"}))


def test_an_encoded_video_frame_takes_no_surface_and_holds_no_claim():
    """An access unit touches no surface machinery at all — the cast composes
    nothing that would demand a surface id or take a claim, which is why a
    frame is constructible from a bag this test wrote by hand."""
    frame = EncodedVideoFrame.from_bag(encoded_frame_bag())

    assert not hasattr(frame, "surface_id")
    assert not hasattr(frame, "writable")
    assert not hasattr(frame, "__dlpack__")


def test_the_access_unit_stays_off_the_repr():
    """A failed assertion on an encoded frame prints its ordering, not tens of
    kilobytes of bitstream."""
    rendered = repr(EncodedVideoFrame.from_bag(encoded_frame_bag()))

    assert "sequence_index=0" in rendered
    assert "annex_b_access_unit_bytes" not in rendered


def test_the_cast_offers_no_way_back_onto_the_wire():
    """Producing an encoded bag is spelling the keys against the wire
    contract and writing it with `ctx.outputs.write(port, bag,
    timestamp_ns=...)`. A to-bag helper would be a second spelling of the
    contract, and `dataclasses.asdict` would emit `annex_b_access_unit_bytes`
    rather than the wire's `bitstream`."""
    frame = EncodedVideoFrame.from_bag(encoded_frame_bag())

    assert not hasattr(frame, "to_bag")
    assert not hasattr(frame, "as_bag")
