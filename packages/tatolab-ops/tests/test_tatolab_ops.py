# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Each node of `tatolab.ops` called as the plain function it is."""

from __future__ import annotations

import pytest

from tatolab.ops import Clamp, Feedback, Lfo, LfoPhase, Remap


def test_lfo_starts_at_zero_and_advances_by_the_stream_time_between_reads():
    first, phase = Lfo(1.0, now=5_000_000_000, previous=LfoPhase())
    assert first == 0.0 and phase == LfoPhase(0.0, 5_000_000_000, True)
    quarter, phase = Lfo(1.0, now=5_250_000_000, previous=phase)
    assert quarter == pytest.approx(1.0)
    assert phase.cycles == pytest.approx(0.25)


def test_remap_maps_one_range_onto_another():
    assert Remap(0.0) == 0.5
    assert Remap(5.0, from_low=0.0, from_high=10.0, to_low=100.0, to_high=200.0) == 150.0
    assert Remap(3.0, from_low=1.0, from_high=1.0, to_low=7.0) == 7.0


def test_clamp_holds_a_value_between_its_bounds():
    assert Clamp(-1.0) == 0.0
    assert Clamp(0.25) == 0.25
    assert Clamp(4.0, low=1.0, high=2.0) == 2.0


def test_feedback_called_as_a_function_passes_its_value_on():
    assert Feedback() == 0.0
    assert Feedback(0.75) == 0.75
