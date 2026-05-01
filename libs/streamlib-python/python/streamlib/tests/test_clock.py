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
