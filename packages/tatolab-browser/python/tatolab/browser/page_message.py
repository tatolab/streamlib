# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`PageMessage`: one JSON object a page takes on `data` or publishes on `events`, the browser module's own pin type."""

from __future__ import annotations

import json
from collections.abc import Buffer, Mapping
from dataclasses import dataclass
from typing import Any, Self

from tatolab.stream import named_pin_type


@named_pin_type(
    description=(
        "One JSON object: the detail of a page's `tatolab:data` event, or what a page published "
        "with `window.tatolab.publish(object)`."
    )
)
@dataclass(frozen=True)
class PageMessage:
    """One JSON object, travelling between nodes as its UTF-8 JSON text."""

    members: Mapping[str, Any]
    """The object's members, each a value JSON holds: text, a number, a boolean, None, a list or
    another mapping. NaN and the infinities are not among them."""

    @classmethod
    def from_bytes(cls, payload: Buffer) -> Self:
        members = json.loads(bytes(payload))
        if not isinstance(members, dict):
            raise ValueError(f"a PageMessage is one JSON object, not {type(members).__name__}")
        return cls(members)

    def to_bytes(self) -> bytes:
        return json.dumps(
            self.members, separators=(",", ":"), allow_nan=False, default=_a_mapping_as_a_dict
        ).encode()


def _a_mapping_as_a_dict(value: object) -> object:
    """Any other mapping as the dict JSON writes as an object; anything else is refused."""
    if isinstance(value, Mapping):
        return dict(value)
    raise TypeError(f"Object of type {type(value).__name__} is not JSON serializable")
