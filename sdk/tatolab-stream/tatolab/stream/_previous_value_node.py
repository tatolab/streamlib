# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`PreviousValue`: the node a loop of function nodes closes through."""

from __future__ import annotations

from tatolab.stream import node

__all__ = ["PreviousValue"]


@node.meta(
    category="Logic",
    description="A number as it was at the last evaluation. A loop of number nodes closes through it.",
)
@node.function
def PreviousValue(value: float = 0.0) -> float:
    """`value` as it was at the last evaluation, 0 at the first. The runtime reads this
    node's input one evaluation late, so a loop of function nodes passes through one."""
    return value


# A graph names the node `tatolab.stream:PreviousValue`, the type the runtime knows it by.
PreviousValue.__module__ = "tatolab.stream"
