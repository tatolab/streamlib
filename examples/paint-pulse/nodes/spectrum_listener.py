# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`SpectrumListener`: hears the music and turns it into motion for the paint ball.

Every window it splits the spectrum into 24 log-spaced bands, follows each band's level
against its own slow peak, and spots onsets with spectral flux over an adaptive threshold.
Each band drives its own scatter clock, from 0 (the ball whole) to 3 (fully scattered): an
onset throws it outward at once and it falls back within a beat, so hits read as explosions.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Annotated

import numpy
from tatolab.stream import (
    Audio,
    AudioWindowContract,
    Description,
    InputPin,
    NumberRange,
    OutputPin,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    node,
)

from nodes.motion import BAND_COUNT, MusicMotion

LOWEST_HZ = 40.0
HIGHEST_HZ = 14_000.0
CLOCK_MAX = 3.0
# How quickly a hit's outward throw dies away.
PUNCH_SECONDS = 0.22

LISTENING_WINDOW = AudioWindowContract(sample_rate=48_000, channels=1, dtype="f32", window_size=1024)
FFT_SIZE = 2048
WINDOW_SECONDS = LISTENING_WINDOW.window_size / LISTENING_WINDOW.sample_rate

# The kick drum's range: its onsets are the beat that pumps the whole ball.
BEAT_LOW_HZ = 35.0
BEAT_HIGH_HZ = 130.0

# Decibels mapped onto 0..1 for a band's level.
FLOOR_DB = -72.0
CEILING_DB = -12.0


def smoothstep(edge0: float, edge1: float, x: float) -> float:
    t = min(1.0, max(0.0, (x - edge0) / (edge1 - edge0)))
    return t * t * (3.0 - 2.0 * t)


class OnsetDetector:
    """A jump in spectral flux well above its recent average, at most one per 160 ms."""

    def __init__(self) -> None:
        self.average = 0.0
        self.deviation = 0.01
        self.since_last = 1.0

    def hear(self, flux: float, dt: float) -> float:
        self.since_last += dt
        threshold = self.average + max(0.012, 2.0 * self.deviation)
        strength = 0.0
        if flux > threshold and self.since_last > 0.16:
            self.since_last = 0.0
            strength = min(1.0, 0.55 + 0.45 * (flux - threshold) / (threshold + 0.02))
        follow = min(1.0, dt * 2.5)
        self.average += (flux - self.average) * follow
        self.deviation += (abs(flux - self.average) - self.deviation) * follow
        return strength


@dataclass
class BandState:
    level: float = 0.0
    envelope: float = 0.0
    peak: float = 0.15
    clock: float = 0.0
    punch: float = 0.0


