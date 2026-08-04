# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Canonical monotonic-clock timestamp source and drift-free periodic timer.

Use [`monotonic_now_ns`] for any timestamp that needs to be compared across
processes — frame stamps, log correlation tokens, anything that crosses a
process boundary. It reads `clock_gettime(CLOCK_MONOTONIC)`, the same kernel
syscall Rust's `Instant::now()` and Python's
`time.clock_gettime_ns(time.CLOCK_MONOTONIC)` make, so values from all of them
share the kernel's monotonic epoch and are directly comparable.

Wall-clock APIs (`time.time`, `datetime.now`, `time.time_ns`) are NOT
comparable across processes — they drift under NTP and reflect different
epochs. Use them only when human-readable wall-clock time is genuinely
required (e.g. ISO8601 log formatting).

[`MonotonicTimer`] is a drift-free periodic timer backed by
`timerfd_create(CLOCK_MONOTONIC)`: the first absolute deadline is
`now + interval`, then `TFD_TIMER_ABSTIME` repeats, so ticks never accumulate
drift. Use it as a context manager; `wait(timeout_ms=...)` bounds teardown
latency.
"""

from __future__ import annotations

from ._engine import MonotonicTimer as MonotonicTimer
from ._engine import monotonic_now_ns as monotonic_now_ns

__all__ = ["MonotonicTimer", "monotonic_now_ns"]
