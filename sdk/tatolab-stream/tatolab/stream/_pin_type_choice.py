# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`PinTypeChoice`: the value of a setting that chooses another pin's type."""

from __future__ import annotations

from typing import Annotated, Any, Final

from ._contract_pin_types import PinType
from ._pin_declaration import read_pin_declaration

__all__ = ["PinTypeChoice"]

_WHAT_A_CHOICE_IS_READ_AS: Final = "PinTypeChoice"


class PinTypeChoice:
    """A chosen pin type: `PinTypeChoice(T, *annotations)` takes one type a pin is written
    with, such as `float` or `EncodedVideo`, then the annotations that narrow it, the ones
    `Annotated[T, ...]` would carry.

    Its `chosen` is the type as the runtime stores it, such as `{"type": "float"}`, and a
    builder records that as the setting's value.
    """

    __slots__ = ("chosen",)

    chosen: PinType
    """The chosen type, as the runtime stores it."""

    def __init__(self, written_type: Any, *annotations: object) -> None:
        annotation: object = (
            Annotated[(written_type, *annotations)]  # pyright: ignore[reportInvalidTypeForm]
            if annotations
            else written_type
        )
        declared = read_pin_declaration(
            _WHAT_A_CHOICE_IS_READ_AS, "chosen", annotation, direction="input", form="pin"
        ).declaration["pin_type"]
        if declared["kind"] != "is":
            raise TypeError(
                f"`PinTypeChoice` chooses one type, and {annotation!r} declares a type found "
                f"another way. To fix: name one type, such as `PinTypeChoice(float)`"
            )
        self.chosen = declared["pin_type"]

    def __eq__(self, other: object) -> bool:
        return isinstance(other, PinTypeChoice) and other.chosen == self.chosen

    def __hash__(self) -> int:
        return hash(repr(self.chosen))

    def __repr__(self) -> str:
        return f"PinTypeChoice(chosen={self.chosen!r})"