@node.meta(
    category="Paint Pulse",
    description="The one analysis step the runtime has no built-in for: an FFT into 24 bands, each with a scatter clock that onsets throw outward.",
)
@node
class SpectrumListener:
    """Music in, one `MusicMotion` per window out."""

    sensitivity: Annotated[
        float, NumberRange(minimum=0.1), Description("How strongly the music moves the ball.")
    ] = 0.5
    spread: Annotated[
        float, NumberRange(minimum=0.0), Description("How far a hit scatters the ball.")
    ] = 0.65
    music: InputPin[
        Annotated[
            Audio,
            StreamDelivery("ordered"),
            LISTENING_WINDOW,
            Description("The music to listen to, mixed to mono"),
        ]
    ]
    motion: OutputPin[
        Annotated[
            MusicMotion,
            Description("Per-band clocks and levels, the beat, the energy, and the bass and highs levels"),
        ]
    ]

    def __init__(self) -> None:
        edges = LOWEST_HZ * (HIGHEST_HZ / LOWEST_HZ) ** (numpy.arange(BAND_COUNT + 1) / BAND_COUNT)
        bin_hz = LISTENING_WINDOW.sample_rate / FFT_SIZE
        self.band_bins = []
        for low, high in zip(edges[:-1], edges[1:]):
            first = max(1, int(round(low / bin_hz)))
            last = max(first + 1, int(round(high / bin_hz)))
            self.band_bins.append((first, last))
        self.beat_bins = (max(1, int(BEAT_LOW_HZ / bin_hz)), int(BEAT_HIGH_HZ / bin_hz) + 1)
        self.hann = numpy.hanning(FFT_SIZE).astype(numpy.float32)
        self.history = numpy.zeros(FFT_SIZE, dtype=numpy.float32)
        self.previous_spectrum = numpy.zeros(FFT_SIZE // 2 + 1, dtype=numpy.float32)
        self.bands = [BandState() for _ in range(BAND_COUNT)]
        self.band_onsets = [OnsetDetector() for _ in range(BAND_COUNT)]
        self.beat_onset = OnsetDetector()
        self.beat = 0.0
        self.energy = 0.0
        self.loudness = 0.0

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while True:
            block = self.music.read()
            if block is None:
                return
            self.listen_to_one_window(block)

    def listen_to_one_window(self, block: Audio) -> None:
        samples = numpy.from_dlpack(block)[0]
        hop = samples.shape[0]
        self.history = numpy.roll(self.history, -hop)
        self.history[-hop:] = samples
        spectrum_db = 20.0 * numpy.log10(
            numpy.abs(numpy.fft.rfft(self.history * self.hann)) / (FFT_SIZE / 4) + 1e-9
        )
        spectrum = numpy.clip((spectrum_db - FLOOR_DB) / (CEILING_DB - FLOOR_DB), 0.0, 1.0).astype(numpy.float32)
        rise = numpy.maximum(spectrum - self.previous_spectrum, 0.0)
        self.previous_spectrum = spectrum

        dt = WINDOW_SECONDS
        self.loudness += (float(numpy.sqrt(numpy.mean(samples * samples))) * 6.0 - self.loudness) * min(1.0, dt * 2.0)
        section = smoothstep(0.05, 0.6, self.loudness)

        shown_sum = 0.0
        raw_clocks = []
        for band, onset, (first, last) in zip(self.bands, self.band_onsets, self.band_bins):
            band.level = float(numpy.mean(spectrum[first:last]))
            attack_or_release = 40.0 if band.level > band.envelope else 5.0
            band.envelope += (band.level - band.envelope) * min(1.0, dt * attack_or_release)
            band.peak = max(band.envelope, band.peak - dt * 0.02, 0.12)
            shown = (band.envelope / band.peak) * smoothstep(0.03, 0.12, band.envelope)
            shown_sum += shown

            kick = onset.hear(float(numpy.mean(rise[first:last])), dt)
            # The ball stays whole between hits: a hit throws its band outward at once, and the
            # throw dies away within a beat, so every onset reads as its own explosion.
            drive = (0.15 * section + 0.55 * shown * shown) * (0.4 + 0.6 * section) * self.sensitivity
            if kick > 0.0:
                loud = smoothstep(0.08, 0.5, drive)
                band.punch = max(band.punch, min(1.0, (0.45 + 0.55 * kick) * loud * min(1.6, self.sensitivity)))
            band.punch *= numpy.exp(-dt / PUNCH_SECONDS)
            target = min(CLOCK_MAX, CLOCK_MAX * self.spread * (0.8 * band.punch + 0.08 * smoothstep(0.2, 1.0, drive)))
            rate = 24.0 if target > band.clock else 5.0
            band.clock += max(-rate * dt, min(rate * dt, target - band.clock))
            band.clock = min(CLOCK_MAX, max(0.0, band.clock))
            raw_clocks.append(band.clock)

        beat_kick = self.beat_onset.hear(float(numpy.mean(rise[self.beat_bins[0]:self.beat_bins[1]])), dt)
        self.beat = max(beat_kick, self.beat - dt * 3.5)
        self.energy += (shown_sum / BAND_COUNT - self.energy) * min(1.0, dt * 6.0)

        # Neighbours pull on each other, so the spectrum opens as a wave rather than in blocks.
        clocks = [
            0.25 * raw_clocks[max(0, i - 1)] + 0.5 * raw_clocks[i] + 0.25 * raw_clocks[min(BAND_COUNT - 1, i + 1)]
            for i in range(BAND_COUNT)
        ]
        levels = [
            (band.envelope / band.peak) * smoothstep(0.03, 0.12, band.envelope) for band in self.bands
        ]
        self.motion.write(
            MusicMotion(
                clocks=[round(float(c), 4) for c in clocks],
                levels=[round(float(min(1.0, lv)), 4) for lv in levels],
                beat=round(float(self.beat), 4),
                energy=round(float(self.energy), 4),
                bass=round(float(sum(levels[:6])) / 6.0, 4),
                highs=round(float(sum(levels[16:])) / 8.0, 4),
            ),
            timestamp_ns=block.timestamp_ns,
        )


