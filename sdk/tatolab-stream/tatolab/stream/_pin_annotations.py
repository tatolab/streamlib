# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What a pin states besides its type, each written as one object on its annotation.

`Annotated[float, NumberRange(minimum=20.0, maximum=20_000.0), NumberUnit("Hz"),
PinMeaning("frequency")]` declares a Float input with a range, a unit and a meaning. Each class
here is one key of the contract's `PinAnnotations`, so the author's word is the catalog's word.
`NumberRange` and `Description` live in their own modules, which config keys also read.
"""

from __future__ import annotations

import math
from typing import final

__all__ = ["NumberStep", "NumberUnit", "PinControl", "PinMeaning"]


@final
class NumberStep:
    """The step a control moves a number pin by: `NumberStep(0.01)`."""

    __slots__ = ("_step",)

    def __init__(self, step: float) -> None:
        # `bool` is an `int` to Python and no step to a control.
        if isinstance(step, bool) or not isinstance(step, (int, float)):
            raise TypeError(f"NumberStep(...) takes the step as an int or a float, got {step!r}.")
        if not math.isfinite(step) or step <= 0:
            raise ValueError(
                f"NumberStep(...) takes a step above 0 that is finite, got {step!r}."
            )
        self._step = float(step)

    @property
    def step(self) -> float:
        """The step as written, as a float."""
        return self._step

    def __eq__(self, other: object) -> bool:
        return isinstance(other, NumberStep) and other._step == self._step

    def __hash__(self) -> int:
        return hash((NumberStep, self._step))

    def __repr__(self) -> str:
        return f"NumberStep({self._step!r})"


class _WordAnnotation:
    """An annotation that states one word or short phrase, never blank."""

    __slots__ = ("_text",)

    def __init__(self, text: str) -> None:
        if not isinstance(text, str):
            raise TypeError(f"{type(self).__name__}(...) takes a str, got {text!r}.")
        if not text.strip():
            raise ValueError(f"{type(self).__name__}(...) takes a word, and was given a blank str.")
        self._text = text

    @property
    def text(self) -> str:
        """The text as written."""
        return self._text

    def __eq__(self, other: object) -> bool:
        return type(other) is type(self) and other._text == self._text  # pyright: ignore[reportAttributeAccessIssue,reportUnknownMemberType]

    def __hash__(self) -> int:
        return hash((type(self), self._text))

    def __repr__(self) -> str:
        return f"{type(self).__name__}({self._text!r})"


@final
class NumberUnit(_WordAnnotation):
    """The unit a number pin is in, as a person reads it: `NumberUnit("Hz")`."""

    __slots__ = ()


@final
class PinMeaning(_WordAnnotation):
    """What a pin's value stands for: `PinMeaning("frequency")`.

    A link between two pins that state different meanings connects, with a warning.
    """

    __slots__ = ()


@final
class PinControl(_WordAnnotation):
    """The control that edits or shows a pin: one of the Studio's, such as `PinControl("slider")`,
    or a module's control by its qualified name."""

    __slots__ = ()
