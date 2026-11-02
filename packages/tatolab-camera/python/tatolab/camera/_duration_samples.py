# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Durations collected over one reporting window, summarised as percentiles in milliseconds."""

from __future__ import annotations


class DurationSamplesInOneReportingWindow:
    """Nanosecond durations of one named measurement, cleared after each report."""

    def __init__(self) -> None:
        self._durations_ns: list[int] = []

    def add(self, duration_ns: int) -> None:
        self._durations_ns.append(duration_ns)

    def summary_in_milliseconds(self) -> str:
        """`p50/p95/p99/max` in milliseconds, then empty for the next window."""
        if not self._durations_ns:
            return "no samples"
        ordered = sorted(self._durations_ns)
        self._durations_ns.clear()

        def at_percentile(percentile: float) -> float:
            index = min(len(ordered) - 1, round(percentile / 100 * (len(ordered) - 1)))
            return ordered[index] / 1_000_000

        return (
            f"p50={at_percentile(50):.2f} p95={at_percentile(95):.2f} "
            f"p99={at_percentile(99):.2f} max={ordered[-1] / 1_000_000:.2f}"
        )
