# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib.EncodedVideoFrame` — the encoded bag's cast, over a live link.

An encoded frame carries its payload inline, so what has to survive the wire
is the payload's msgpack *type*: `bin`, which reaches Python as `bytes`. That
is what these tests drive, over real wired iceoryx2 ports rather than a
stand-in, because a bag has to survive the wire before `into=` has anything to
cast.

Both ends live on this thread because iceoryx2's ports are `!Send`, and the
destination is wired first because a send with no subscriber attached is
dropped. No engine and no GPU: the ports are wired directly, which is what
keeps this half of the proof in CI.
"""

import os
from collections.abc import Iterator
from typing import Any

import pytest

from streamlib import ColorInfo, EncodedVideoFrame, ProcessorLinkDataAccess
from streamlib.encoded_video_frame import _CODECS_ON_THE_WIRE, _REQUIRED_BAG_KEYS

OUTPUT_PORT = "encoded_video_to_downstream"
INPUT_PORT = "encoded_video_from_upstream"

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


class WiredLinkUnderTest:
    """One live link, from the writing end to the reading end."""

    def __init__(
        self, source: ProcessorLinkDataAccess, destination: ProcessorLinkDataAccess
    ) -> None:
        self.source = source
        self.destination = destination

    def deliver(self, bag: "dict[str, Any]") -> None:
        self.source.write_to_output_port(OUTPUT_PORT, bag)


@pytest.fixture
def wired_link(request: pytest.FixtureRequest) -> Iterator[WiredLinkUnderTest]:
    """A source and a destination joined by one link.

    Service names carry the pid and the test's own name because iceoryx2
    service state is machine-global and outlives a crashed process — a fixed
    name would let one bad run poison every later one.
    """
    unique = f"encvid{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/encoded_video"
    notify_service_name = f"{unique}_dest/notify"
    link_id = f"L-{unique}"

    destination = ProcessorLinkDataAccess()
    destination.wire_input_link(
        INPUT_PORT,
        channel_service_name,
        notify_service_name,
        "read_next_in_order",
        8,
        2,
        1,
        link_id,
    )
    source = ProcessorLinkDataAccess()
    source.wire_output_link(
        OUTPUT_PORT,
        channel_service_name,
        notify_service_name,
        1024,
        1 << 20,
        8,
        2,
        1,
        link_id,
    )
    yield WiredLinkUnderTest(source, destination)


def test_the_bitstream_crosses_the_wire_as_bytes(wired_link: WiredLinkUnderTest):
    """The contract underneath the cast: `bitstream` is a byte buffer on the
    wire, so an untyped read hands back `bytes` rather than a list of numbers.

    A producer whose access unit encoded as a msgpack array would still read
    back equal-looking data here — as a `list`, unreadable as a buffer by a
    muxer, a socket, or a consumer in another language.
    """
    wired_link.deliver(encoded_frame_bag())

    bag = wired_link.destination.read_from_input_port(INPUT_PORT)

    assert bag is not None
    assert type(bag["bitstream"]) is bytes
    assert bag["bitstream"] == ANNEX_B_ACCESS_UNIT


def test_every_wire_key_survives_the_read_into_an_encoded_video_frame(
    wired_link: WiredLinkUnderTest,
):
    """All eight keys, off a live link, in one assertion each — this is the
    lock on the wire contract the codec built-ins publish against."""
    wired_link.deliver(
        encoded_frame_bag(is_sync_point=False, group_index=3, sequence_index=91)
    )

    frame = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedVideoFrame
    )

    assert isinstance(frame, EncodedVideoFrame)
    assert frame.codec == "h264"
    assert frame.annex_b_access_unit_bytes == ANNEX_B_ACCESS_UNIT
    assert frame.is_sync_point is False
    assert frame.group_index == 3
    assert frame.sequence_index == 91
    assert frame.width == 320
    assert frame.height == 192
    assert frame.color == ColorInfo(
        primaries="bt709", transfer="bt709", matrix="bt709", range="limited"
    )


@pytest.mark.parametrize("codec", _CODECS_ON_THE_WIRE)
def test_both_elementary_streams_read_through_the_same_cast(
    wired_link: WiredLinkUnderTest, codec: str
):
    """One cast serves both codecs: `codec` is metadata on an otherwise
    identical bag, which is why the pair differs in an enumerant and a name."""
    wired_link.deliver(encoded_frame_bag(codec=codec))

    frame = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedVideoFrame
    )

    assert isinstance(frame, EncodedVideoFrame)
    assert frame.codec == codec


def test_a_key_this_cast_does_not_read_does_not_break_the_read(
    wired_link: WiredLinkUnderTest,
):
    """The bag map is open: the day a producer adds a key must not be the day
    every `read(port, into=EncodedVideoFrame)` starts raising."""
    wired_link.deliver(encoded_frame_bag(a_future_key="ignored"))

    frame = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedVideoFrame
    )

    assert isinstance(frame, EncodedVideoFrame)
    assert frame.sequence_index == 0
    assert not hasattr(frame, "a_future_key")


def test_a_bag_with_no_color_reads_as_unspecified(wired_link: WiredLinkUnderTest):
    """`color` is absent-means-unspecified — the H.273 rule — so a producer
    that writes none is describing an unspecified stream, not an unreadable
    bag."""
    bag = encoded_frame_bag()
    del bag["color"]
    wired_link.deliver(bag)

    frame = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=EncodedVideoFrame
    )

    assert isinstance(frame, EncodedVideoFrame)
    assert frame.color is None


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
