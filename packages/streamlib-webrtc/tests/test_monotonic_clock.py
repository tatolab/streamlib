# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stamp this wheel takes is on the engine's clock.

The wheel links no engine crate, so it reads the platform clock itself, and
nothing but this comparison proves it reads the same one. On Linux both read
`CLOCK_MONOTONIC` and the check is a formality; on a Mac that has slept, a
reading on the wrong clock lands off by every second of that sleep.
"""

import streamlib

from streamlib_webrtc import _native


def test_a_stamp_this_wheel_takes_lands_between_two_engine_readings():
    """Bracketed rather than compared within a tolerance: two readings on one
    clock, taken in order, can only land in order, however long the runner was
    preempted between them."""
    for _ in range(1_000):
        engine_reading_before = streamlib.monotonic_now_ns()
        this_wheels_reading = _native.monotonic_now_ns()
        engine_reading_after = streamlib.monotonic_now_ns()

        assert engine_reading_before <= this_wheels_reading <= engine_reading_after, (
            f"this wheel read {this_wheels_reading} ns, outside the engine's "
            f"[{engine_reading_before}, {engine_reading_after}] — it is not on "
            "the engine's clock"
        )
