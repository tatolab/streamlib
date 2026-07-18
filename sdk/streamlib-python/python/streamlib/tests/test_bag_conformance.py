# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Cross-language dynamic-bag wire conformance (issue #1407).

The Python SDK has no dedicated ``Bag`` type — a processor's ``inputs.read``
already returns a native ``dict`` and ``outputs.write`` accepts one, since a
StreamLib payload is a msgpack named map. This test proves that native dict
decodes the *same* committed fixture bytes the Rust ``Bag`` and the Deno
object do, across every value class, and that a Python-encoded equivalent is
itself a decodable named map (the write direction).

The fixture is authored by Rust (source of truth) at
``sdk/streamlib-plugin-sdk/tests/fixtures/bag_conformance.msgpack`` and read
identically by all three runtimes — a wire disagreement fails a test rather
than silently corrupting a payload.
"""

from __future__ import annotations

from pathlib import Path

import msgpack


# tests -> streamlib -> python -> streamlib-python -> sdk -> repo root
_REPO_ROOT = Path(__file__).resolve().parents[5]
_FIXTURE = (
    _REPO_ROOT
    / "sdk"
    / "streamlib-plugin-sdk"
    / "tests"
    / "fixtures"
    / "bag_conformance.msgpack"
)

# The canonical value-class-complete map every runtime's conformance test
# mirrors. ``blob`` is a msgpack ``bin`` (Python ``bytes``); everything else
# is a plain JSON-shaped value.
_CANONICAL = {
    "nil": None,
    "flag": True,
    "count": -7,
    "big": 4_000_000_000,
    "ratio": 1.5,
    "name": "streamlib",
    "list": [1, 2, 3],
    "nested": {"inner": "value"},
    "blob": b"\xde\xad\xbe\xef",
}


def _decode(raw: bytes) -> dict:
    # raw=False decodes msgpack `str` as ``str`` and `bin` as ``bytes`` — the
    # same split the Rust ``Bag`` draws between a string field and a ``bin``.
    return msgpack.unpackb(raw, raw=False)


def test_fixture_decodes_to_canonical_values() -> None:
    decoded = _decode(_FIXTURE.read_bytes())
    assert decoded == _CANONICAL
    # `blob` must arrive as `bin` (bytes), never a list of ints.
    assert isinstance(decoded["blob"], bytes)


def test_python_encoding_is_a_decodable_named_map() -> None:
    # The write direction: a Python dict packed with `use_bin_type=True`
    # round-trips to the same logical map — i.e. it is a named map the Rust
    # `Bag` and the Deno object can read.
    packed = msgpack.packb(_CANONICAL, use_bin_type=True)
    assert _decode(packed) == _CANONICAL


def test_tolerant_missing_and_unexpected_fields() -> None:
    decoded = _decode(_FIXTURE.read_bytes())
    # A field a consumer doesn't know about is simply present and ignorable.
    assert "nested" in decoded
    # A field the producer never sent is a plain miss, not an exception.
    assert decoded.get("frame_rate") is None
