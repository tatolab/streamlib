# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The animation clock every node here runs on, read off the monotonic `ctx.time`."""

from __future__ import annotations

__all__ = [
    "NANOSECONDS_PER_MILLISECOND",
    "NANOSECONDS_PER_SECOND",
    "ElapsedSecondsSinceFirstProcess",
]

NANOSECONDS_PER_SECOND = 1_000_000_000
NANOSECONDS_PER_MILLISECOND = 1_000_000


class ElapsedSecondsSinceFirstProcess:
    """Seconds since the first `process()` that asked, so an animation starts with the picture.

    Anchored on the first call rather than on setup: a child's interpreter starts
    well before traffic reaches it.
    """

    def __init__(self) -> None:
        self._first_process_at_ns: int | None = None

    def seconds_at(self, now_ns: int) -> float:
        """Seconds from the first `now_ns` this clock was asked about to this one."""
        if self._first_process_at_ns is None:
            self._first_process_at_ns = now_ns
        return (now_ns - self._first_process_at_ns) / NANOSECONDS_PER_SECOND
