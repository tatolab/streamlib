# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The clock and logging surfaces, exercised without an engine.

`monotonic_now_ns` and `MonotonicTimer` are pure kernel-facing calls and
`log.*` degrades to a no-op sink before an engine boots, so none of this
needs a GPU.
"""

import pytest

import streamlib
from engine_media_clock import engine_media_clock_now_ns
from streamlib import MonotonicTimer, _engine, clock, log, monotonic_now_ns

# Small, so the one wiring test below returns at once.
TIMER_TEST_INTERVAL_NS = 1_000_000
# Bounded like every wait in this suite: a timer that never ticks must fail,
# not hang. Only a broken wiring reaches it.
TIMER_TICK_TIMEOUT_MS = 2_000
# A deadline no test run reaches, for the tests that must see no tick.
AN_HOUR_NS = 3_600_000_000_000


def test_monotonic_now_ns_returns_a_positive_int():
    value = monotonic_now_ns()
    assert isinstance(value, int)
    assert value > 0


def test_monotonic_now_ns_is_non_decreasing_across_calls():
    samples = [monotonic_now_ns() for _ in range(1000)]
    for previous, current in zip(samples, samples[1:]):
        assert current >= previous, f"clock went backwards: {current} < {previous}"


def test_monotonic_now_ns_reads_the_engine_media_clock():
    """Two streamlib reads bracket a `time` module read of the engine's clock.

    Pins the canonical-source contract: the value is the clock every engine
    stamp is taken on — `CLOCK_MONOTONIC` on Linux, `mach_absolute_time` on
    macOS — so a helper's stamps are comparable with the engine's.
    """
    first_streamlib_read = monotonic_now_ns()
    kernel_read = engine_media_clock_now_ns()
    second_streamlib_read = monotonic_now_ns()
    # `mach_absolute_time` ticks are coarser than a nanosecond, and the kernel
    # and the engine round a tick to nanoseconds separately.
    tick_rounding_slack_ns = 1_000
    assert (
        first_streamlib_read - tick_rounding_slack_ns
        <= kernel_read
        <= second_streamlib_read + tick_rounding_slack_ns
    )


def test_the_clock_module_re_exports_the_native_surface():
    """Old-SDK parity: `from streamlib import clock` keeps working."""
    assert clock.monotonic_now_ns is streamlib.monotonic_now_ns
    assert clock.MonotonicTimer is streamlib.MonotonicTimer


def test_python_exports_exactly_one_name_for_the_monotonic_clock():
    """Every language exports one clock name; Python's is `monotonic_now_ns`.

    A second name for the same number reads as a second epoch, which is the
    confusion the one-clock rule exists to kill. Checked on the native module
    and on both re-export surfaces above it, because a re-export is not the
    only way a second name can reappear.

    Matched by suffix rather than named outright: `ship-change-removed-gate.sh`
    content-greps `sdk/` for each `REMOVED:` pattern a change file declares, so
    spelling the deleted export here would hold `one-monotonic-clock` red for
    good.
    """
    for exporting_module in (_engine, streamlib, clock):
        assert {
            name for name in dir(exporting_module) if name.endswith("_now_ns")
        } == {"monotonic_now_ns"}, (
            f"{exporting_module.__name__} exports more than one monotonic-clock name"
        )


def test_a_timer_delivers_its_tick_through_the_wait():
    """The wiring from the kernel timer to `wait()`, and nothing about latency.

    How late a tick lands is the scheduler's business, and the deadline
    arithmetic is pinned without a clock by the wheel's Rust tests.
    """
    with MonotonicTimer(TIMER_TEST_INTERVAL_NS) as timer:
        assert timer.wait(timeout_ms=TIMER_TICK_TIMEOUT_MS) >= 1


def test_a_poll_before_the_first_deadline_returns_zero():
    with MonotonicTimer(AN_HOUR_NS) as timer:
        assert timer.wait(timeout_ms=0) == 0


def test_waiting_on_a_closed_timer_returns_minus_one():
    timer = MonotonicTimer(AN_HOUR_NS)
    timer.close()
    assert timer.wait(timeout_ms=0) == -1


def test_the_context_manager_closes_the_timer():
    with MonotonicTimer(AN_HOUR_NS) as timer:
        assert timer.interval_ns == AN_HOUR_NS
    assert timer.wait(timeout_ms=0) == -1


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
