# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream.msgpack` — packing a record, unpacking a payload, and the two casts."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, TypedDict

import pytest

import tatolab.stream
from tatolab.stream import MsgpackCast, msgpack_typed_dict_cast
from tatolab.stream.msgpack import pack, unpack

MSGPACK_BIN8_MARKER = 0xC4
MSGPACK_FIXSTR_MARKER_BASE = 0xA0


def test_pack_then_unpack_returns_the_record():
    record = {
        "surface_id": "camera#7",
        "width": 1280,
        "timestamp_ns": -5,
        "ratio": 0.5,
        "enabled": True,
        "absent": None,
        "samples": b"\x00\x01\x02",
        "nested": {"levels": [1, 2, {"deeper": b"\xff"}]},
    }

    assert unpack(pack(record)) == record


def test_bytes_pack_as_msgpack_bin_and_str_as_msgpack_str():
    assert pack({"b": b"\x07"}) == bytes(
        [0x81, MSGPACK_FIXSTR_MARKER_BASE | 1, ord("b"), MSGPACK_BIN8_MARKER, 1, 0x07]
    )
    assert pack({"s": "x"}) == bytes(
        [0x81, MSGPACK_FIXSTR_MARKER_BASE | 1, ord("s"), MSGPACK_FIXSTR_MARKER_BASE | 1, ord("x")]
    )


def test_unpack_reads_msgpack_bin_as_bytes_and_msgpack_str_as_str():
    unpacked = unpack(pack({"b": b"\x07", "s": "x"}))

    assert isinstance(unpacked["b"], bytes)
    assert isinstance(unpacked["s"], str)


def test_a_tuple_packs_as_an_array_and_unpacks_as_a_list():
    assert unpack(pack({"extent": (1280, 720)})) == {"extent": [1280, 720]}


def test_pack_refuses_what_is_not_a_mapping_by_name():
    with pytest.raises(TypeError, match="pack\\(\\) encodes a mapping .* got list"):
        pack([1, 2])  # pyright: ignore[reportArgumentType]


def test_pack_takes_a_mapping_that_is_not_a_dict():
    from types import MappingProxyType

    assert unpack(pack(MappingProxyType({"width": 4}))) == {"width": 4}


def test_pack_refuses_a_key_that_is_not_a_string_naming_its_path():
    with pytest.raises(TypeError, match="string keys .* the int key 1 at the top level"):
        pack({1: "x"})  # pyright: ignore[reportArgumentType]


def test_pack_refuses_a_nested_key_that_is_not_a_string_naming_its_path():
    with pytest.raises(TypeError, match="string keys .* the int key 2 under 'outer' > 0"):
        pack({"outer": [{2: "y"}]})


def _nested_record(container_depth: int) -> dict[str, Any]:
    record: dict[str, Any] = {}
    for _ in range(container_depth - 1):
        record = {"inner": record}
    return record


def test_pack_takes_a_record_nesting_128_containers():
    assert unpack(pack(_nested_record(128))) == _nested_record(128)


def test_pack_refuses_a_record_nesting_more_than_128_containers():
    with pytest.raises(ValueError, match="nest more than 128 deep"):
        pack(_nested_record(129))


def test_unpack_reads_a_map_with_integer_keys_as_a_rust_producer_may_write():
    integer_keyed_map = bytes([0x81, 0x01, MSGPACK_FIXSTR_MARKER_BASE | 1, ord("x")])

    assert unpack(integer_keyed_map) == {1: "x"}


def test_the_module_is_reachable_as_tatolab_stream_msgpack():
    assert tatolab.stream.msgpack.pack is pack
    assert tatolab.stream.msgpack.unpack is unpack


@dataclass(frozen=True)
class Detection(MsgpackCast):
    label: str
    confidence: float
    box: "list[int]"
    crop: bytes = b""


@dataclass(frozen=True)
class Corner:
    x: int
    y: int


@dataclass(frozen=True)
class Region(MsgpackCast):
    name: str
    top_left: Corner

    def __post_init__(self) -> None:
        if isinstance(self.top_left, dict):
            object.__setattr__(self, "top_left", Corner(**self.top_left))


def test_from_bytes_constructs_the_dataclass_from_the_payload():
    payload = pack({"label": "cat", "confidence": 0.9, "box": [1, 2, 3, 4], "crop": b"\x01"})

    detection = Detection.from_bytes(payload)

    assert detection == Detection(label="cat", confidence=0.9, box=[1, 2, 3, 4], crop=b"\x01")


def test_from_bytes_raises_for_a_missing_key_naming_it():
    payload = pack({"label": "cat", "box": [1, 2, 3, 4]})

    with pytest.raises(TypeError, match="confidence"):
        Detection.from_bytes(payload)


def test_from_bytes_refuses_a_payload_that_is_not_a_map():
    with pytest.raises(TypeError, match="a Detection is read from a msgpack map; the payload holds a list"):
        Detection.from_bytes(bytes([0x92, 0x01, 0x02]))


def test_to_bytes_round_trips_through_from_bytes():
    detection = Detection(label="dog", confidence=0.25, box=[0, 0, 8, 8], crop=b"\x00\xff")

    assert Detection.from_bytes(detection.to_bytes()) == detection


def test_to_bytes_writes_the_fields_as_a_map_of_their_names():
    detection = Detection(label="dog", confidence=0.25, box=[0, 0, 8, 8])

    assert unpack(detection.to_bytes()) == {
        "label": "dog",
        "confidence": 0.25,
        "box": [0, 0, 8, 8],
        "crop": b"",
    }


def test_to_bytes_writes_a_nested_dataclass_as_a_nested_map():
    region = Region(name="door", top_left=Corner(x=3, y=4))

    assert unpack(region.to_bytes()) == {"name": "door", "top_left": {"x": 3, "y": 4}}
    assert Region.from_bytes(region.to_bytes()) == region


def test_to_bytes_refuses_a_class_that_is_not_a_dataclass_by_name():
    class HandWrittenRecord(MsgpackCast):
        def __init__(self, label: str) -> None:
            self.label = label

    with pytest.raises(TypeError, match="HandWrittenRecord is not a dataclass"):
        HandWrittenRecord("cat").to_bytes()
    assert HandWrittenRecord.from_bytes(pack({"label": "cat"})).label == "cat"


class DetectionRecord(TypedDict):
    label: str
    confidence: float


def test_msgpack_typed_dict_cast_hands_back_the_unpacked_map():
    payload = pack({"label": "cat", "confidence": 0.9})

    record = msgpack_typed_dict_cast(DetectionRecord).from_bytes(payload)

    assert record == {"label": "cat", "confidence": 0.9}
    assert type(record) is dict


def test_msgpack_typed_dict_cast_checks_nothing():
    """A TypedDict is a type checker's promise only; the cast validates no key."""
    payload = pack({"label": "cat", "a_key_the_typed_dict_does_not_name": 1})

    record: Any = msgpack_typed_dict_cast(DetectionRecord).from_bytes(payload)

    assert record == {"label": "cat", "a_key_the_typed_dict_does_not_name": 1}


def test_a_typed_dict_cannot_take_the_mixin():
    with pytest.raises(TypeError):

        class DetectionRecordWithTheMixin(TypedDict, MsgpackCast):  # type: ignore[misc]
            label: str
