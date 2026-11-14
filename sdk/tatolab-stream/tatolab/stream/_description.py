# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`Description`: what a config key or a function node's port is, written on its annotation."""

from __future__ import annotations

import typing
from typing import Any, Optional

__all__ = ["Description"]


class Description:
    """A config key's or a function node port's description: `Annotated[int, Description("…")]`.

    CommonMark with no raw HTML, served exactly as written. The text is a string
    value, a literal or a variable holding one; nothing is read from a file.
    """

    __slots__ = ("_text",)

    def __init__(self, text: str) -> None:
        self._text = refuse_a_description_that_is_not_a_string(text, "Description(...)")

    @property
    def text(self) -> str:
        """The description as written."""
        return self._text

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Description) and other._text == self._text

    def __hash__(self) -> int:
        return hash((Description, self._text))

    def __repr__(self) -> str:
        return f"Description({self._text!r})"


def refuse_a_description_that_is_not_a_string(description: object, declared_on: str) -> str:
    """`description` itself, refused unless it is a `str`."""
    if not isinstance(description, str):
        raise TypeError(
            f"{declared_on} takes its description as a str, got {description!r}. A "
            f"description is a string value written in the code, never a path to read it "
            f"from."
        )
    return description


def description_and_annotation_without_it(annotation: Any, described: str) -> "tuple[Optional[str], Any]":
    """The `Description` an `Annotated[...]` carries, and the annotation with it taken off.

    `described` names what the annotation types, for the refusal of two descriptions.
    """
    if typing.get_origin(annotation) is not typing.Annotated:
        return None, annotation
    annotated, *metadata = typing.get_args(annotation)
    descriptions = [entry for entry in metadata if isinstance(entry, Description)]
    if len(descriptions) > 1:
        raise TypeError(
            f"{described} carries {len(descriptions)} `Description`s; it takes one."
        )
    if not descriptions:
        return None, annotation
    other_metadata = [entry for entry in metadata if not isinstance(entry, Description)]
    if not other_metadata:
        return descriptions[0].text, annotated
    return descriptions[0].text, typing.Annotated[annotated, *other_metadata]  # pyright: ignore[reportInvalidTypeArguments]
