# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The animation nodes this project adds, each called as the plain function it is."""

from __future__ import annotations

import pytest

from core_nodes.animation import (
    Envelope,
    EnvelopeLevel,
    Pulse,
    PulsePhase,
    Ramp,
    RampPhase,
    Speed,
    SpeedPosition,
)

ONE_SECOND_NS = 1_000_000_000


def test_ramp_climbs_from_zero_to_one_and_starts_over():
    value, phase = Ramp(0.25, now=10 * ONE_SECOND_NS, previous=RampPhase())
    assert value == 0.0
    value, phase = Ramp(0.25, now=12 * ONE_SECOND_NS, previous=phase)
    assert value == pytest.approx(0.5)
    value, phase = Ramp(0.25, now=15 * ONE_SECOND_NS, previous=phase)
    assert value == pytest.approx(0.25)


def test_pulse_is_on_for_the_first_width_of_each_cycle():
    on, phase = Pulse(1.0, 0.25, now=0, previous=PulsePhase())
    assert on is True
    on, phase = Pulse(1.0, 0.25, now=ONE_SECOND_NS // 2, previous=phase)
    assert on is False
    on, phase = Pulse(1.0, 0.25, now=ONE_SECOND_NS + ONE_SECOND_NS // 10, previous=phase)
    assert on is True


def test_speed_sums_its_rate_over_stream_time_and_reset_sends_it_back():
    position, state = Speed(2.0, now=5 * ONE_SECOND_NS, previous=SpeedPosition())
    assert position == 0.0
    position, state = Speed(2.0, now=8 * ONE_SECOND_NS, previous=state)
    assert position == pytest.approx(6.0)
    position, state = Speed(-1.0, now=9 * ONE_SECOND_NS, previous=state)
    assert position == pytest.approx(5.0)
    position, state = Speed(2.0, reset=True, now=10 * ONE_SECOND_NS, previous=state)
    assert position == 0.0


def test_envelope_rises_while_its_gate_is_on_and_falls_when_it_lets_go():
    level, state = Envelope(True, 2.0, 4.0, now=0, previous=EnvelopeLevel())
    assert level == 0.0
    level, state = Envelope(True, 2.0, 4.0, now=ONE_SECOND_NS, previous=state)
    assert level == pytest.approx(0.5)
    level, state = Envelope(True, 2.0, 4.0, now=5 * ONE_SECOND_NS, previous=state)
    assert level == 1.0
    level, state = Envelope(False, 2.0, 4.0, now=6 * ONE_SECOND_NS, previous=state)
    assert level == pytest.approx(0.75)
    level, state = Envelope(False, 2.0, 0.0, now=7 * ONE_SECOND_NS, previous=state)
    assert level == 0.0
