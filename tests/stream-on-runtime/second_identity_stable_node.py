# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A second class, in a second module, for the two-node identity arm.

Its own module rather than a second class beside `IdentityStableNode`,
because that is the half a shared module could not tell apart: two classes in
one module differ only in `__qualname__`, and a graph mixing modules is what a
real app looks like.
"""

from tatolab.stream import node


@node(execution="continuous", interval_ms=1)
class SecondIdentityStableNode:
    """Does nothing per tick. Identity is the whole subject here."""

    def process(self, ctx) -> None: ...
