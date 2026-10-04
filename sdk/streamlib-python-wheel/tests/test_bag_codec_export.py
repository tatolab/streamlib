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

import subprocess
import sys
import textwrap
from typing import Any

import pytest

import streamlib

MAXIMUM_NESTED_CONTAINER_DEPTH = 128
NESTED_PAST_THE_MAXIMUM = f"containers nest more than {MAXIMUM_NESTED_CONTAINER_DEPTH} deep"


def lists_nested_containers_deep(containers: int) -> list[Any]:
    """`containers` lists, each holding the next, the innermost empty."""
    nested: list[Any] = []
    for _ in range(containers - 1):
        nested = [nested]
    return nested


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


def test_a_payload_nested_past_the_decoder_bound_is_refused() -> None:
    # One-element arrays all the way down, the shape a hostile peer sends to
    # recurse the decoder off its stack. The subscriber that decodes
    # relay-delivered bytes has nothing else standing between it and whatever
    # the far end sent.
    nested_far_past_the_bound = b"\x91" * 5000 + b"\xc0"

    with pytest.raises(ValueError, match=NESTED_PAST_THE_MAXIMUM):
        streamlib.decode_msgpack_bytes_to_python_object(nested_far_past_the_bound)


def test_the_deepest_bag_that_decodes_encodes_again_and_one_deeper_is_refused() -> None:
    # A passthrough processor publishes what it read, so decode keeps encode's
    # bound: the deepest bag that decodes is one that encodes again.
    reaching_the_maximum = streamlib.encode_bag_to_msgpack_bytes(
        {"nested": lists_nested_containers_deep(MAXIMUM_NESTED_CONTAINER_DEPTH - 1)}
    )
    one_element_array = b"\x91"
    one_past_the_maximum = reaching_the_maximum.replace(
        b"nested", b"nested" + one_element_array, 1
    )

    decoded = streamlib.decode_msgpack_bytes_to_python_object(reaching_the_maximum)
    assert streamlib.encode_bag_to_msgpack_bytes(decoded) == reaching_the_maximum
    with pytest.raises(ValueError, match=NESTED_PAST_THE_MAXIMUM) as refused:
        streamlib.decode_msgpack_bytes_to_python_object(one_past_the_maximum)
    assert "Have its producer nest the data at most 128 containers deep" in str(
        refused.value
    )


def test_bytes_that_do_not_hold_a_whole_msgpack_value_are_refused() -> None:
    with pytest.raises(ValueError, match="marker byte"):
        streamlib.decode_msgpack_bytes_to_python_object(b"")

    # An array header promising one element, with nothing behind it — refused
    # rather than handed back as the empty list that did arrive.
    with pytest.raises(ValueError, match="marker byte"):
        streamlib.decode_msgpack_bytes_to_python_object(b"\x91")


def test_a_top_level_that_is_not_a_named_map_is_refused() -> None:
    with pytest.raises(TypeError, match="a bag is a dict with string keys"):
        streamlib.encode_bag_to_msgpack_bytes([1, 2, 3])  # type: ignore[arg-type]


def test_a_non_string_key_is_refused() -> None:
    with pytest.raises(TypeError, match="bag keys must be strings"):
        streamlib.encode_bag_to_msgpack_bytes({1: "value"})  # type: ignore[dict-item]


def test_a_bag_nested_to_the_maximum_encodes_and_one_container_more_is_refused() -> None:
    # The bag itself is the outermost container.
    reaching_the_maximum = {
        "nested": lists_nested_containers_deep(MAXIMUM_NESTED_CONTAINER_DEPTH - 1)
    }
    one_past_the_maximum = {
        "nested": lists_nested_containers_deep(MAXIMUM_NESTED_CONTAINER_DEPTH)
    }

    streamlib.encode_bag_to_msgpack_bytes(reaching_the_maximum)
    with pytest.raises(ValueError, match=NESTED_PAST_THE_MAXIMUM):
        streamlib.encode_bag_to_msgpack_bytes(one_past_the_maximum)


def test_a_bag_holding_itself_is_refused_rather_than_crashing_the_process() -> None:
    # In its own process: without the bound the encode recurses off its stack,
    # which would end this suite rather than fail this test.
    completed = subprocess.run(
        [
            sys.executable,
            "-c",
            textwrap.dedent(
                f"""
                import streamlib

                bag_holding_itself = {{"label": "loop"}}
                bag_holding_itself["itself"] = bag_holding_itself
                try:
                    streamlib.encode_bag_to_msgpack_bytes(bag_holding_itself)
                except ValueError as refusal:
                    assert {NESTED_PAST_THE_MAXIMUM!r} in str(refusal), refusal
                    assert "holds itself" in str(refusal), refusal
                else:
                    raise AssertionError("a bag holding itself encoded")
                """
            ),
        ],
        capture_output=True,
        text=True,
        timeout=120.0,
        check=False,
    )

    assert completed.returncode == 0, (
        f"exit status {completed.returncode}\n{completed.stderr[-4000:]}"
    )
