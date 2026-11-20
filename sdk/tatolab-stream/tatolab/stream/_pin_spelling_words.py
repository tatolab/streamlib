# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""How a pin's refusal names what it was typed and the spellings it could be typed instead."""

from __future__ import annotations

import types
import typing
from typing import Final

from ._pin_types import (
    INTEGER_SIZES_ONLY_AN_ELEMENT_HOLDS,
    PIN_TYPE_SPELLINGS,
    ExactNumberSize,
)

__all__ = [
    "WRITTEN_FORMS_A_LIST_ELEMENT_TAKES",
    "is_a_union",
    "name_of",
    "spellings_read_today",
    "written_forms_a_list_element_takes",
    "written_name_of",
]

_WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES: Final[frozenset[str]] = frozenset(
    ExactNumberSize(size).written_form for size in INTEGER_SIZES_ONLY_AN_ELEMENT_HOLDS
)

WRITTEN_FORMS_A_LIST_ELEMENT_TAKES: Final[frozenset[str]] = frozenset(
    spelling.written_form
    for spelling in PIN_TYPE_SPELLINGS
    if spelling.pin_type_name
    in (
        "boolean",
        "integer",
        "float",
        "string",
        "menu",
        "color",
        "vector",
        "rectangle",
        "transform",
        "asset",
    )
)
"""The written forms of the small types, which a List element and a List's bundle member take."""


def written_forms_a_list_element_takes() -> str:
    """The small types a List element takes, each written as a declaration writes it."""
    return ", ".join(
        f"`{spelling.written_form}`"
        for spelling in PIN_TYPE_SPELLINGS
        if spelling.written_form in WRITTEN_FORMS_A_LIST_ELEMENT_TAKES
    )


def spellings_read_today() -> str:
    """Each spelling the typed grammar reads as a value, written as a declaration writes it."""
    read_today = [
        f"`{spelling.written_form}`"
        for spelling in PIN_TYPE_SPELLINGS
        if spelling.not_read_yet_because is None
        and not spelling.only_a_pin_carries
        and spelling.written_form not in _WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES
    ]
    return ", ".join(read_today)


def written_name_of(annotation: object) -> str:
    """`annotation` as a declaration writes it, without backticks."""
    if isinstance(annotation, (type, typing.TypeVar)):
        return annotation.__name__
    return repr(annotation)


def is_a_union(annotation: object) -> bool:
    """Whether `annotation` is a union, `A | B` or `Union[A, B]`."""
    return typing.get_origin(annotation) in (typing.Union, types.UnionType)


def name_of(annotation: object) -> str:
    """`annotation` as a refusal names it, in backticks."""
    if isinstance(annotation, type):
        return f"`{annotation.__name__}`"
    return f"`{annotation!r}`"
