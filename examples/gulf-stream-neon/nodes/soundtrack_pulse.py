# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The soundtrack, heard: its beat and energy at stream time, as one bundle.

Importable as `nodes.soundtrack_pulse:SoundtrackPulse`. It reads the track's
WAV once its path is set, finds its kick onsets from the low band's spectral flux,
and each step publishes how hard the beat is hitting and how loud the track is
at that instant. Everything downstream that pulses, pulses on this.
"""

from __future__ import annotations

from pathlib import Path
from typing import Annotated, Callable

from tatolab.stream import (
    Description,
    Input,
    OutputPin,
    RuntimeContextLimitedAccess,
    computed,
    log,
    node,
)

from nodes.neon_world import SoundtrackPulseReading, TakeClock
from nodes.soundtrack_analysis import SoundtrackCurves, soundtrack_curves


def analysed_soundtrack(soundtrack_path: str) -> SoundtrackCurves:
    """The kick envelope and loudness of the track at `soundtrack_path`, relative to the
    project."""
    path = Path(soundtrack_path)
    if not path.is_absolute():
        path = Path(__file__).resolve().parent.parent / path
    curves = soundtrack_curves(str(path))
    log.info("soundtrack analysed", path=str(path), frames=len(curves.beat))
    return curves


@node.meta(
    category="Gulf Stream Neon",
    description="The soundtrack heard: its kick envelope and loudness at stream time, which every compute node pulses on.",
)
@node(execution="continuous", interval_ms=16)
def SoundtrackPulse(
    *,
    soundtrack_path: Input[
        Annotated[str, Description("The track, relative to the project.")]
    ] = Input("media/soundtrack.wav"),
    pulse: OutputPin[Annotated[SoundtrackPulseReading, Description("beat 0-1, energy 0-1, glow")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Publishes the soundtrack's kick envelope and loudness, step by step."""

    take_clock = TakeClock()
    curves = computed(lambda: analysed_soundtrack(soundtrack_path.get()))

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        beat, energy = curves.get().at(take_clock.seconds_at(ctx))
        pulse.write(
            SoundtrackPulseReading(
                beat=round(float(beat), 4),
                energy=round(float(energy), 4),
                glow=round(1.0 + 0.5 * float(energy), 4),
            ),
            timestamp_ns=ctx.time,
        )

    return each_frame
