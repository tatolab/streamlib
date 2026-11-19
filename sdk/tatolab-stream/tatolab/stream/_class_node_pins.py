# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`InputPin[T]` and `OutputPin[T]`: how a class node under bare `@node` declares a stream, a trigger and an output.

A class node declares each of them as an annotation with no value, `video: InputPin[Texture]`,
and each value input as an annotated field with a default, `radius: float = 4.0`. The form
states the flow: an `InputPin` streams, or fires for a Trigger, and a field is a value. Each is
read by bare `@node` at decoration and is never constructed.
"""

from __future__ import annotations

from typing import Generic, NoReturn, TypeVar

__all__ = ["InputPin", "OutputPin"]

PinTypeOfThePin = TypeVar("PinTypeOfThePin")


class _ClassNodePin(Generic[PinTypeOfThePin]):
    """A pin a class node declares in an annotation; never constructed."""

    __slots__ = ()

    def __init__(self) -> NoReturn:
        raise TypeError(
            f"`{type(self).__name__}[...]` declares a class node's pin in an annotation with no "
            f"value, `video: {type(self).__name__}[Texture]`, and is never constructed."
        )


class InputPin(_ClassNodePin[PinTypeOfThePin]):
    """A class node's Stream or Trigger input: `video: InputPin[Texture]`.

    How it reads its messages is `StreamDelivery` inside it,
    `InputPin[Annotated[Audio, StreamDelivery("ordered")]]`; a pin that states none reads by
    its type's default.
    """

    __slots__ = ()


class OutputPin(_ClassNodePin[PinTypeOfThePin]):
    """A class node's output: `result: OutputPin[Texture]`."""

    __slots__ = ()
