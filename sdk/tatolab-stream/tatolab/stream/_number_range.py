# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`NumberRange`: the inclusive bounds of a config key's number, written on its annotation."""

from __future__ import annotations

import math
import typing
from typing import Any, Optional, Union

__all__ = ["NumberRange"]

_Bound = Union[int, float]


class NumberRange:
    """A config key's inclusive bounds: `Annotated[float, NumberRange(minimum=0.0, maximum=1.0)]`.

    The derived schema carries them as JSON Schema's `minimum` and `maximum`, which the
    runtime holds a live edit to. Either bound may be left out; at least one is stated.
    """

    __slots__ = ("_minimum", "_maximum")

    def __init__(
        self, *, minimum: Optional[_Bound] = None, maximum: Optional[_Bound] = None
    ) -> None:
        if minimum is None and maximum is None:
            raise TypeError(
                "NumberRange(...) states no bound; write `minimum=`, `maximum=`, or both."
            )
        self._minimum = _refuse_a_bound_that_is_not_a_finite_number(minimum, "minimum")
        self._maximum = _refuse_a_bound_that_is_not_a_finite_number(maximum, "maximum")
        if self._minimum is not None and self._maximum is not None and self._minimum > self._maximum:
            raise ValueError(
                f"NumberRange(minimum={self._minimum!r}, maximum={self._maximum!r}) admits "
                f"no number: its minimum is above its maximum."
            )

    @property
    def minimum(self) -> Optional[_Bound]:
        """The smallest number the key admits, or `None` when unbounded below."""
        return self._minimum

    @property
    def maximum(self) -> Optional[_Bound]:
        """The largest number the key admits, or `None` when unbounded above."""
        return self._maximum

    def json_schema_keywords(self) -> "dict[str, _Bound]":
        """The `minimum` and `maximum` keywords this range writes into a key's schema."""
        keywords: "dict[str, _Bound]" = {}
        if self._minimum is not None:
            keywords["minimum"] = self._minimum
        if self._maximum is not None:
            keywords["maximum"] = self._maximum
        return keywords

    def __eq__(self, other: object) -> bool:
        return (
            isinstance(other, NumberRange)
            and other._minimum == self._minimum
            and other._maximum == self._maximum
        )

    def __hash__(self) -> int:
        return hash((NumberRange, self._minimum, self._maximum))

    def __repr__(self) -> str:
        return f"NumberRange(minimum={self._minimum!r}, maximum={self._maximum!r})"


def _refuse_a_bound_that_is_not_a_finite_number(
    bound: object, keyword: str
) -> Optional[_Bound]:
    if bound is None:
        return None
    # `bool` is an `int` to Python and a distinct type to JSON Schema.
    if isinstance(bound, bool) or not isinstance(bound, (int, float)):
        raise TypeError(f"NumberRange(...) takes `{keyword}` as an int or a float, got {bound!r}.")
    if isinstance(bound, float) and not math.isfinite(bound):
        raise ValueError(
            f"NumberRange(...) takes a finite `{keyword}`, got {bound!r}; leave the bound "
            f"out to leave that side unbounded."
        )
    return bound


def number_range_an_annotation_carries(annotation: Any, ranged: str) -> Optional[NumberRange]:
    """The `NumberRange` an `Annotated[...]` carries, or `None`.

    `ranged` names what the annotation types, for the refusal of two ranges.
    """
    if typing.get_origin(annotation) is not typing.Annotated:
        return None
    ranges = [entry for entry in typing.get_args(annotation)[1:] if isinstance(entry, NumberRange)]
    if len(ranges) > 1:
        raise TypeError(f"{ranged} carries {len(ranges)} `NumberRange`s; it takes one.")
    return ranges[0] if ranges else None
