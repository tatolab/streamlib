# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The soundtrack's beat and energy curves, analysed once per interpreter.

Kick onsets come from the low band's spectral flux; energy is smoothed RMS.
`SoundtrackPulse` and `SoundtrackBeat` both read these curves, so the compute
nodes and the finish pulse on the same kick.
"""

from __future__ import annotations

import functools
import wave
from pathlib import Path

import numpy

SOUNDTRACK_PATH = Path(__file__).resolve().parent.parent / "media" / "soundtrack.wav"

ANALYSIS_FRAMES_PER_SECOND = 120
WINDOW_SAMPLES = 2048
BEAT_DECAY_SECONDS = 0.22

def read_mono_samples(path: Path) -> tuple[numpy.ndarray, int]:
    with wave.open(str(path), "rb") as track:
        rate = track.getframerate()
        channels = track.getnchannels()
        raw = track.readframes(track.getnframes())
    samples = numpy.frombuffer(raw, dtype="<i2").astype(numpy.float32) / 32768.0
    return samples.reshape(-1, channels).mean(axis=1), rate


def pulse_curves(samples: numpy.ndarray, rate: int) -> tuple[numpy.ndarray, numpy.ndarray]:
    """The beat and energy curves, sampled ANALYSIS_FRAMES_PER_SECOND times a second."""
    hop = rate // ANALYSIS_FRAMES_PER_SECOND
    padded = numpy.pad(samples, (WINDOW_SAMPLES // 2, WINDOW_SAMPLES))
    frame_count = len(samples) // hop
    window = numpy.hanning(WINDOW_SAMPLES).astype(numpy.float32)
    frequencies = numpy.fft.rfftfreq(WINDOW_SAMPLES, 1.0 / rate)
    low_band = (frequencies >= 35.0) & (frequencies <= 160.0)
    low = numpy.empty(frame_count, numpy.float32)
    loudness = numpy.empty(frame_count, numpy.float32)
    for index in range(frame_count):
        chunk = padded[index * hop : index * hop + WINDOW_SAMPLES] * window
        spectrum = numpy.abs(numpy.fft.rfft(chunk))
        low[index] = numpy.log1p(spectrum[low_band].sum())
        loudness[index] = float(numpy.sqrt(numpy.mean(chunk * chunk)))
    flux = numpy.maximum(numpy.diff(low, prepend=low[0]), 0.0)
    threshold = numpy.convolve(flux, numpy.ones(60) / 60.0, mode="same") * 1.6 + 1e-3
    onsets = flux > threshold
    beat = numpy.zeros(frame_count, numpy.float32)
    level = 0.0
    decay = numpy.exp(-1.0 / (BEAT_DECAY_SECONDS * ANALYSIS_FRAMES_PER_SECOND))
    last_onset = -999
    for index in range(frame_count):
        level *= decay
        if onsets[index] and index - last_onset > ANALYSIS_FRAMES_PER_SECOND // 8:
            level = max(level, min(1.0, flux[index] / (threshold[index] * 2.0)))
            last_onset = index
        beat[index] = level
    energy = numpy.convolve(loudness, numpy.ones(24) / 24.0, mode="same")
    energy = energy / max(float(energy.max()), 1e-6)
    return beat, energy




class SoundtrackCurves:
    """The analysed curves, read by time."""

    def __init__(self, beat: numpy.ndarray, energy: numpy.ndarray) -> None:
        self.beat = beat
        self.energy = energy

    def at(self, seconds: float) -> tuple[float, float]:
        index = min(max(int(seconds * ANALYSIS_FRAMES_PER_SECOND), 0), len(self.beat) - 1)
        return float(self.beat[index]), float(self.energy[index])


@functools.lru_cache(maxsize=4)
def soundtrack_curves(path: str = str(SOUNDTRACK_PATH)) -> SoundtrackCurves:
    track = Path(path)
    if not track.exists():
        return SoundtrackCurves(numpy.zeros(1, numpy.float32), numpy.zeros(1, numpy.float32))
    samples, rate = read_mono_samples(track)
    return SoundtrackCurves(*pulse_curves(samples, rate))
