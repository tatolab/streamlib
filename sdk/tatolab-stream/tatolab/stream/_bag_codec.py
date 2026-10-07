# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The engine's one bag codec, for a caller that carries the wire bytes itself."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from ._runtime_lend import runtime_backed_function


@runtime_backed_function()
def encode_bag_to_msgpack_bytes(bag: Mapping[str, Any]) -> bytes:
    """Encode a bag to the msgpack bytes the wire carries, for a caller — an
    extension wheel with its own transport — that carries them itself.

    The engine's one bag codec, reachable: a dict with string keys at every
    level, values from `dict`, `list`, `tuple`, `str`, `bytes`, `int`, `float`,
    `bool` and `None`, `bytes` as msgpack `bin` at 1×. Anything else raises
    `TypeError`; an integer wider than 64 bits, and containers nested more than
    128 deep — the bag itself the outermost, as one holding itself is — raise
    `ValueError`.
    """
    ...


@runtime_backed_function()
def decode_msgpack_bytes_to_python_object(msgpack_bytes: bytes) -> Any:
    """Decode msgpack bytes into ordinary Python data.

    These are payload bytes with no transport frame header in front of them. A
    value whose containers nest more than 128 deep raises `ValueError` — the
    bound `encode_bag_to_msgpack_bytes` keeps, so whatever decodes encodes
    again, and bytes from an untrusted peer cannot recurse without limit.
    """
    ...
