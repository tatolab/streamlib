# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The clock and logging surfaces, exercised without an engine.

`monotonic_now_ns` and `MonotonicTimer` are pure kernel-facing calls and
`log.*` degrades to a no-op sink before an engine boots, so none of this
needs a GPU.
"""

import time

import pytest

import streamlib
from streamlib import MonotonicTimer, clock, log, monotonic_now_ns

# Small enough to keep the suite fast, large enough that scheduler jitter
# cannot swallow a whole interval.
TIMER_TEST_INTERVAL_NS = 20_000_000
# Bounded like every wait in this suite: a timer that never ticks must fail,
# not hang.
TIMER_TICK_TIMEOUT_MS = 2_000


def test_monotonic_now_ns_returns_a_positive_int():
    value = monotonic_now_ns()
    assert isinstance(value, int)
    assert value > 0


def test_monotonic_now_ns_is_non_decreasing_across_calls():
    samples = [monotonic_now_ns() for _ in range(1000)]
    for previous, current in zip(samples, samples[1:]):
        assert current >= previous, f"clock went backwards: {current} < {previous}"


def test_monotonic_now_ns_reads_the_kernel_monotonic_clock():
    """Two streamlib reads bracket a `time` module read of the same clock.

    Pins the canonical-source contract: the value is
    `clock_gettime(CLOCK_MONOTONIC)`, the same syscall Rust's `Instant::now()`
    makes, so stamps are comparable across processes on this kernel.
    """
    first_streamlib_read = monotonic_now_ns()
    kernel_read = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
    second_streamlib_read = monotonic_now_ns()
    assert first_streamlib_read <= kernel_read <= second_streamlib_read


def test_the_clock_module_re_exports_the_native_surface():
    """Old-SDK parity: `from streamlib import clock` keeps working."""
    assert clock.monotonic_now_ns is streamlib.monotonic_now_ns
    assert clock.MonotonicTimer is streamlib.MonotonicTimer


def test_a_timer_ticks_at_roughly_its_interval():
    with MonotonicTimer(TIMER_TEST_INTERVAL_NS) as timer:
        before_first_tick = monotonic_now_ns()
        expirations = timer.wait(timeout_ms=TIMER_TICK_TIMEOUT_MS)
        after_first_tick = monotonic_now_ns()
    assert expirations >= 1, "the timer never ticked within the bounded wait"
    # The first absolute deadline is `now + interval`; a tick before it would
    # mean the timer is not the drift-free absolute-time shape it claims.
    elapsed = after_first_tick - before_first_tick
    assert elapsed >= TIMER_TEST_INTERVAL_NS // 2, (
        f"a tick arrived after only {elapsed}ns for a {TIMER_TEST_INTERVAL_NS}ns interval"
    )


def test_a_wait_that_times_out_returns_zero():
    one_hour_ns = 3_600_000_000_000
    with MonotonicTimer(one_hour_ns) as timer:
        assert timer.wait(timeout_ms=10) == 0


def test_waiting_on_a_closed_timer_returns_minus_one():
    timer = MonotonicTimer(TIMER_TEST_INTERVAL_NS)
    timer.close()
    assert timer.wait(timeout_ms=10) == -1


def test_the_context_manager_closes_the_timer():
    with MonotonicTimer(TIMER_TEST_INTERVAL_NS) as timer:
        assert timer.interval_ns == TIMER_TEST_INTERVAL_NS
    assert timer.wait(timeout_ms=10) == -1


@pytest.mark.parametrize("invalid_interval_ns", [0, -1])
def test_a_non_positive_interval_is_refused(invalid_interval_ns):
    with pytest.raises(ValueError, match="interval_ns must be > 0"):
        MonotonicTimer(invalid_interval_ns)


def test_every_log_level_accepts_structured_attrs():
    """The old SDK's `log.info("msg", key=value)` shape, engine or no engine."""
    log.trace("trace record", detail="fine")
    log.debug("debug record", frame_number=7)
    log.info("info record", width=1920, height=1080)
    log.warn("warn record", dropped=3)
    log.error("error record", error="synthetic")


def test_warn_is_the_primary_name_and_warning_its_alias():
    assert log.warning is log.warn


def test_log_functions_accept_a_bare_message():
    log.info("no attrs at all")
