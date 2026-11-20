# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Which of the pin type spellings the typed grammar reads, as its refusals name them: the
spellings a pin of one type is written in today, and those a List element takes."""

from __future__ import annotations

from typing import Final

from ._pin_types import INTEGER_SIZES_ONLY_AN_ELEMENT_HOLDS, PIN_TYPE_SPELLINGS, ExactNumberSize

__all__ = [
    "WRITTEN_FORMS_A_LIST_ELEMENT_TAKES",
    "spellings_read_today",
    "written_forms_a_list_element_takes",
]

# The written forms of the sizes only an element of a List or a Tensor holds.
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
"""The written forms of the small types, which a List element takes."""


def written_forms_a_list_element_takes() -> str:
    """Each written form a List element takes, as a refusal lists them."""
    return ", ".join(
        f"`{spelling.written_form}`"
        for spelling in PIN_TYPE_SPELLINGS
        if spelling.written_form in WRITTEN_FORMS_A_LIST_ELEMENT_TAKES
    )


def spellings_read_today() -> str:
    """Each spelling a pin of one type that holds a value is written in today, as a refusal lists
    them."""
    read_today = [
        f"`{spelling.written_form}`"
        for spelling in PIN_TYPE_SPELLINGS
        if spelling.not_read_yet_because is None
        and not spelling.only_a_pin_carries
        and spelling.written_form not in _WRITTEN_FORMS_ONLY_AN_ELEMENT_TAKES
    ]
    return ", ".join(read_today)
