# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Tests for the canonical monotonic-clock timestamp source."""

import ctypes
import os
import time
from pathlib import Path

import pytest

import streamlib


def _locate_python_native_lib():
    """Find `libstreamlib_python_native.so` for direct ctypes loading.

    Mirrors the resolution other polyglot tests use (env var override,
    else workspace `target/{debug,release}` walk).
    """
    env = os.environ.get("STREAMLIB_PYTHON_NATIVE_LIB")
    if env and Path(env).exists():
        return Path(env)
    here = Path(__file__).resolve()
    # tests/test_clock.py → streamlib/ → python/ → streamlib-python/ → libs/ → workspace.
    workspace = here.parents[5]
    for profile in ("debug", "release"):
        candidate = workspace / "target" / profile / "libstreamlib_python_native.so"
        if candidate.exists():
            return candidate
    return None


def test_monotonic_now_ns_returns_int():
    """Public API returns a Python int (not float, not bytes)."""
    value = streamlib.monotonic_now_ns()
    assert isinstance(value, int)
    assert value > 0


def test_monotonic_non_decreasing_across_calls():
    """N consecutive calls produce a non-decreasing sequence."""
    samples = [streamlib.monotonic_now_ns() for _ in range(1000)]
    for prev, curr in zip(samples, samples[1:]):
        assert curr >= prev, f"clock went backwards: {curr} < {prev}"


def test_monotonic_advances_under_load():
    """Clock advances by at least a few microseconds across a busy loop."""
    start = streamlib.monotonic_now_ns()
    # Burn a small amount of CPU; deliberately not time.sleep to avoid
    # hiding a stuck clock behind a sleeping kernel.
    accumulator = 0
    for i in range(100_000):
        accumulator += i
    end = streamlib.monotonic_now_ns()
    assert end - start > 1_000, f"clock barely moved: {end - start} ns"


def test_monotonic_matches_clock_gettime_within_jitter():
    """Public API agrees with `clock_gettime(CLOCK_MONOTONIC)` modulo wake-up jitter.

    Pins the canonical-source contract: `streamlib.monotonic_now_ns()`
    is identical to the syscall both the cdylib and Rust's `Instant::now()`
    make.
    """
    a = streamlib.monotonic_now_ns()
    b = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    c = streamlib.monotonic_now_ns()
    # The two streamlib reads bracket the time-module read; the
    # time-module read must fall within them (modulo monotonicity).
    assert a <= b <= c, f"out of order: a={a}, b={b}, c={c}"


@pytest.fixture(scope="module")
def cdylib():
    """Lazily load `libstreamlib_python_native.so` for direct FFI tests.

    Skips the test cleanly when the cdylib hasn't been built (CI runs
    that build only the Python wheel, not the workspace).
    """
    path = _locate_python_native_lib()
    if path is None:
        pytest.skip(
            "libstreamlib_python_native.so not built — "
            "run `cargo build -p streamlib-python-native` first",
        )
    lib = ctypes.CDLL(str(path))
    lib.slpn_monotonic_now_ns.argtypes = []
    lib.slpn_monotonic_now_ns.restype = ctypes.c_uint64
    return lib


def test_cdylib_slpn_monotonic_now_ns_matches_clock_gettime(cdylib):
    """The cdylib export must read CLOCK_MONOTONIC, not wall-clock.

    Pins the Rust body so a regression to `SystemTime::now()` (the
    pre-#545 behavior) breaks this test. Brackets a `clock_gettime`
    read with two cdylib reads — the cdylib values must surround
    the direct syscall value, which can only happen if the cdylib
    is reading the same kernel CLOCK_MONOTONIC source.
    """
    a = cdylib.slpn_monotonic_now_ns()
    b = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    c = cdylib.slpn_monotonic_now_ns()
    assert a <= b <= c, (
        f"cdylib reads outside CLOCK_MONOTONIC: a={a}, gettime={b}, c={c}. "
        "If a > b > c by years, the cdylib likely reverted to wall-clock."
    )


def test_cdylib_slpn_monotonic_now_ns_is_non_decreasing(cdylib):
    """N consecutive cdylib calls produce a non-decreasing sequence."""
    samples = [cdylib.slpn_monotonic_now_ns() for _ in range(1000)]
    for prev, curr in zip(samples, samples[1:]):
        assert curr >= prev, f"cdylib clock went backwards: {curr} < {prev}"


def test_sub_microsecond_resolution():
    """Two consecutive reads can differ by less than 1µs (sub-µs resolution)."""
    # Take many pairs and check at least one pair has sub-µs delta. On
    # modern Linux clock_gettime resolves to ~10-100ns so this should
    # happen within a handful of iterations; we sample widely to make
    # the test robust against scheduling jitter.
    found_sub_us = False
    for _ in range(1000):
        a = streamlib.monotonic_now_ns()
        b = streamlib.monotonic_now_ns()
        if 0 < (b - a) < 1_000:
            found_sub_us = True
            break
    assert found_sub_us, "expected at least one sub-microsecond delta"


@pytest.fixture(scope="module")
def installed_clock(cdylib):
    """Wire MonotonicTimer's cdylib binding for the test session."""
    streamlib.clock.install_timerfd(cdylib)
    yield cdylib


def test_monotonic_timer_fires_on_schedule(installed_clock):
    """16ms timer delivers ~10 ticks within bounded slack."""
    interval_ns = 16 * 1_000_000
    timer = streamlib.MonotonicTimer(interval_ns)
    try:
        start_ns = streamlib.monotonic_now_ns()
        ticks = 0
        waits = 0
        while ticks < 10 and waits < 60:
            got = timer.wait(50)
            waits += 1
            if got > 0:
                ticks += got
        elapsed_ns = streamlib.monotonic_now_ns() - start_ns
        expected_ns = interval_ns * ticks
        assert ticks >= 10, f"expected at least 10 ticks, got {ticks}"
        slack_ns = 5_000_000 * ticks
        assert expected_ns - slack_ns <= elapsed_ns <= expected_ns + slack_ns, (
            f"elapsed {elapsed_ns}ns outside expected {expected_ns}±{slack_ns}"
        )
    finally:
        timer.close()


def test_monotonic_timer_wait_returns_zero_on_timeout(installed_clock):
    """1-second interval + 50ms wait timeouts before first tick."""
    timer = streamlib.MonotonicTimer(1_000_000_000)
    try:
        got = timer.wait(50)
        assert got == 0, f"expected timeout (0), got {got}"
    finally:
        timer.close()


def test_monotonic_timer_rejects_non_positive_interval(installed_clock):
    """Construction validates intervals."""
    with pytest.raises(ValueError, match="interval_ns must be > 0"):
        streamlib.MonotonicTimer(0)
    with pytest.raises(ValueError, match="interval_ns must be > 0"):
        streamlib.MonotonicTimer(-100)


def test_monotonic_timer_context_manager_closes(installed_clock):
    """`with` block calls close() at exit; subsequent wait() returns -1."""
    with streamlib.MonotonicTimer(16 * 1_000_000) as timer:
        first = timer.wait(50)
        assert first >= 0  # tick or timeout
    # After __exit__, the handle is null; wait() returns -1 (error sentinel).
    assert timer.wait(10) == -1
