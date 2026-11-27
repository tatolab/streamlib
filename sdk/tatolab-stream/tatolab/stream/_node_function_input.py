# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`Input[T]`, a value input of a node written as one function, declared with its default."""

from __future__ import annotations

from typing import Generic, TypeVar

__all__ = ["Input", "InputReadOffItsDeclaration"]

_InputValue = TypeVar("_InputValue")


class InputReadOffItsDeclaration(RuntimeError):
    """`.get()` called on the `Input(...)` that declares an input's default, not on the input the
    running node's body is handed."""


class Input(Generic[_InputValue]):
    """A value input of a node written as one function, declared with its default:
    `level: Input[float] = Input(0.25)`.

    `T` is the pin type, written as a class node's field writes it, `Input[Annotated[float,
    NumberRange(minimum=0.0)]]`. The body is handed the running input in its place, which
    `.get()` reads in a build or in the per-frame function.
    """

    __slots__ = ("default",)

    default: _InputValue
    """The value the input holds until one is set or wired."""

    def __init__(self, default: _InputValue) -> None:
        self.default = default

    def get(self) -> _InputValue:
        """The input's value, read in a build or in the per-frame function of the running node."""
        raise InputReadOffItsDeclaration(
            f"{self!r} declares an input's default, and holds no running value. To fix: call "
            f"`.get()` on the input the node's body is handed, in a `computed(...)` build or in "
            f"the function it returns."
        )

    def __repr__(self) -> str:
        return f"Input({self.default!r})"
