# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Tests for the canonical monotonic-clock timestamp source."""

import time

import streamlib


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
