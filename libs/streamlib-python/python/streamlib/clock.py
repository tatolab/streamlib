# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Canonical monotonic-clock timestamp source for the Python SDK.

Use `streamlib.monotonic_now_ns()` for any timestamp that needs to be
compared across processes — frame stamps, escalate request IDs, log
correlation tokens, anything that crosses the host/subprocess boundary.

The function calls `clock_gettime(CLOCK_MONOTONIC)` via Python's `time`
module — identical to the syscall `streamlib-python-native`'s
`slpn_monotonic_now_ns()` makes, and to the syscall Rust's
`std::time::Instant::now()` makes on Linux. Values from all three
sources share the same kernel epoch and are directly comparable.

Wall-clock APIs (`time.time`, `datetime.now`, `time.time_ns`) are NOT
comparable across processes — they drift under NTP and reflect
different epochs. Use them only when human-readable wall-clock time
is genuinely required (e.g. ISO8601 log formatting).
"""

from __future__ import annotations

import time


def monotonic_now_ns() -> int:
    """Current monotonic time in nanoseconds via `clock_gettime(CLOCK_MONOTONIC)`.

    Comparable across processes on the same kernel — to host Rust
    `Instant` reads and to the Deno SDK's `monotonicNowNs()`.
    """
    return time.clock_gettime_ns(time.CLOCK_MONOTONIC)
