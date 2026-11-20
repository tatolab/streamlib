# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`ViewModelValues`: values for a Rive artboard's view-model properties, the Rive module's own pin type."""

from __future__ import annotations

import json
from collections.abc import Buffer, Mapping
from dataclasses import dataclass
from typing import Self

from tatolab.stream import named_pin_type


@named_pin_type(
    description="Values for a Rive artboard's view-model properties, each named by its property path."
)
@dataclass(frozen=True)
class ViewModelValues:
    """A map from view-model property path to the value assigned to it, travelling as UTF-8 JSON.

    A number goes to a number property, text to a string or enum property, a boolean to a
    boolean property, `0xAARRGGBB` or `"#RRGGBB"` to a colour property, and anything but `False`
    fires a trigger.
    """

    values: Mapping[str, "str | int | float | bool"]

    @classmethod
    def from_bytes(cls, payload: Buffer) -> Self:
        values = json.loads(bytes(payload))
        if not isinstance(values, dict):
            raise ValueError(f"ViewModelValues are one JSON object, not {type(values).__name__}")
        return cls(values)

    def to_bytes(self) -> bytes:
        return json.dumps(
            self.values, separators=(",", ":"), allow_nan=False, default=_a_mapping_as_a_dict
        ).encode()


def _a_mapping_as_a_dict(value: object) -> object:
    """Any other mapping as the dict JSON writes as an object; anything else is refused."""
    if isinstance(value, Mapping):
        return dict(value)
    raise TypeError(f"Object of type {type(value).__name__} is not JSON serializable")
