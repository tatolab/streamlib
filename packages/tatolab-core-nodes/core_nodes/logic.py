# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Nodes that choose, count, gate and hold."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated

from tatolab.stream import Image, State, Stateful, UnreadMessage, node


@dataclass(frozen=True)
class Comparison:
    """How `a` compares with `b`, within a tolerance."""

    less: bool
    equal: bool
    greater: bool


@node.meta(category="Logic", description="Whether a is less than, equal to or greater than b.")
@node.function
def Compare(a: float = 0.0, b: float = 0.0, tolerance: float = 0.0) -> Comparison:  # noqa: N802
    """Whether `a` is less than, equal to (within `tolerance`) or greater than `b`."""
    equal = abs(a - b) <= tolerance
    return Comparison(less=not equal and a < b, equal=equal, greater=not equal and a > b)


@node.meta(
    category="Logic",
    description="Let a signal of any kind through while open, and block it while closed.",
)
@node.function
def Gate(value: UnreadMessage, open: bool = True) -> UnreadMessage | None:  # noqa: N802, A002
    """`value` handed on unread while `open`, else nothing, of any family."""
    return value if open else None


@node.meta(category="Logic", description="Pick one of two numbers with an on/off input.")
@node.function
def Switch(on: bool = False, when_on: float = 1.0, when_off: float = 0.0) -> float:  # noqa: N802
    """`when_on` while `on`, else `when_off`."""
    return when_on if on else when_off


@node.meta(category="Logic", description="Pick one of four numbers by index.")
@node.function
def Select(  # noqa: N802
    index: int = 0,
    option_0: float = 0.0,
    option_1: float = 0.0,
    option_2: float = 0.0,
    option_3: float = 0.0,
) -> float:
    """The option `index` names, `index` held between 0 and 3."""
    held_index = min(max(index, 0), 3)
    if held_index == 0:
        return option_0
    if held_index == 1:
        return option_1
    if held_index == 2:
        return option_2
    return option_3


@dataclass(frozen=True)
class TriggerLevel:
    """Whether a `Trigger`'s input was on at its last evaluation."""

    was_on: bool = False


@node.meta(category="Logic", description="A one-frame pulse each time its input turns on.")
@node.function
def Trigger(  # noqa: N802
    x: bool = False, *, previous: State[TriggerLevel]
) -> Stateful[bool, TriggerLevel]:
    """On for the one evaluation at which `x` turns on, and off otherwise."""
    return x and not previous.was_on, TriggerLevel(x)


@dataclass(frozen=True)
class CounterCount:
    """A `Counter`'s count, and whether its `increment` was on at its last evaluation."""

    count: int = 0
    increment_was_on: bool = False


@node.meta(category="Logic", description="Count how many times its input has turned on.")
@node.function
def Counter(  # noqa: N802
    increment: bool = False, reset: bool = False, *, previous: State[CounterCount]
) -> Stateful[int, CounterCount]:
    """How many times `increment` has turned on, back to 0 while `reset` is on."""
    if reset:
        return 0, CounterCount(0, increment)
    count = previous.count + 1 if increment and not previous.increment_was_on else previous.count
    return count, CounterCount(count, increment)


@dataclass(frozen=True)
class SampledValue:
    """The value a `SampleAndHold` holds, and whether its `sample` was on at its last evaluation."""

    held: float = 0.0
    sample_was_on: bool = False


@node.meta(
    category="Logic",
    description="Grab a number when sample turns on and hold it until the next time.",
)
@node.function
def SampleAndHold(  # noqa: N802
    x: float = 0.0, sample: bool = False, *, previous: State[SampledValue]
) -> Stateful[float, SampledValue]:
    """`x` as it was when `sample` last turned on, starting at 0."""
    held = x if sample and not previous.sample_was_on else previous.held
    return held, SampledValue(held, sample)


@dataclass(frozen=True)
class HeldFrame:
    """The frame a `HoldFrame` holds, none before its first."""

    frame: UnreadMessage | None = None


@node.meta(
    category="Logic",
    description="Freeze a picture: hold the first frame until refresh takes a newer one.",
)
@node.function
def HoldFrame(  # noqa: N802
    frame: Annotated[UnreadMessage, Image] | None = None,
    refresh: bool = False,
    *,
    previous: State[HeldFrame],
) -> Stateful[UnreadMessage | None, HeldFrame]:
    """The first Image `frame` it is handed, held and handed on each read until `refresh` takes a newer one."""
    if frame is not None and (previous.frame is None or refresh):
        return frame, HeldFrame(frame)
    return previous.frame, previous
