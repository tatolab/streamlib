# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`streamlib.encode_bag_to_msgpack_bytes` / `decode_msgpack_bytes_to_python_object`.

The engine's bag codec, reachable by a caller that carries the bytes itself —
an extension wheel publishing a bag over its own transport. No link is read or
written here: these are two pure functions, and what they must prove is that
the wire type survives the round trip and that the codec's refusals are not
softened on the way out.

The Rust half of this lives beside the codec in `python_bag_conversion.rs`;
what these add is the Python surface — the names reachable off `streamlib`, and
`bytes` arriving back as `bytes` rather than a list of integers.
"""

from typing import Any

import pytest

import streamlib


def test_a_nested_bag_carrying_binary_round_trips_unchanged() -> None:
    bag: dict[str, Any] = {
        "label": "telemetry",
        "nested": {"payload": b"\x00\xc8\x07", "items": [1, 2.5, None]},
    }

    decoded = streamlib.decode_msgpack_bytes_to_python_object(
        streamlib.encode_bag_to_msgpack_bytes(bag)
    )

    assert decoded == bag
    assert type(decoded["nested"]["payload"]) is bytes


def test_binary_rides_as_msgpack_bin_at_one_times_its_length() -> None:
    # Every byte above 127 costs a marker of its own in a msgpack array, so the
    # same payload encoded as one would be over twice this long.
    payload = b"\xff" * 1024
    framing_bytes_around_a_lone_payload = 16

    encoded = streamlib.encode_bag_to_msgpack_bytes({"payload": payload})

    assert len(encoded) <= len(payload) + framing_bytes_around_a_lone_payload
    assert payload in encoded


def test_a_top_level_that_is_not_a_named_map_is_refused() -> None:
    with pytest.raises(TypeError, match="a bag is a dict with string keys"):
        streamlib.encode_bag_to_msgpack_bytes([1, 2, 3])  # type: ignore[arg-type]


def test_a_non_string_key_is_refused() -> None:
    with pytest.raises(TypeError, match="bag keys must be strings"):
        streamlib.encode_bag_to_msgpack_bytes({1: "value"})  # type: ignore[dict-item]
