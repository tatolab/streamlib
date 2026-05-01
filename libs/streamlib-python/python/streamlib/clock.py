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

import ctypes
import time
from typing import Optional


def monotonic_now_ns() -> int:
    """Current monotonic time in nanoseconds via `clock_gettime(CLOCK_MONOTONIC)`.

    Comparable across processes on the same kernel — to host Rust
    `Instant` reads and to the Deno SDK's `monotonicNowNs()`.
    """
    return time.clock_gettime_ns(time.CLOCK_MONOTONIC)


_TIMERFD_LIB: Optional[ctypes.CDLL] = None


def _bind_timerfd_lib(lib: ctypes.CDLL) -> ctypes.CDLL:
    """Configure `argtypes`/`restype` for the timerfd FFI symbols on `lib`.

    Idempotent — bind again on a fresh library handle and the per-handle
    state is overwritten cleanly. `subprocess_runner` calls this once at
    startup with the loaded `libstreamlib_python_native` handle.
    """
    lib.slpn_timerfd_create.argtypes = [ctypes.c_uint64]
    lib.slpn_timerfd_create.restype = ctypes.c_void_p
    lib.slpn_timerfd_wait.argtypes = [ctypes.c_void_p, ctypes.c_int32]
    lib.slpn_timerfd_wait.restype = ctypes.c_int64
    lib.slpn_timerfd_close.argtypes = [ctypes.c_void_p]
    lib.slpn_timerfd_close.restype = None
    return lib


def install_timerfd(lib: ctypes.CDLL) -> None:
    """Wire the cdylib for [`MonotonicTimer`]. Called by `subprocess_runner`."""
    global _TIMERFD_LIB
    _TIMERFD_LIB = _bind_timerfd_lib(lib)


class MonotonicTimer:
    """Drift-free periodic timer backed by `timerfd_create(CLOCK_MONOTONIC)`.

    Mirrors the Rust `LinuxTimerFdAudioClock` — first absolute deadline
    is `now + interval`, then `TFD_TIMER_ABSTIME` repeats to avoid
    cumulative drift. Use as a context manager; teardown latency is
    bounded by the `timeout_ms` argument to `wait()`.

    Linux only. Constructing on other platforms raises `RuntimeError`.
    """

    __slots__ = ("_handle", "_interval_ns")

    def __init__(self, interval_ns: int):
        if interval_ns <= 0:
            raise ValueError(f"interval_ns must be > 0, got {interval_ns}")
        if _TIMERFD_LIB is None:
            raise RuntimeError(
                "MonotonicTimer requires the streamlib native lib — call "
                "clock.install_timerfd(lib) first (subprocess_runner does "
                "this automatically)"
            )
        handle = _TIMERFD_LIB.slpn_timerfd_create(ctypes.c_uint64(interval_ns))
        if not handle:
            raise RuntimeError(
                f"slpn_timerfd_create({interval_ns}) failed — timerfd is "
                "Linux-only and requires a kernel that supports "
                "CLOCK_MONOTONIC + TFD_TIMER_ABSTIME"
            )
        self._handle = ctypes.c_void_p(handle)
        self._interval_ns = interval_ns

    @property
    def interval_ns(self) -> int:
        return self._interval_ns

    def wait(self, timeout_ms: int = 100) -> int:
        """Wait up to `timeout_ms` for the next tick.

        Returns: positive expiration count if a tick fired, 0 on
        timeout (no tick yet — caller should poll shutdown / stdin
        and try again), -1 on error. The default 100ms timeout
        bounds teardown latency without spinning.
        """
        if self._handle is None or not self._handle.value:
            return -1
        assert _TIMERFD_LIB is not None  # established by __init__
        return int(_TIMERFD_LIB.slpn_timerfd_wait(self._handle, ctypes.c_int32(timeout_ms)))

    def close(self) -> None:
        # `_handle` may be missing if `__init__` raised before assignment
        # (e.g. invalid interval, missing cdylib binding); the slot-based
        # `__slots__` declaration leaves the attribute genuinely absent
        # rather than `None`, so `getattr` is the right shape here.
        handle = getattr(self, "_handle", None)
        if handle is not None and handle.value:
            assert _TIMERFD_LIB is not None
            _TIMERFD_LIB.slpn_timerfd_close(handle)
            self._handle = ctypes.c_void_p(0)

    def __enter__(self) -> "MonotonicTimer":
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()

    def __del__(self) -> None:
        self.close()
