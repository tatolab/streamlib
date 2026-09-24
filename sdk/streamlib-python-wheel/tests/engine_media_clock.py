# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The engine's media clock, read through the `time` module rather than streamlib.

A bracket built on this cannot pass by agreeing with the clock it checks.
"""

import sys
import time

#: `mach_absolute_time` in nanoseconds on macOS; `CLOCK_MONOTONIC` on Linux.
ENGINE_MEDIA_CLOCK_ID = time.CLOCK_UPTIME_RAW if sys.platform == "darwin" else time.CLOCK_MONOTONIC


def engine_media_clock_now_ns() -> int:
    """The engine's media clock, read straight from the kernel."""
    return time.clock_gettime_ns(ENGINE_MEDIA_CLOCK_ID)
