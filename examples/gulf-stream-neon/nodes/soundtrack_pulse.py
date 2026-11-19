# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The soundtrack, heard: its beat and energy at stream time, as records.

Importable as `nodes.soundtrack_pulse:SoundtrackPulse`. It reads the track's
WAV once in `setup()`, finds its kick onsets from the low band's spectral flux,
and each step publishes how hard the beat is hitting and how loud the track is
at that instant. Everything downstream that pulses, pulses on this.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from tatolab.stream import Output, Records, RuntimeContextFullAccess, RuntimeContextLimitedAccess, log, node

from nodes.neon_world import TakeClock
from nodes.soundtrack_analysis import soundtrack_curves

PULSE_RECORD_NAMES = ["beat", "energy", "glow"]


@dataclass
class SoundtrackPulseConfig:
    """SoundtrackPulse's settings: the track, relative to the project."""

    soundtrack_path: str = "media/soundtrack.wav"


@node.process(execution="continuous", interval_ms=33)
class SoundtrackPulse:
    """Publishes the soundtrack's kick envelope and loudness, step by step."""

    def __init__(self, config: SoundtrackPulseConfig) -> None:
        self.config = config

    pulse = Output(Records, names=PULSE_RECORD_NAMES, description="beat 0-1, energy 0-1, glow")

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        self.take_clock = TakeClock()
        path = Path(self.config.soundtrack_path)
        if not path.is_absolute():
            path = Path(__file__).resolve().parent.parent / path
        self.curves = soundtrack_curves(str(path))
        log.info("soundtrack analysed", path=str(path), frames=len(self.curves.beat))

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        beat, energy = self.curves.at(self.take_clock.seconds_at(ctx))
        ctx.outputs.write(
            self.pulse,
            {"beat": round(beat, 4), "energy": round(energy, 4), "glow": round(1.0 + 0.5 * energy, 4)},
            timestamp_ns=ctx.time,
        )
