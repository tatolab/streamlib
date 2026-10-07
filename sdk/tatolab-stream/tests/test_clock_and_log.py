# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The clock and logging surfaces as `tatolab.stream` declares them.

What the runtime behind them does — the clock it reads, the timer's ticks, the
records it writes — is tested against the runtime.
"""

import pytest

import tatolab.stream
from tatolab.stream import MonotonicTimer, clock, log

AN_HOUR_NS = 3_600_000_000_000


def test_the_clock_module_and_tatolab_stream_export_the_same_clock():
    """Old-SDK parity: `from tatolab.stream import clock` keeps working."""
    assert clock.monotonic_now_ns is tatolab.stream.monotonic_now_ns
    assert clock.MonotonicTimer is tatolab.stream.MonotonicTimer
    assert clock.start_monotonic_timer is tatolab.stream.start_monotonic_timer


def test_a_timer_is_never_constructed_from_its_protocol():
    """`start_monotonic_timer` is how a node starts one."""
    with pytest.raises(TypeError, match="Protocols cannot be instantiated"):
        MonotonicTimer(AN_HOUR_NS)  # pyright: ignore[reportAbstractUsage, reportCallIssue]


def test_tatolab_stream_exports_exactly_one_name_for_the_monotonic_clock():
    """Every language exports one clock name; Python's is `monotonic_now_ns`.

    A second name for the same number reads as a second epoch, which is the
    confusion the one-clock rule exists to kill. Checked on both re-export
    surfaces, because a re-export is not the only way a second name can
    reappear; the runtime's own module is checked against the runtime.

    Matched by suffix rather than named outright: `ship-change-removed-gate.sh`
    content-greps `sdk/` for each `REMOVED:` pattern a change file declares, so
    spelling the deleted export here would hold `one-monotonic-clock` red for
    good.
    """
    for exporting_module in (tatolab.stream, clock):
        assert {
            name for name in dir(exporting_module) if name.endswith("_now_ns")
        } == {"monotonic_now_ns"}, (
            f"{exporting_module.__name__} exports more than one monotonic-clock name"
        )


def test_warn_is_the_primary_name_and_warning_its_alias():
    assert log.warning is log.warn
