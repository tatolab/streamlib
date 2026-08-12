# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A second class, in a second module, for the two-processor identity arm.

Its own module rather than a second class beside `IdentityStableProcessor`,
because that is the half a shared module could not tell apart: two classes in
one module differ only in `__qualname__`, and a graph mixing modules is what a
real app looks like.
"""

from streamlib import processor


@processor(execution="continuous", interval_ms=1)
class SecondIdentityStableProcessor:
    """Does nothing per tick. Identity is the whole subject here."""

    def process(self, ctx) -> None: ...
