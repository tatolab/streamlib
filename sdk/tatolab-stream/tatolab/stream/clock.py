# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Canonical monotonic-clock timestamp source and drift-free periodic timer.

Use [`monotonic_now_ns`] for any timestamp that needs to be compared across
processes on one machine — frame stamps, log correlation tokens, anything that
crosses a process boundary. It reads the engine's media clock: on Linux
`clock_gettime(CLOCK_MONOTONIC)`, what `time.clock_gettime_ns(time.CLOCK_MONOTONIC)`
reads; on macOS `mach_absolute_time`, what
`time.clock_gettime_ns(time.CLOCK_UPTIME_RAW)` reads, which stops while the
machine sleeps. Every process on one machine, and every stamp the engine
takes, reads the one clock that machine's boot started.

That epoch is the machine's own boot, so a reading from another machine is a
reading of an unrelated clock and subtracting the two means nothing.

Wall-clock APIs (`time.time`, `datetime.now`, `time.time_ns`) are NOT
comparable across processes — they drift under NTP and reflect different
epochs. Use them only when human-readable wall-clock time is genuinely
required (e.g. ISO8601 log formatting).

[`start_monotonic_timer`] starts a drift-free periodic timer on that same
clock — a `timerfd` on Linux, a kqueue timer on macOS. The first deadline is
`now + interval` and every one after it is absolute, so ticks never accumulate
drift. Use the [`MonotonicTimer`] it returns as a context manager;
`wait(timeout_ms=...)` bounds teardown latency:

    with start_monotonic_timer(interval_ns) as timer:
        while timer.wait(timeout_ms=100) >= 0:
            ...

Both functions reach the runtime, which lends `tatolab.runtime` to the
interpreter it starts for a node; where nothing is lent they raise
`RuntimeError` naming themselves.
"""

from __future__ import annotations

from types import TracebackType
from typing import Literal, Protocol

from ._runtime_lend import runtime_backed_function, runtime_backed_protocol

__all__ = [
    "MonotonicTimer",
    "monotonic_now_ns",
    "start_monotonic_timer",
]


@runtime_backed_protocol
class MonotonicTimer(Protocol):
    """Drift-free periodic timer on the clock `monotonic_now_ns` reads.

    A `timerfd` on Linux, a kqueue timer on macOS. The first deadline is
    `now + interval` and every one after it is absolute, so ticks never
    accumulate drift.
    """

    @property
    def interval_ns(self) -> int: ...
    def wait(self, timeout_ms: int = 100) -> int:
        """Wait up to `timeout_ms` for the next tick.

        Returns a positive expiration count when a tick fired, 0 on timeout,
        -1 once closed.
        """
        ...

    def close(self) -> None:
        """Release the timer's file descriptor. Idempotent."""
        ...

    def __enter__(self) -> MonotonicTimer: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = None,
        exception: BaseException | None = None,
        traceback: TracebackType | None = None,
    ) -> Literal[False]: ...


@runtime_backed_function()
def monotonic_now_ns() -> int:
    """Current monotonic time in nanoseconds, on the engine's media clock.

    `CLOCK_MONOTONIC` on Linux; `mach_absolute_time` on macOS, which is
    `time.CLOCK_UPTIME_RAW` and stops while the machine sleeps.
    """
    ...


@runtime_backed_function(native_callable_name="MonotonicTimer")
def start_monotonic_timer(interval_ns: int) -> MonotonicTimer:
    """Start a drift-free timer ticking every `interval_ns`, first at `now + interval_ns`."""
    ...
