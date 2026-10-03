# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The cast every exposed name — machine, stream, node, port — goes through.

Pure Python, so it runs where no engine is loaded. The engine's
`cast_exposed_name_to_url_safe` casts the same way, and both are held to
`runtime/streamlib-engine/tests/fixtures/exposed_name_cast_cases.json`.
"""

from __future__ import annotations

import unicodedata

__all__ = [
    "EXPOSED_NAME_MAXIMUM_LENGTH",
    "ExposedNameCastsToNothingError",
    "cast_exposed_name_to_url_safe",
]

EXPOSED_NAME_MAXIMUM_LENGTH = 63
"""Longest name the cast keeps, in characters — a DNS label's bound."""

_RFC_3986_UNRESERVED_CHARACTERS = frozenset(
    "abcdefghijklmnopqrstuvwxyz0123456789-._~"
)
_REPLACEMENT_CHARACTER = "-"


class ExposedNameCastsToNothingError(ValueError):
    """A name that casts to empty, `.` or `..`, so it cannot name anything."""


def cast_exposed_name_to_url_safe(name: str) -> str:
    """Cast `name` to lowercase RFC 3986 unreserved characters (`a-z 0-9 - . _ ~`).

    Accents are dropped, every other character becomes `-`, runs of `-`
    collapse, `-` is trimmed from both ends and the result is cut to
    `EXPOSED_NAME_MAXIMUM_LENGTH`. A name casting to empty, `.` or `..` raises
    `ExposedNameCastsToNothingError`.
    """
    without_accents = "".join(
        character
        for character in unicodedata.normalize("NFKD", name)
        if not unicodedata.category(character).startswith("M")
    )
    cast_characters: "list[str]" = []
    for character in without_accents.lower():
        kept = (
            character
            if character in _RFC_3986_UNRESERVED_CHARACTERS
            else _REPLACEMENT_CHARACTER
        )
        if (
            kept == _REPLACEMENT_CHARACTER
            and cast_characters
            and cast_characters[-1] == _REPLACEMENT_CHARACTER
        ):
            continue
        cast_characters.append(kept)

    cast = (
        "".join(cast_characters)
        .strip(_REPLACEMENT_CHARACTER)[:EXPOSED_NAME_MAXIMUM_LENGTH]
        .rstrip(_REPLACEMENT_CHARACTER)
    )
    if cast in ("", ".", ".."):
        raise ExposedNameCastsToNothingError(
            f"the name {name!r} casts to {cast!r}, which cannot name anything — a name "
            f"has to keep at least one of a-z 0-9 - . _ ~ once lowercased with its "
            f"accents dropped, and cannot be '.' or '..'"
        )
    return cast
