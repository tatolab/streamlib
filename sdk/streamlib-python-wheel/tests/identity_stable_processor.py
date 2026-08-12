# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The one class every launch arrangement adds.

It lives in its own module beside the entry file rather than inside it, which
is the only legal home for a processor: a helper process reaches its class by
importing this name, and a class in the entry file identifies as `__main__`,
which names the child's own entry file instead.
"""

from streamlib import processor


@processor(execution="continuous", interval_ms=1)
class IdentityStableProcessor:
    """Does nothing per tick. Identity is the whole subject here."""

    def process(self, ctx) -> None: ...
