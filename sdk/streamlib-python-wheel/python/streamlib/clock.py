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
reading of an unrelated clock and subtracting the two means nothing. A bag
that crossed the runtime mesh carries its producer's stamp unchanged; ask
`ctx.inputs.inbound_link_stamp_clock_identity(port, link)` which machine a
link's stamps were taken on before comparing them with another link's, and
[`this_machines_stamp_clock_identity`] which machine the readings taken here
are on before comparing a link's stamps against one of those.

Wall-clock APIs (`time.time`, `datetime.now`, `time.time_ns`) are NOT
comparable across processes — they drift under NTP and reflect different
epochs. Use them only when human-readable wall-clock time is genuinely
required (e.g. ISO8601 log formatting).

[`MonotonicTimer`] is a drift-free periodic timer on that same clock — a
`timerfd` on Linux, a kqueue timer on macOS. The first deadline is
`now + interval` and every one after it is absolute, so ticks never accumulate
drift. Use it as a context manager; `wait(timeout_ms=...)` bounds teardown
latency.
"""

from __future__ import annotations

from ._engine import MonotonicTimer as MonotonicTimer
from ._engine import monotonic_now_ns as monotonic_now_ns
from ._engine import (
    this_machines_stamp_clock_identity as this_machines_stamp_clock_identity,
)

__all__ = [
    "MonotonicTimer",
    "monotonic_now_ns",
    "this_machines_stamp_clock_identity",
]
